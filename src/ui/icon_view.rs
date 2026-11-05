//! Icon view (View > as Icons): a pane's folder as a grid of large icons,
//! with NSCollectionView. As with the column view, the list stays
//! underneath (hidden) and follows the selection, so every command, the
//! status line, Quick Look, and the preview pane work as in the list.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;
use std::ptr::NonNull;

use objc2::rc::{Retained, Weak};
use objc2::runtime::ProtocolObject;
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send};
use objc2_app_kit::{
    NSBox, NSBoxType, NSCollectionView, NSCollectionViewDataSource, NSCollectionViewDelegate,
    NSCollectionViewDropOperation, NSCollectionViewFlowLayout, NSCollectionViewItem, NSColor, NSDragOperation,
    NSDraggingInfo, NSDraggingSession, NSEvent, NSFont, NSImageView, NSIndexPathNSCollectionViewAdditions,
    NSLineBreakMode, NSPasteboardWriting, NSResponder, NSTextAlignment, NSTextField, NSView, NSViewController,
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

    fn highlight(&self) -> Option<Retained<NSBox>> {
        if !self.isViewLoaded() {
            return None;
        }
        self.view().subviews().firstObject().and_then(|v| v.downcast::<NSBox>().ok())
    }

    fn show(&self, entry: &Entry, local: bool) {
        let (icon, _) = metrics();
        let picture = icon_of(entry, local);
        let copy: Retained<objc2_app_kit::NSImage> = unsafe { msg_send![&picture, copy] };
        copy.setSize(NSSize::new(icon, icon));
        let alpha = if entry.hidden { 0.5 } else { 1.0 };
        if let Some(image) = self.imageView() {
            image.setImage(Some(&copy));
            image.setAlphaValue(alpha);
        }
        if let Some(label) = self.textField() {
            label.setStringValue(&ns(&entry.name));
            label.setToolTip(Some(&ns(&entry.name)));
            label.setAlphaValue(alpha);
        }
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
                icon.show(&entry, icons.ivars().local.get());
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

        #[unsafe(method(collectionView:validateDrop:proposedIndexPath:dropOperation:))]
        fn validate_drop(
            &self,
            _view: &NSCollectionView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            path: *mut *mut NSIndexPath,
            operation: NonNull<NSCollectionViewDropOperation>,
        ) -> NSDragOperation {
            let icons = self.view();
            let under = icons.as_ref().and_then(|i| i.folder_under(info));
            // Over a folder the drop goes onto it (accept looks again): the
            // proposed drop between items is left as it is.
            let _ = (path, operation);
            let target = match under {
                Some((_, folder)) => Some(folder),
                None => icons.as_ref().and_then(|i| i.pane()).map(|p| p.location()),
            };
            match (icons.and_then(|i| i.pane()), target) {
                (Some(pane), Some(target)) => pane.drop_operation(info, &target),
                _ => NSDragOperation::None,
            }
        }

        #[unsafe(method(collectionView:acceptDrop:indexPath:dropOperation:))]
        fn accept_drop(
            &self,
            _view: &NSCollectionView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            path: &NSIndexPath,
            operation: NSCollectionViewDropOperation,
        ) -> bool {
            let _ = (path, operation);
            let icons = self.view();
            let target = match icons.as_ref().and_then(|i| i.folder_under(info)) {
                Some((_, folder)) => Some(folder),
                None => icons.as_ref().and_then(|i| i.pane()).map(|p| p.location()),
            };
            match (icons.and_then(|i| i.pane()), target) {
                (Some(pane), Some(target)) => pane.drop_into(info, target),
                _ => false,
            }
        }
    }

);

impl IconSource {
    fn view(&self) -> Option<Retained<IconView>> {
        self.ivars().get().and_then(Weak::load)
    }
}

impl IconView {
    pub fn new(mtm: MainThreadMarker, pane: &PaneController) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(IconIvars {
            pane: OnceCell::new(),
            source: OnceCell::new(),
            entries: RefCell::new(Vec::new()),
            local: Cell::new(true),
            syncing: Cell::new(false),
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
        this.registerForDraggedTypes(&NSArray::from_retained_slice(&super::pane_view::drag_types()));
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
        let (_, size) = metrics();
        let layout = NSCollectionViewFlowLayout::new(self.mtm());
        layout.setItemSize(size);
        layout.setMinimumInteritemSpacing(6.0);
        layout.setMinimumLineSpacing(6.0);
        layout.setSectionInset(NSEdgeInsets { top: 12.0, left: 12.0, bottom: 12.0, right: 12.0 });
        self.setCollectionViewLayout(Some(&layout));
        // After the layout: setting one forgets the registered item class.
        unsafe { self.registerClass_forItemWithIdentifier(Some(IconItem::class()), &ns(ITEM)) };
        let background = super::themed::colors().map_or_else(NSColor::controlBackgroundColor, |c| c.background);
        self.setBackgroundColors(Some(&NSArray::from_retained_slice(&[background])));
        self.reloadData();
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
    }

    /// Shows the pane's items and selection.
    pub fn sync(&self, entries: Vec<Entry>, local: bool, selected: &[PathBuf]) {
        self.ivars().syncing.set(true);
        let changed = {
            let current = self.ivars().entries.borrow();
            current.len() != entries.len() || current.iter().zip(&entries).any(|(a, b)| a != b)
        };
        if changed || self.ivars().local.get() != local {
            *self.ivars().entries.borrow_mut() = entries;
            self.ivars().local.set(local);
            self.reloadData();
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
            let scroll = objc2_app_kit::NSCollectionViewScrollPosition::NearestHorizontalEdge;
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

/// The icon view inside its scroll view, ready for the pane.
pub fn scroll_view(icons: &IconView, mtm: MainThreadMarker) -> Retained<objc2_app_kit::NSScrollView> {
    let scroll = objc2_app_kit::NSScrollView::new(mtm);
    scroll.setDocumentView(Some(icons));
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll
}
