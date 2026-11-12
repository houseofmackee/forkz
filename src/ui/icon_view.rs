//! Icon view (View > as Icons): a pane's folder as a grid of large icons,
//! with NSCollectionView. The gallery (View > as Gallery) uses one as its
//! strip: a single row of smaller icons that scrolls sideways. As with the column view, the list stays
//! underneath (hidden) and follows the selection, so every command, the
//! status line, Quick Look, and the preview pane work as in the list.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use objc2::rc::{Retained, Weak};
use objc2::runtime::ProtocolObject;
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBox, NSBoxType, NSCollectionView, NSCollectionViewDataSource, NSCollectionViewDelegate,
    NSCollectionViewFlowLayout, NSCollectionViewItem, NSColor, NSDragOperation, NSDraggingInfo, NSDraggingSession,
    NSEvent, NSFont, NSImageView, NSIndexPathNSCollectionViewAdditions, NSLineBreakMode, NSPasteboardWriting,
    NSResponder, NSTextAlignment, NSTextField, NSView, NSViewController,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSIndexPath, NSInteger, NSMutableSet, NSObjectProtocol, NSPoint, NSRect, NSSet, NSSize,
};
use objc2_quick_look_ui::QLPreviewPanel;

use super::pane_view::{PaneController, key_command};
use super::util::{icon_of, ns};
use crate::fs::Entry;

const ITEM: &str = "ForkzIcon";

/// The item size for a text size (icons grow with zoom).
fn metrics() -> (f64, NSSize) {
    let (text, _) = super::pane_view::look_metrics();
    let icon = (64.0 * text / 13.0).round();
    (icon, NSSize::new((icon * 1.65).round(), (icon + text * 2.6 + 16.0).round()))
}

/// The gallery strip's items: icons only (the name is under the preview).
fn strip_metrics() -> (f64, NSSize) {
    let (text, _) = super::pane_view::look_metrics();
    let icon = (48.0 * text / 13.0).round();
    (icon, NSSize::new(icon + 16.0, icon + 16.0))
}

/// The height the gallery strip needs (its items, insets, and scroller).
pub fn strip_height() -> f64 {
    strip_metrics().1.height + 2.0 * STRIP_INSET + 14.0
}

const STRIP_INSET: f64 = 8.0;

/// A pause this long starts a new type-to-select name (as in the list).
const TYPING_PAUSE: Duration = Duration::from_millis(1000);

/// The item type-to-select picks: the first whose name starts with what
/// was typed (ignoring case), or else the name that sorts right after it
/// (as Finder does).
pub fn type_select_index(names: &[&str], typed: &str) -> Option<usize> {
    let typed = typed.to_lowercase();
    if typed.is_empty() {
        return None;
    }
    let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    if let Some(index) = lower.iter().position(|n| n.starts_with(&typed)) {
        return Some(index);
    }
    lower
        .iter()
        .enumerate()
        .filter(|(_, n)| crate::fs::natural_cmp(n, &typed) == std::cmp::Ordering::Greater)
        .min_by(|(_, a), (_, b)| crate::fs::natural_cmp(a, b))
        .map(|(i, _)| i)
}

define_class!(
    /// An icon's view: clicks anywhere on it are its own (not its image
    /// view's, which as a control would keep them), so they reach the item
    /// and the collection view, which select and open.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzIconCellView"]
    pub struct IconCellView;

    impl IconCellView {
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            hit.map(|_| self.retain().into_super())
        }
    }
);

define_class!(
    /// One icon: the picture, the name below, and a rounded highlight
    /// behind them when selected. AppKit makes these itself (from the
    /// registered class), so they keep no Rust state: the highlight is the
    /// view's first subview.
    #[unsafe(super(NSCollectionViewItem, NSViewController, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzIconItem"]
    pub struct IconItem;

    impl IconItem {
        #[unsafe(method(loadView))]
        fn load_view(&self) {
            self.build();
        }

        /// A double-click opens the item (the click itself selects it).
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let _: () = unsafe { msg_send![super(self), mouseDown: event] };
            if event.clickCount() == 2
                && let Some(view) = self.collectionView()
                && let Some(icons) = view.downcast_ref::<IconView>()
                && let Some(pane) = icons.pane()
            {
                pane.open_selection();
            }
        }

        #[unsafe(method(setSelected:))]
        fn set_selected(&self, selected: bool) {
            let _: () = unsafe { msg_send![super(self), setSelected: selected] };
            self.update_highlight();
        }
    }
);

impl IconItem {
    fn build(&self) {
        let mtm = self.mtm();
        let (icon, size) = metrics();
        let (text, _) = super::pane_view::look_metrics();
        let cell = IconCellView::alloc(mtm).set_ivars(());
        let cell: Retained<IconCellView> =
            unsafe { msg_send![super(cell), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), size)] };
        let view: Retained<NSView> = cell.into_super();
        let highlight = NSBox::new(mtm);
        highlight.setBoxType(NSBoxType::Custom);
        highlight.setBorderWidth(0.0);
        highlight.setCornerRadius(8.0);
        highlight.setFrame(NSRect::new(NSPoint::new(2.0, 2.0), NSSize::new(size.width - 4.0, size.height - 4.0)));
        highlight.setHidden(true);
        let image = NSImageView::initWithFrame(
            NSImageView::alloc(mtm),
            NSRect::new(
                NSPoint::new(((size.width - icon) / 2.0).floor(), size.height - icon - 8.0),
                NSSize::new(icon, icon),
            ),
        );
        // An image view takes drags of image files for itself (and refuses
        // them when not editable), which would hide the folder underneath.
        image.unregisterDraggedTypes();
        let label = NSTextField::labelWithString(&ns(""), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize(text)));
        label.setAlignment(NSTextAlignment::Center);
        label.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        label.setMaximumNumberOfLines(2);
        if let Some(cell) = label.cell() {
            cell.setWraps(true);
        }
        if let Some(colors) = super::themed::colors() {
            label.setTextColor(Some(&colors.text));
        }
        label.setFrame(NSRect::new(NSPoint::new(4.0, 6.0), NSSize::new(size.width - 8.0, (text * 2.6).round())));
        view.addSubview(&highlight);
        view.addSubview(&image);
        view.addSubview(&label);
        self.setView(&view);
        self.setImageView(Some(&image));
        self.setTextField(Some(&label));
    }

    /// The highlight: shown when selected, in the accent color while the
    /// icon view has the focus (quieter otherwise, as in the list).
    fn update_highlight(&self) {
        let Some(highlight) = self.highlight() else { return };
        let selected = self.isSelected();
        highlight.setHidden(!selected);
        let focused = self.collectionView().is_some_and(|view| {
            view.window()
                .and_then(|w| w.firstResponder())
                .is_some_and(|r| std::ptr::eq(Retained::as_ptr(&r).cast::<u8>(), Retained::as_ptr(&view).cast::<u8>()))
        });
        let fill = match (super::themed::colors(), focused) {
            (Some(c), true) => c.selection,
            (Some(c), false) => c.selection.colorWithAlphaComponent(0.45),
            (None, true) => NSColor::selectedContentBackgroundColor(),
            (None, false) => NSColor::unemphasizedSelectedContentBackgroundColor(),
        };
        highlight.setFillColor(&fill);
        if let Some(label) = self.textField() {
            let text = match (super::themed::colors(), selected && focused) {
                (Some(c), true) => c.selection_text,
                (Some(c), false) => c.text,
                (None, true) => NSColor::alternateSelectedControlTextColor(),
                (None, false) => NSColor::labelColor(),
            };
            label.setTextColor(Some(&text));
        }
    }

    /// Shown while a drag would drop onto this folder.
    fn show_as_drop_target(&self) {
        let Some(highlight) = self.highlight() else { return };
        let accent = super::themed::colors().map_or_else(NSColor::controlAccentColor, |c| c.accent);
        highlight.setFillColor(&accent.colorWithAlphaComponent(0.35));
        highlight.setHidden(false);
    }

    fn highlight(&self) -> Option<Retained<NSBox>> {
        if !self.isViewLoaded() {
            return None;
        }
        self.view().subviews().firstObject().and_then(|v| v.downcast::<NSBox>().ok())
    }

    /// Shows a picture (an icon or a thumbnail) at the icon size, keeping
    /// its shape.
    fn set_picture(&self, picture: &objc2_app_kit::NSImage, strip: bool) {
        let (icon, _) = if strip { strip_metrics() } else { metrics() };
        let copy: Retained<objc2_app_kit::NSImage> = unsafe { msg_send![picture, copy] };
        copy.setSize(super::thumbnails::fitted(picture, icon));
        if let Some(image) = self.imageView() {
            image.setImage(Some(&copy));
        }
    }

    /// Places the picture, name, and highlight for the grid or the strip.
    fn lay_out(&self, strip: bool) {
        let (Some(image), Some(label), Some(highlight)) = (self.imageView(), self.textField(), self.highlight()) else {
            return;
        };
        let (icon, size) = if strip { strip_metrics() } else { metrics() };
        let (text, _) = super::pane_view::look_metrics();
        self.view().setFrameSize(size);
        highlight.setFrame(NSRect::new(NSPoint::new(2.0, 2.0), NSSize::new(size.width - 4.0, size.height - 4.0)));
        let top = if strip { ((size.height - icon) / 2.0).floor() } else { size.height - icon - 8.0 };
        image.setFrame(NSRect::new(NSPoint::new(((size.width - icon) / 2.0).floor(), top), NSSize::new(icon, icon)));
        label.setFrame(NSRect::new(NSPoint::new(4.0, 6.0), NSSize::new(size.width - 8.0, (text * 2.6).round())));
        label.setHidden(strip);
    }

    fn show(&self, entry: &Entry, local: bool, strip: bool) {
        self.lay_out(strip);
        // The contents' thumbnail when there is one (it may come later).
        let thumbnail = super::thumbnails::applies(entry, local).then(|| super::thumbnails::get(entry)).flatten();
        let picture = thumbnail.unwrap_or_else(|| icon_of(entry, local));
        self.set_picture(&picture, strip);
        let alpha = if entry.hidden { 0.5 } else { 1.0 };
        if let Some(image) = self.imageView() {
            image.setAlphaValue(alpha);
        }
        if let Some(label) = self.textField() {
            label.setStringValue(&ns(&entry.name));
            label.setToolTip(Some(&ns(&entry.name)));
            label.setAlphaValue(alpha);
        }
        // The strip has no names: the icon's tooltip gives it.
        self.view().setToolTip(strip.then(|| ns(&entry.name)).as_deref());
    }
}

pub struct IconIvars {
    pane: OnceCell<Weak<PaneController>>,
    /// Kept: the view holds its data source and delegate weakly.
    source: OnceCell<Retained<IconSource>>,
    entries: RefCell<Vec<Entry>>,
    local: Cell<bool>,
    /// Set while forkz changes the selection itself.
    syncing: Cell<bool>,
    /// The folder icon highlighted under a drag.
    drop_index: Cell<Option<NSInteger>>,
    /// The gallery's strip (one row, sideways) rather than the grid.
    strip: bool,
    /// Type-to-select: what was typed, and when last.
    typed: RefCell<(String, Option<Instant>)>,
}

define_class!(
    #[unsafe(super(NSCollectionView, NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzIconView"]
    #[ivars = IconIvars]
    pub struct IconView;

    impl IconView {
        /// The list's keys: Space, Return, Tab, F-keys (arrows move).
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if self.type_select(event) {
                return;
            }
            match (self.pane(), key_command(event)) {
                (Some(pane), Some(command)) => super::run_command(command, Some(pane.index())),
                _ => unsafe { msg_send![super(self), keyDown: event] },
            }
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let accepted: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if accepted && let Some(pane) = self.pane() {
                super::set_active(pane.index());
            }
            self.refresh_highlights();
            accepted
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first_responder(&self) -> bool {
            let accepted: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            // After the focus has moved.
            if let Some(pane) = self.pane() {
                let index = pane.index();
                dispatch2::DispatchQueue::main().exec_async(move || super::refresh_icon_highlights(index));
            }
            accepted
        }

        // Quick Look: the hidden list provides the items.
        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts_preview_panel_control(&self, _panel: Option<&QLPreviewPanel>) -> bool {
            true
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin_preview_panel_control(&self, panel: Option<&QLPreviewPanel>) {
            if let (Some(panel), Some(pane)) = (panel, self.pane()) {
                pane.hand_quick_look(panel, true);
            }
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn end_preview_panel_control(&self, panel: Option<&QLPreviewPanel>) {
            if let (Some(panel), Some(pane)) = (panel, self.pane()) {
                pane.hand_quick_look(panel, false);
            }
        }
    }

    unsafe impl NSObjectProtocol for IconView {}
);

define_class!(
    /// The icon view's data source and delegate (an object of its own: a
    /// collection view that is its own delegate forwards to itself forever).
    #[unsafe(super(objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzIconSource"]
    #[ivars = OnceCell<Weak<IconView>>]
    pub struct IconSource;

    unsafe impl NSObjectProtocol for IconSource {}

    unsafe impl NSCollectionViewDataSource for IconSource {
        #[unsafe(method(collectionView:numberOfItemsInSection:))]
        fn number_of_items(&self, _view: &NSCollectionView, _section: NSInteger) -> NSInteger {
            self.view().map_or(0, |v| v.ivars().entries.borrow().len() as NSInteger)
        }

        #[unsafe(method_id(collectionView:itemForRepresentedObjectAtIndexPath:))]
        fn item(&self, view: &NSCollectionView, path: &NSIndexPath) -> Retained<NSCollectionViewItem> {
            let item = view.makeItemWithIdentifier_forIndexPath(&ns(ITEM), path);
            if let Some(icons) = self.view()
                && let Some(icon) = item.downcast_ref::<IconItem>()
                && let Some(entry) = icons.entry_at(path.item())
            {
                icon.show(&entry, icons.ivars().local.get(), icons.ivars().strip);
            }
            item
        }
    }

    unsafe impl NSCollectionViewDelegate for IconSource {
        #[unsafe(method(collectionView:didSelectItemsAtIndexPaths:))]
        fn did_select(&self, _view: &NSCollectionView, _paths: &NSSet<NSIndexPath>) {
            if let Some(icons) = self.view() {
                icons.selection_changed();
            }
        }

        #[unsafe(method(collectionView:didDeselectItemsAtIndexPaths:))]
        fn did_deselect(&self, _view: &NSCollectionView, _paths: &NSSet<NSIndexPath>) {
            if let Some(icons) = self.view() {
                icons.selection_changed();
            }
        }

        #[unsafe(method(collectionView:canDragItemsAtIndexPaths:withEvent:))]
        fn can_drag(&self, _view: &NSCollectionView, _paths: &NSSet<NSIndexPath>, _event: &NSEvent) -> bool {
            true
        }

        /// Dragging out: a file URL (or a file promise from a server).
        #[unsafe(method_id(collectionView:pasteboardWriterForItemAtIndexPath:))]
        fn pasteboard_writer(
            &self,
            _view: &NSCollectionView,
            path: &NSIndexPath,
        ) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
            let icons = self.view();
            let entry = icons.as_ref().and_then(|i| i.entry_at(path.item()));
            match (entry, icons.and_then(|i| i.pane())) {
                (Some(entry), Some(pane)) => Some(pane.drag_writer(&entry)),
                _ => None,
            }
        }

        #[unsafe(method(collectionView:draggingSession:willBeginAtPoint:forItemsAtIndexPaths:))]
        fn dragging_will_begin(
            &self,
            _view: &NSCollectionView,
            _session: &NSDraggingSession,
            _point: NSPoint,
            paths: &NSSet<NSIndexPath>,
        ) {
            if let Some(icons) = self.view()
                && let Some(pane) = icons.pane()
            {
                let dragged: Vec<PathBuf> =
                    paths.iter().filter_map(|p| icons.entry_at(p.item())).map(|e| e.path).collect();
                pane.remember_drag(dragged);
            }
        }
    }

);

impl IconSource {
    fn view(&self) -> Option<Retained<IconView>> {
        self.ivars().get().and_then(Weak::load)
    }
}

define_class!(
    /// The icon view's scroll view, which takes drops (so the empty space
    /// below the icons counts too): onto the folder icon under the pointer,
    /// or into the pane's folder.
    #[unsafe(super(objc2_app_kit::NSScrollView, NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzIconScroll"]
    #[ivars = OnceCell<Weak<IconView>>]
    pub struct IconScroll;

    impl IconScroll {
        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            self.icons().map_or(NSDragOperation::None, |icons| icons.drag_moved(info))
        }

        #[unsafe(method(draggingUpdated:))]
        fn dragging_updated(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            self.icons().map_or(NSDragOperation::None, |icons| icons.drag_moved(info))
        }

        #[unsafe(method(draggingExited:))]
        fn dragging_exited(&self, _info: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
            if let Some(icons) = self.icons() {
                icons.show_drop_target(None);
            }
        }

        #[unsafe(method(prepareForDragOperation:))]
        fn prepare_for_drag(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            true
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            match self.icons() {
                Some(icons) => icons.drop_items(info),
                None => false,
            }
        }
    }
);

impl IconScroll {
    fn icons(&self) -> Option<Retained<IconView>> {
        self.ivars().get().and_then(Weak::load)
    }
}

impl IconView {
    /// The grid, or (`strip`) the gallery's strip.
    pub fn new(mtm: MainThreadMarker, pane: &PaneController, strip: bool) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(IconIvars {
            pane: OnceCell::new(),
            source: OnceCell::new(),
            entries: RefCell::new(Vec::new()),
            local: Cell::new(true),
            syncing: Cell::new(false),
            drop_index: Cell::new(None),
            strip,
            typed: RefCell::new((String::new(), None)),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        let _ = this.ivars().pane.set(Weak::from_retained(&pane.retain()));
        unsafe {
            this.registerClass_forItemWithIdentifier(Some(IconItem::class()), &ns(ITEM));
            let source = IconSource::alloc(mtm).set_ivars(OnceCell::new());
            let source: Retained<IconSource> = msg_send![super(source), init];
            let _ = source.ivars().set(Weak::from_retained(&this));
            this.setDataSource(Some(ProtocolObject::from_ref(&*source)));
            this.setDelegate(Some(ProtocolObject::from_ref(&*source)));
            let _ = this.ivars().source.set(source);
        }
        this.setSelectable(true);
        this.setAllowsMultipleSelection(true);
        for local in [false, true] {
            this.setDraggingSourceOperationMask_forLocal(
                NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic,
                local,
            );
        }
        this.apply_look();
        this
    }

    fn pane(&self) -> Option<Retained<PaneController>> {
        self.ivars().pane.get().and_then(Weak::load)
    }

    /// Typing a name selects the item it starts (letters typed in quick
    /// succession add up; a Space while typing is part of the name, not
    /// Quick Look). False for keys that are not typing.
    fn type_select(&self, event: &NSEvent) -> bool {
        use objc2_app_kit::NSEventModifierFlags as M;
        if event.modifierFlags().intersects(M::Command | M::Control | M::Option) {
            return false;
        }
        let Some(text) = event.characters().map(|c| c.to_string()) else { return false };
        // Letters and the like (not arrows, Return, Tab, or function keys).
        let typing =
            !text.is_empty() && text.chars().all(|c| !c.is_control() && !('\u{f700}'..='\u{f8ff}').contains(&c));
        if !typing {
            return false;
        }
        let now = Instant::now();
        let mut typed = self.ivars().typed.borrow_mut();
        let recent = typed.1.is_some_and(|last| now.duration_since(last) < TYPING_PAUSE);
        if text == " " && !recent {
            return false;
        }
        if !recent {
            typed.0.clear();
        }
        typed.0.push_str(&text);
        typed.1 = Some(now);
        let wanted = typed.0.clone();
        drop(typed);
        let index = {
            let entries = self.ivars().entries.borrow();
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            type_select_index(&names, &wanted)
        };
        if let Some(index) = index {
            let set: Retained<NSMutableSet<NSIndexPath>> = NSMutableSet::new();
            set.addObject(&NSIndexPath::indexPathForItem_inSection(index as NSInteger, 0));
            unsafe { self.deselectAll(None) };
            let scroll = if self.ivars().strip {
                objc2_app_kit::NSCollectionViewScrollPosition::CenteredHorizontally
            } else {
                objc2_app_kit::NSCollectionViewScrollPosition::NearestHorizontalEdge
            };
            self.selectItemsAtIndexPaths_scrollPosition(&set, scroll);
            // Selected by forkz, so the delegate is not told: the pane is.
            self.selection_changed();
            self.refresh_highlights();
        }
        true
    }

    /// A thumbnail is ready: the item showing that file gets it.
    pub fn thumbnail_ready(&self, path: &Path) {
        let Some((index, entry)) =
            self.ivars().entries.borrow().iter().enumerate().find(|(_, e)| e.path == path).map(|(i, e)| (i, e.clone()))
        else {
            return;
        };
        let Some(thumbnail) = super::thumbnails::get(&entry) else { return };
        if let Some(item) = self
            .itemAtIndexPath(&NSIndexPath::indexPathForItem_inSection(index as NSInteger, 0))
            .and_then(|item| item.downcast::<IconItem>().ok())
        {
            item.set_picture(&thumbnail, self.ivars().strip);
        }
    }

    pub fn refresh_highlights(&self) {
        for item in self.visibleItems().iter() {
            if let Some(icon) = item.downcast_ref::<IconItem>() {
                icon.update_highlight();
            }
        }
    }

    fn entry_at(&self, index: NSInteger) -> Option<Entry> {
        usize::try_from(index).ok().and_then(|i| self.ivars().entries.borrow().get(i).cloned())
    }

    /// The icon size (with the text size) and the theme's background.
    pub fn apply_look(&self) {
        let strip = self.ivars().strip;
        let (_, size) = if strip { strip_metrics() } else { metrics() };
        let layout = NSCollectionViewFlowLayout::new(self.mtm());
        layout.setItemSize(size);
        layout.setMinimumInteritemSpacing(6.0);
        layout.setMinimumLineSpacing(6.0);
        let inset = if strip { STRIP_INSET } else { 12.0 };
        layout.setSectionInset(NSEdgeInsets { top: inset, left: inset, bottom: inset, right: inset });
        if strip {
            layout.setScrollDirection(objc2_app_kit::NSCollectionViewScrollDirection::Horizontal);
        }
        self.setCollectionViewLayout(Some(&layout));
        // After the layout: setting one forgets the registered item class.
        unsafe { self.registerClass_forItemWithIdentifier(Some(IconItem::class()), &ns(ITEM)) };
        let background = super::themed::colors().map_or_else(NSColor::controlBackgroundColor, |c| c.background);
        self.setBackgroundColors(Some(&NSArray::from_retained_slice(&[background])));
        self.reloadData();
    }

    /// Items dropped: onto the folder icon under the pointer, or into the
    /// pane's folder.
    fn drop_items(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
        self.show_drop_target(None);
        match (self.pane(), self.drop_target(info)) {
            (Some(pane), Some(target)) => pane.drop_into(info, target),
            _ => false,
        }
    }

    /// Where a drop goes: the folder icon under the pointer, or the pane's
    /// folder.
    fn drop_target(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> Option<PathBuf> {
        match self.folder_under(info) {
            Some((_, folder)) => Some(folder),
            None => self.pane().map(|pane| pane.location()),
        }
    }

    /// The operation for a drag where it is now, with its folder shown.
    fn drag_moved(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
        let under = self.folder_under(info);
        let operation = match (self.pane(), self.drop_target(info)) {
            (Some(pane), Some(target)) => pane.drop_operation(info, &target),
            _ => NSDragOperation::None,
        };
        let highlight = under.map(|(index, _)| index).filter(|_| operation != NSDragOperation::None);
        self.show_drop_target(highlight);
        operation
    }

    /// Highlights the folder icon a drop would go to (or none).
    fn show_drop_target(&self, index: Option<NSInteger>) {
        let previous = self.ivars().drop_index.replace(index);
        if previous == index {
            return;
        }
        let item = |i: NSInteger| {
            self.itemAtIndexPath(&NSIndexPath::indexPathForItem_inSection(i, 0))
                .and_then(|item| item.downcast::<IconItem>().ok())
        };
        if let Some(old) = previous.and_then(item) {
            old.update_highlight();
        }
        if let Some(new) = index.and_then(item) {
            new.show_as_drop_target();
        }
    }

    /// The folder icon under a drag, if any (with its index).
    fn folder_under(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> Option<(NSInteger, PathBuf)> {
        let point = self.convertPoint_fromView(info.draggingLocation(), None);
        let index = self.indexPathForItemAtPoint(point)?.item();
        let entry = self.entry_at(index).filter(Entry::is_folder_like)?;
        Some((index, entry.path))
    }

    fn selection_changed(&self) {
        if self.ivars().syncing.get() {
            return;
        }
        let Some(pane) = self.pane() else { return };
        let mut chosen: Vec<(NSInteger, PathBuf)> = self
            .selectionIndexPaths()
            .iter()
            .filter_map(|p| self.entry_at(p.item()).map(|e| (p.item(), e.path)))
            .collect();
        chosen.sort_by_key(|(index, _)| *index);
        let location = pane.location();
        pane.browse_select(location, chosen.into_iter().map(|(_, p)| p).collect());
        if self.ivars().strip {
            pane.gallery_selection_changed();
        }
    }

    /// Shows the pane's items and selection.
    pub fn sync(&self, entries: Vec<Entry>, local: bool, selected: &[PathBuf]) {
        self.ivars().syncing.set(true);
        let (changed, same_folder) = {
            let current = self.ivars().entries.borrow();
            let folder = |list: &[Entry]| list.first().and_then(|e| e.path.parent().map(Path::to_path_buf));
            let changed = current.len() != entries.len() || current.iter().zip(&entries).any(|(a, b)| a != b);
            (changed, folder(&current) == folder(&entries))
        };
        if changed || self.ivars().local.get() != local {
            // A change in the same folder (after a drop, say) keeps the
            // scroll position; reloading would go back to the top.
            let origin = self.visibleRect().origin;
            *self.ivars().entries.borrow_mut() = entries;
            self.ivars().local.set(local);
            self.reloadData();
            if same_folder {
                if let Some(scroll) = self.enclosingScrollView() {
                    scroll.layoutSubtreeIfNeeded();
                }
                self.scrollPoint(origin);
            }
        }
        let set: Retained<NSMutableSet<NSIndexPath>> = NSMutableSet::new();
        let mut first = None;
        for (index, entry) in self.ivars().entries.borrow().iter().enumerate() {
            if selected.contains(&entry.path) {
                set.addObject(&NSIndexPath::indexPathForItem_inSection(index as NSInteger, 0));
                first.get_or_insert(index);
            }
        }
        unsafe { self.deselectAll(None) };
        if set.count() > 0 {
            let scroll = if self.ivars().strip {
                objc2_app_kit::NSCollectionViewScrollPosition::CenteredHorizontally
            } else {
                objc2_app_kit::NSCollectionViewScrollPosition::NearestHorizontalEdge
            };
            self.selectItemsAtIndexPaths_scrollPosition(&set, scroll);
        }
        self.ivars().syncing.set(false);
        // Once laid out, and with the focus where it ends up.
        if let Some(pane) = self.pane() {
            let index = pane.index();
            dispatch2::DispatchQueue::main().exec_async(move || super::refresh_icon_highlights(index));
        }
    }
}

/// The icon view inside its scroll view (which takes drops), ready for
/// the pane.
pub fn scroll_view(icons: &IconView, mtm: MainThreadMarker) -> Retained<objc2_app_kit::NSScrollView> {
    let scroll = IconScroll::alloc(mtm).set_ivars(OnceCell::new());
    let scroll: Retained<IconScroll> = unsafe { msg_send![super(scroll), initWithFrame: NSRect::ZERO] };
    let _ = scroll.ivars().set(Weak::from_retained(&icons.retain()));
    scroll.setDocumentView(Some(icons));
    if icons.ivars().strip {
        scroll.setHasHorizontalScroller(true);
    } else {
        scroll.setHasVerticalScroller(true);
    }
    scroll.setAutohidesScrollers(true);
    scroll.registerForDraggedTypes(&NSArray::from_retained_slice(&super::pane_view::drag_types()));
    scroll.into_super()
}

#[cfg(test)]
mod tests {
    use super::type_select_index;

    #[test]
    fn typing_picks_an_item() {
        let names = ["Subfolder", "Bundle.zip", "Calculator icon.png", "notes.txt", "Notes icon.png", "zeta"];
        assert_eq!(type_select_index(&names, "cal"), Some(2));
        assert_eq!(type_select_index(&names, "NOTES"), Some(3));
        assert_eq!(type_select_index(&names, "notes i"), Some(4));
        // Nothing starts with it: the name right after it.
        assert_eq!(type_select_index(&names, "d"), Some(4));
        assert_eq!(type_select_index(&names, "zz"), None);
        assert_eq!(type_select_index(&names, ""), None);
    }
}
