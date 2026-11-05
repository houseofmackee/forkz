//! Column view (View > as Columns): a pane's folders side by side, as in
//! Finder, with NSBrowser. Choosing items in a column makes that column's
//! folder the pane's folder and the chosen items its selection. The list
//! stays underneath (hidden) and follows along, so every command, the
//! status line, Quick Look, and the preview pane work as in the list.
//!
//! Folders on the Mac are listed as columns open; on servers, in the
//! background (the column fills in when the listing arrives).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use dispatch2::DispatchQueue;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBrowser, NSBrowserColumnResizingType, NSBrowserDelegate, NSBrowserDropOperation, NSControl, NSDragOperation,
    NSDraggingInfo, NSEvent, NSFont, NSPasteboard, NSResponder, NSView,
};
use objc2_foundation::{NSIndexSet, NSInteger, NSMutableIndexSet, NSObjectProtocol, NSSize, NSString};
use objc2_quick_look_ui::QLPreviewPanel;

use super::pane_view::{PaneController, key_command};
use super::util::{icon_of, ns};
use crate::fs::Entry;
use crate::vfs::FsRef;

pub struct BrowserIvars {
    pane: OnceCell<Weak<PaneController>>,
    fs: RefCell<Option<FsRef>>,
    /// The first column's folder.
    root: RefCell<PathBuf>,
    /// Folder listings in the order shown (hidden items left out unless
    /// they are shown).
    cache: RefCell<HashMap<PathBuf, Vec<Entry>>>,
    /// Folders being listed in the background (servers).
    loading: RefCell<HashSet<PathBuf>>,
    /// Set while forkz changes the selection itself.
    syncing: Cell<bool>,
    /// The columns were loaded while on screen (columns loaded in a window
    /// not shown yet keep empty labels).
    shown: Cell<bool>,
    /// The listings were forgotten: every column is loaded again.
    stale: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSBrowser, NSControl, NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzColumnBrowser"]
    #[ivars = BrowserIvars]
    pub struct ColumnBrowser;

    impl ColumnBrowser {
        /// The list's keys: Space, Return, Tab, F-keys (arrows browse).
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
            accepted
        }

        #[unsafe(method(columnSelectionChanged:))]
        fn column_selection_changed(&self, _sender: Option<&AnyObject>) {
            self.selection_changed();
        }

        #[unsafe(method(columnDoubleClicked:))]
        fn column_double_clicked(&self, _sender: Option<&AnyObject>) {
            if let Some(pane) = self.pane() {
                pane.open_selection();
            }
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

    unsafe impl NSObjectProtocol for ColumnBrowser {}

    unsafe impl NSBrowserDelegate for ColumnBrowser {
        #[unsafe(method_id(rootItemForBrowser:))]
        fn root_item(&self, _browser: &NSBrowser) -> Option<Retained<AnyObject>> {
            Some(item(&self.ivars().root.borrow()))
        }

        #[unsafe(method(browser:numberOfChildrenOfItem:))]
        fn number_of_children(&self, _browser: &NSBrowser, item: Option<&AnyObject>) -> NSInteger {
            item.map_or(0, |i| self.children(&path_of(i)).len() as NSInteger)
        }

        #[unsafe(method_id(browser:child:ofItem:))]
        fn child(&self, _browser: &NSBrowser, index: NSInteger, parent: Option<&AnyObject>) -> Retained<AnyObject> {
            let folder = parent.map(path_of).unwrap_or_default();
            let children = self.children(&folder);
            let path = usize::try_from(index).ok().and_then(|i| children.get(i)).map_or(folder, |e| e.path.clone());
            item(&path)
        }

        #[unsafe(method(browser:isLeafItem:))]
        fn is_leaf(&self, _browser: &NSBrowser, item: Option<&AnyObject>) -> bool {
            item.and_then(|i| self.entry(&path_of(i))).is_none_or(|e| !e.is_folder_like())
        }

        #[unsafe(method(browser:canDragRowsWithIndexes:inColumn:withEvent:))]
        fn can_drag(&self, _browser: &NSBrowser, _rows: &NSIndexSet, _column: NSInteger, _event: &NSEvent) -> bool {
            true
        }

        /// Dragging out: file URLs (or file promises from a server).
        #[unsafe(method(browser:writeRowsWithIndexes:inColumn:toPasteboard:))]
        fn write_rows(&self, _browser: &NSBrowser, rows: &NSIndexSet, column: NSInteger, pasteboard: &NSPasteboard) -> bool {
            let folder = self.parentForItemsInColumn(column).map(|i| path_of(&i)).unwrap_or_default();
            let listing = self.children(&folder);
            let entries: Vec<Entry> =
                super::pane_view::index_rows(rows).into_iter().filter_map(|r| listing.get(r).cloned()).collect();
            self.pane().is_some_and(|pane| pane.write_drag(&entries, pasteboard))
        }

        #[unsafe(method(browser:validateDrop:proposedRow:column:dropOperation:))]
        fn validate_drop(
            &self,
            _browser: &NSBrowser,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            row: NonNull<NSInteger>,
            column: NonNull<NSInteger>,
            operation: NonNull<NSBrowserDropOperation>,
        ) -> NSDragOperation {
            // SAFETY: the browser passes pointers to its proposed drop.
            let (row, column, operation) = unsafe { (&mut *row.as_ptr(), *column.as_ptr(), &mut *operation.as_ptr()) };
            match (self.pane(), self.drop_folder(row, column, operation)) {
                (Some(pane), Some(target)) => pane.drop_operation(info, &target),
                _ => NSDragOperation::None,
            }
        }

        #[unsafe(method(browser:acceptDrop:atRow:column:dropOperation:))]
        fn accept_drop(
            &self,
            _browser: &NSBrowser,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            row: NSInteger,
            column: NSInteger,
            operation: NSBrowserDropOperation,
        ) -> bool {
            let (mut row, mut operation) = (row, operation);
            match (self.pane(), self.drop_folder(&mut row, column, &mut operation)) {
                (Some(pane), Some(target)) => pane.drop_into(info, target),
                _ => false,
            }
        }

        #[unsafe(method_id(browser:objectValueForItem:))]
        fn object_value(&self, _browser: &NSBrowser, item: Option<&AnyObject>) -> Option<Retained<AnyObject>> {
            item.map(|item| self.label(&path_of(item)))
        }
    }
);

/// A browser item: the path, as a string.
fn item(path: &Path) -> Retained<AnyObject> {
    Retained::into_super(Retained::into_super(ns(&path.to_string_lossy())))
}

fn path_of(item: &AnyObject) -> PathBuf {
    item.downcast_ref::<NSString>().map(|s| PathBuf::from(s.to_string())).unwrap_or_default()
}

impl ColumnBrowser {
    pub fn new(mtm: MainThreadMarker, pane: &PaneController) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(BrowserIvars {
            pane: OnceCell::new(),
            fs: RefCell::new(None),
            root: RefCell::new(PathBuf::from("/")),
            cache: RefCell::new(HashMap::new()),
            loading: RefCell::new(HashSet::new()),
            syncing: Cell::new(false),
            shown: Cell::new(false),
            stale: Cell::new(false),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: objc2_foundation::NSRect::ZERO] };
        let _ = this.ivars().pane.set(Weak::from_retained(&pane.retain()));
        this.setTitled(false);
        this.setSeparatesColumns(true);
        this.setHasHorizontalScroller(true);
        this.setAutohidesScroller(true);
        this.setAllowsMultipleSelection(true);
        this.setSendsActionOnArrowKeys(true);
        this.setTakesTitleFromPreviousColumn(false);
        this.setColumnResizingType(NSBrowserColumnResizingType::UserColumnResizing);
        this.setMinColumnWidth(140.0);
        this.setDefaultColumnWidth(300.0);
        // Widths the user sets are kept (per pane).
        this.setColumnsAutosaveName(&ns(&format!("ForkzColumns{}", pane.index())));
        unsafe {
            this.setDelegate(Some(ProtocolObject::from_ref(&*this)));
            this.setTarget(Some(&this));
            this.setAction(Some(sel!(columnSelectionChanged:)));
            this.setDoubleAction(Some(sel!(columnDoubleClicked:)));
        }
        this.registerForDraggedTypes(&objc2_foundation::NSArray::from_retained_slice(&super::pane_view::drag_types()));
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

    /// The list's text size, row height, and theme background.
    pub fn apply_look(&self) {
        let (_, row) = super::pane_view::look_metrics();
        self.setRowHeight(row);
        let background =
            super::themed::colors().map_or_else(objc2_app_kit::NSColor::controlBackgroundColor, |c| c.background);
        self.setBackgroundColor(&background);
        self.loadColumnZero();
    }

    fn entry(&self, path: &Path) -> Option<Entry> {
        let parent = path.parent()?;
        self.ivars().cache.borrow().get(parent)?.iter().find(|e| e.path == path).cloned()
    }

    /// A folder's items: listed now on the Mac, in the background on a
    /// server (empty until then).
    fn children(&self, folder: &Path) -> Vec<Entry> {
        if let Some(list) = self.ivars().cache.borrow().get(folder) {
            return list.clone();
        }
        let Some(fs) = self.ivars().fs.borrow().clone() else { return Vec::new() };
        let Some(pane) = self.pane() else { return Vec::new() };
        let (show_hidden, sort) = (pane.show_hidden(), pane.sort());
        let (exclusions, _) = super::listing_exclusions();
        let prepare = move |mut list: Vec<Entry>| {
            list.retain(|e| show_hidden || !e.hidden);
            list.retain(|e| !exclusions.as_ref().is_some_and(|x| x.excludes(&e.name, e.is_folder_like())));
            crate::fs::sort_entries(&mut list, sort);
            list
        };
        if fs.is_local() {
            let list = prepare(fs.list(folder).unwrap_or_default());
            self.ivars().cache.borrow_mut().insert(folder.to_path_buf(), list.clone());
            return list;
        }
        if self.ivars().loading.borrow_mut().insert(folder.to_path_buf()) {
            let folder = folder.to_path_buf();
            let index = pane.index();
            std::thread::spawn(move || {
                let list = prepare(fs.list(&folder).unwrap_or_default());
                DispatchQueue::main().exec_async(move || super::column_listing_arrived(index, folder, list));
            });
        }
        Vec::new()
    }

    /// A server folder's listing arrived: its column fills in.
    pub fn listing_arrived(&self, folder: PathBuf, list: Vec<Entry>) {
        if !self.ivars().loading.borrow_mut().remove(&folder) {
            return;
        }
        self.ivars().cache.borrow_mut().insert(folder.clone(), list);
        if let Some(column) = self.column_of(&folder)
            && column <= self.lastColumn()
        {
            self.reloadColumn(column);
        }
    }

    /// The column showing a folder's items (the root's is 0).
    fn column_of(&self, folder: &Path) -> Option<NSInteger> {
        let root = self.ivars().root.borrow();
        let inside = folder.strip_prefix(&*root).ok()?;
        Some(inside.components().count() as NSInteger)
    }

    /// An item's label: its icon, then its name (the browser's text cells
    /// show images as attachments).
    fn label(&self, path: &Path) -> Retained<AnyObject> {
        use objc2_app_kit::{
            NSAttributedStringAttachmentConveniences, NSFontAttributeName, NSMutableParagraphStyle,
            NSParagraphStyleAttributeName, NSTextAttachment,
        };
        use objc2_foundation::{NSAttributedString, NSDictionary, NSMutableAttributedString};
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (text, _) = super::pane_view::look_metrics();
        let font = NSFont::systemFontOfSize(text);
        // One line, shortened in the middle when the column is narrow (a
        // wrapped second line would not show, leaving the row blank).
        let paragraph = NSMutableParagraphStyle::new();
        paragraph.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingMiddle);
        let attributes: Retained<NSDictionary<NSString, AnyObject>> = NSDictionary::from_retained_objects(
            &[unsafe { NSFontAttributeName }, unsafe { NSParagraphStyleAttributeName }],
            &[
                Retained::into_super(Retained::into_super(font.clone())),
                Retained::into_super(Retained::into_super(Retained::into_super(paragraph.clone()))),
            ],
        );
        let label = NSMutableAttributedString::new();
        if let Some(entry) = self.entry(path) {
            let local = self.ivars().fs.borrow().as_ref().is_some_and(|fs| fs.is_local());
            let side = (text + 3.0).round();
            let icon = icon_of(&entry, local);
            let copy: Retained<objc2_app_kit::NSImage> = unsafe { msg_send![&icon, copy] };
            copy.setSize(NSSize::new(side, side));
            let attachment = NSTextAttachment::new();
            attachment.setImage(Some(&copy));
            // Centered on the text.
            let offset = (font.capHeight() - side) / 2.0;
            attachment.setBounds(objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(0.0, offset.round()),
                NSSize::new(side, side),
            ));
            label.appendAttributedString(&NSAttributedString::attributedStringWithAttachment(&attachment));
            let gap = unsafe { NSAttributedString::new_with_attributes(&ns("  "), &attributes) };
            label.appendAttributedString(&gap);
        }
        let title = unsafe { NSAttributedString::new_with_attributes(&ns(&name), &attributes) };
        label.appendAttributedString(&title);
        // The style counts from a paragraph's first character: the icon.
        unsafe {
            label.addAttribute_value_range(
                NSParagraphStyleAttributeName,
                &paragraph,
                objc2_foundation::NSRange::new(0, label.length()),
            );
        }
        Retained::into_super(Retained::into_super(Retained::into_super(label)))
    }

    /// The user chose items (or clicked into a column): that column's
    /// folder becomes the pane's, the items its selection.
    fn selection_changed(&self) {
        if self.ivars().syncing.get() {
            return;
        }
        let Some(pane) = self.pane() else { return };
        let mut column = self.selectedColumn();
        if column < 0 {
            column = self.clickedColumn();
        }
        if column < 0 {
            return;
        }
        let Some(folder) = self.parentForItemsInColumn(column).map(|i| path_of(&i)) else { return };
        let rows = super::pane_view::index_rows(&self.selectedRowIndexesInColumn(column).unwrap_or_default());
        let paths: Vec<PathBuf> = rows
            .into_iter()
            .filter_map(|r| self.itemAtRow_inColumn(r as NSInteger, column))
            .map(|i| path_of(&i))
            .collect();
        pane.browse_select(folder, paths);
    }

    /// Shows the pane's folder and selection: the first call, and after
    /// the pane navigates or reloads. `entries` is the folder's listing.
    pub fn sync(&self, fs: &FsRef, location: &Path, entries: Option<Vec<Entry>>, selected: &[PathBuf]) {
        let same_fs = self.ivars().fs.borrow().as_ref().is_some_and(|f| crate::vfs::same(f, fs));
        let inside = location.starts_with(&*self.ivars().root.borrow());
        let visible = self.window().is_some_and(|w| w.isVisible()) && !self.isHiddenOrHasHiddenAncestor();
        let first_showing = visible && !self.ivars().shown.replace(visible);
        let reload_all = first_showing || self.ivars().stale.replace(false);
        self.ivars().syncing.set(true);
        if !same_fs || !inside {
            *self.ivars().fs.borrow_mut() = Some(fs.clone());
            *self.ivars().root.borrow_mut() = location.to_path_buf();
            self.ivars().cache.borrow_mut().clear();
            self.ivars().loading.borrow_mut().clear();
        }
        // The pane's own listing is the freshest.
        if let Some(entries) = entries {
            self.ivars().cache.borrow_mut().insert(location.to_path_buf(), entries);
        }
        if !same_fs || !inside || (reload_all && first_showing) {
            self.loadColumnZero();
        } else if reload_all {
            // Changes: every column again, each where it was scrolled.
            for column in 0..=self.lastColumn() {
                self.reloadColumn(column);
            }
        }
        let column = self.column_of(location).unwrap_or(0);
        if column <= self.lastColumn() {
            self.reloadColumn(column);
        }
        // Each folder on the way, chosen in its column (labels are icons
        // and names, so the browser's own path lookup cannot be used).
        let root = self.ivars().root.borrow().clone();
        let mut folder = root.clone();
        for (column, part) in location.strip_prefix(&root).unwrap_or(Path::new("")).components().enumerate() {
            let child = folder.join(part);
            let Some(row) = self.children(&folder).iter().position(|e| e.path == child) else { break };
            self.selectRow_inColumn(row as NSInteger, column as NSInteger);
            folder = child;
        }
        let listing = self.children(location);
        let rows: Vec<usize> = selected.iter().filter_map(|p| listing.iter().position(|e| e.path == *p)).collect();
        match rows.as_slice() {
            // One folder: its items show in the next column.
            [row] if listing[*row].is_folder_like() => self.selectRow_inColumn(*row as NSInteger, column),
            [] => {}
            rows => {
                let set = NSMutableIndexSet::new();
                for row in rows {
                    set.addIndex(*row);
                }
                self.selectRowIndexes_inColumn(&set, column);
            }
        }
        // The next column as far as it fits, but the folder's own first
        // (a narrow pane would otherwise show mostly the next column).
        self.scrollColumnToVisible(self.lastColumn());
        self.reveal_column(column);
        self.ivars().syncing.set(false);
    }

    /// Scrolls a column fully into view (scrollColumnToVisible leaves a
    /// partly visible column as it is).
    fn reveal_column(&self, column: NSInteger) {
        let scroll = self.subviews().iter().find_map(|v| v.downcast::<objc2_app_kit::NSScrollView>().ok());
        let Some(document) = scroll.and_then(|s| s.documentView()) else { return };
        let frame = document.convertRect_fromView(self.frameOfColumn(column), Some(self));
        document.scrollRectToVisible(frame);
    }

    /// Forgets listings (after changes, or the hidden-files setting or
    /// sort changed): every column is loaded again when next shown.
    pub fn forget_listings(&self) {
        self.ivars().cache.borrow_mut().clear();
        self.ivars().stale.set(true);
    }

    /// Where a drop at (row, column) goes: the folder dropped on, or the
    /// column's folder (with the drop retargeted to the whole column).
    fn drop_folder(
        &self,
        row: &mut NSInteger,
        column: NSInteger,
        operation: &mut NSBrowserDropOperation,
    ) -> Option<PathBuf> {
        let column = column.max(0);
        if *operation == NSBrowserDropOperation::On
            && *row >= 0
            && let Some(item) = self.itemAtRow_inColumn(*row, column)
        {
            let path = path_of(&item);
            if self.entry(&path).is_some_and(|e| e.is_folder_like()) {
                return Some(path);
            }
        }
        *row = -1;
        *operation = NSBrowserDropOperation::On;
        self.parentForItemsInColumn(column).map(|i| path_of(&i))
    }
}
