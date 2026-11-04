//! One file pane: a path bar, the file list (an NSTableView), and a status
//! line. `PaneController` owns the pane's state and is the table's data
//! source and delegate; `FileTable` adds the keyboard, Quick Look, and focus
//! handling.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSColor, NSControl, NSControlTextEditingDelegate, NSDragOperation, NSDraggingInfo, NSEvent, NSEventModifierFlags,
    NSImageView, NSLayoutConstraintOrientation, NSMenu, NSMenuDelegate, NSMenuItem, NSMenuItemValidation,
    NSPasteboardTypeFileURL, NSPasteboardWriting, NSPathControl, NSResponder, NSScrollView, NSSearchFieldDelegate,
    NSStackView, NSTableCellView, NSTableColumn, NSTableColumnResizingOptions, NSTableView,
    NSTableViewColumnAutoresizingStyle, NSTableViewDataSource, NSTableViewDelegate, NSTableViewDropOperation,
    NSTableViewStyle, NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTextView, NSUserInterfaceItemIdentification,
    NSUserInterfaceLayoutOrientation, NSView, NSWindowDelegate, NSWorkspace,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSIndexSet, NSInteger, NSMutableIndexSet, NSNotification, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSSortDescriptor, NSString,
};
use objc2_quick_look_ui::{QLPreviewItem, QLPreviewPanel, QLPreviewPanelDataSource, QLPreviewPanelDelegate};

use super::commands::Command;
use super::util::{file_url, format_date, icon_of, kind_of, mtm, ns, small_label, url_path};
use crate::fs::ops::JobKind;
use crate::fs::{self, Sort, SortKey};
use crate::pane::{PaneState, selection_summary};
use crate::vfs::{self, FsRef};
use dispatch2::DispatchQueue;

const ROW_HEIGHT: f64 = 22.0;

pub struct TableIvars {
    controller: OnceCell<Weak<PaneController>>,
}

define_class!(
    #[unsafe(super(NSTableView, NSControl, NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzFileTable"]
    #[ivars = TableIvars]
    pub struct FileTable;

    impl FileTable {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let Some(controller) = self.controller() else { return };
            let flags = event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
            let plain = (flags & !(NSEventModifierFlags::Function | NSEventModifierFlags::NumericPad)).is_empty();
            let command = match event.keyCode() {
                48 if plain => Some(Command::SwitchPane),                  // Tab
                36 | 76 if plain => Some(Command::Rename),                 // Return, Enter
                49 if plain => Some(Command::QuickLook),                   // Space
                125 if flags.contains(NSEventModifierFlags::Command) => Some(Command::Open), // Command-Down
                120 => Some(Command::Rename),                              // F2
                96 => Some(Command::CopyToOther),                          // F5
                97 => Some(Command::MoveToOther),                          // F6
                98 => Some(Command::NewFolder),                            // F7
                100 => Some(Command::Trash),                               // F8
                _ => None,
            };
            match command {
                Some(command) => super::run_command(command, Some(controller.index())),
                // Arrows, type-to-select, and the rest.
                None => unsafe { msg_send![super(self), keyDown: event] },
            }
        }

        /// A double-click on ".." goes up. (The table does not send its
        /// double action for that row, as it cannot be dragged.)
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if !self.double_clicked_parent(event) {
                unsafe { msg_send![super(self), mouseDown: event] }
            }
        }

        #[unsafe(method(becomeFirstResponder))]
        fn become_first_responder(&self) -> bool {
            let accepted: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            if accepted && let Some(controller) = self.controller() {
                super::set_active(controller.index());
            }
            accepted
        }

        /// Edit > Copy: the selected files, to paste in Finder or the other pane.
        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            if let Some(c) = self.controller() {
                super::run_command(Command::CopyFiles, Some(c.index()));
            }
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            if let Some(c) = self.controller() {
                super::run_command(Command::PasteFiles, Some(c.index()));
            }
        }

        // QLPreviewPanelController (an informal protocol): Quick Look finds
        // the controller in the key window's responder chain.
        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn accepts_preview_panel_control(&self, _panel: Option<&QLPreviewPanel>) -> bool {
            true
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn begin_preview_panel_control(&self, panel: Option<&QLPreviewPanel>) {
            let Some(panel) = panel else { return };
            unsafe {
                panel.setDataSource(Some(ProtocolObject::from_ref(self)));
                panel.setDelegate(Some(self.as_ref()));
            }
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn end_preview_panel_control(&self, panel: Option<&QLPreviewPanel>) {
            if let Some(panel) = panel {
                unsafe {
                    panel.setDataSource(None);
                    panel.setDelegate(None);
                }
            }
        }
    }

    unsafe impl NSObjectProtocol for FileTable {}

    unsafe impl NSMenuItemValidation for FileTable {
        /// Edit > Copy and Paste: files only on the Mac; pasting needs files.
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            self.validate_edit_item(item)
        }
    }

    unsafe impl QLPreviewPanelDataSource for FileTable {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn number_of_preview_items(&self, _panel: Option<&QLPreviewPanel>) -> NSInteger {
            self.controller().map_or(0, |c| c.selected_paths().len() as NSInteger)
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        fn preview_item(
            &self,
            _panel: Option<&QLPreviewPanel>,
            index: NSInteger,
        ) -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
            self.preview_item_at(index)
        }
    }

    unsafe impl QLPreviewPanelDelegate for FileTable {
        /// Arrow keys in Quick Look move through the list, like in Finder.
        #[unsafe(method(previewPanel:handleEvent:))]
        fn preview_panel_handle_event(&self, panel: Option<&QLPreviewPanel>, event: Option<&NSEvent>) -> bool {
            self.handle_preview_event(panel, event)
        }
    }

    unsafe impl NSWindowDelegate for FileTable {}
);

impl FileTable {
    fn double_clicked_parent(&self, event: &NSEvent) -> bool {
        if event.clickCount() != 2 {
            return false;
        }
        let point = self.convertPoint_fromView(event.locationInWindow(), None);
        let Some(controller) = self.controller() else { return false };
        if self.rowAtPoint(point) != 0 || !is_parent_row(&controller.ivars().state.borrow(), 0) {
            return false;
        }
        controller.go_up();
        true
    }

    fn validate_edit_item(&self, item: &NSMenuItem) -> bool {
        let Some(controller) = self.controller() else { return false };
        let action = item.action();
        if action == Some(sel!(copy:)) {
            controller.is_local() && !controller.selected_paths().is_empty()
        } else if action == Some(sel!(paste:)) {
            !paths_on_pasteboard(&objc2_app_kit::NSPasteboard::generalPasteboard()).is_empty()
        } else {
            true
        }
    }

    fn preview_item_at(&self, index: NSInteger) -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
        let paths = self.controller()?.selected_paths();
        let path = paths.get(usize::try_from(index).ok()?)?;
        Some(ProtocolObject::from_retained(file_url(path)))
    }

    fn handle_preview_event(&self, panel: Option<&QLPreviewPanel>, event: Option<&NSEvent>) -> bool {
        let Some(event) = event else { return false };
        if event.r#type() != objc2_app_kit::NSEventType::KeyDown || !matches!(event.keyCode(), 125 | 126) {
            return false;
        }
        let _: () = unsafe { msg_send![super(self), keyDown: event] };
        if let Some(panel) = panel {
            unsafe { panel.reloadData() };
        }
        true
    }

    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TableIvars { controller: OnceCell::new() });
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }

    fn controller(&self) -> Option<Retained<PaneController>> {
        self.ivars().controller.get().and_then(Weak::load)
    }
}

pub struct PaneIvars {
    index: usize,
    state: RefCell<PaneState>,
    table: OnceCell<Retained<FileTable>>,
    path_control: OnceCell<Retained<NSPathControl>>,
    status: OnceCell<Retained<NSTextField>>,
    container: OnceCell<Retained<NSStackView>>,
    active: Cell<bool>,
    /// The right-clicked row (outside the selection) of the open context menu.
    menu_row: Cell<Option<usize>>,
    /// Set while a context menu command runs: what it applies to.
    context_row: Cell<Option<usize>>,
    /// The column chooser shown on the column titles.
    header_menu: OnceCell<Retained<NSMenu>>,
    /// The filter field above the list.
    filter: OnceCell<Retained<objc2_app_kit::NSSearchField>>,
    /// What to select once a remote listing arrives.
    pending_select: RefCell<Option<Vec<PathBuf>>>,
    /// The folders of the path bar's items when remote (they have no URLs).
    crumbs: RefCell<Vec<PathBuf>>,
}

/// The pasteboard type of items dragged from a remote pane (between panes
/// only; Finder cannot take them).
pub const ITEMS_TYPE: &str = "io.github.houseofmackee.forkz.items";

thread_local! {
    /// What is being dragged from a pane: its file system and paths.
    static DRAG: RefCell<Option<(FsRef, Vec<PathBuf>)>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzPaneController"]
    #[ivars = PaneIvars]
    pub struct PaneController;

    impl PaneController {
        #[unsafe(method(rowDoubleClicked:))]
        fn row_double_clicked(&self, _sender: Option<&AnyObject>) {
            let clicked = self.table().clickedRow();
            if clicked == 0 && is_parent_row(&self.ivars().state.borrow(), 0) {
                self.go_up();
            } else if clicked >= 0 {
                self.open_selection();
            }
        }

        #[unsafe(method(pathClicked:))]
        fn path_clicked(&self, sender: &NSPathControl) {
            self.path_item_clicked(sender);
        }

        #[unsafe(method(toggleColumn:))]
        fn toggle_column(&self, sender: &NSMenuItem) {
            if let Some((id, _)) = crate::settings::COLUMNS.get(sender.tag() as usize) {
                super::toggle_column(self.is_local(), id);
            }
        }

        #[unsafe(method(command:))]
        fn command(&self, sender: &NSMenuItem) {
            if let Some(command) = Command::from_tag(sender.tag()) {
                self.ivars().context_row.set(self.ivars().menu_row.take());
                super::run_command(command, Some(self.index()));
                self.ivars().context_row.set(None);
            }
        }
    }

    unsafe impl NSObjectProtocol for PaneController {}

    unsafe impl NSTableViewDataSource for PaneController {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            let state = self.ivars().state.borrow();
            (state.entries().len() + parent_offset(&state)) as NSInteger
        }

        #[unsafe(method(tableView:sortDescriptorsDidChange:))]
        fn sort_descriptors_did_change(&self, table: &NSTableView, _old: &NSArray<NSSortDescriptor>) {
            let Some(descriptor) = table.sortDescriptors().firstObject() else { return };
            let key = descriptor.key().and_then(|k| SortKey::from_id(&k.to_string())).unwrap_or_default();
            let sort = Sort { key, ascending: descriptor.ascending() };
            let selected = self.selected_paths();
            self.ivars().state.borrow_mut().set_sort(sort);
            self.table().reloadData();
            self.select_paths(&selected);
            super::pane_sort_changed(self.index(), sort);
        }

        #[unsafe(method_id(tableView:pasteboardWriterForRow:))]
        fn pasteboard_writer(
            &self,
            _table: &NSTableView,
            row: NSInteger,
        ) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
            self.writer_for_row(row)
        }

        #[unsafe(method(tableView:draggingSession:willBeginAtPoint:forRowIndexes:))]
        fn dragging_will_begin(
            &self,
            _table: &NSTableView,
            _session: &objc2_app_kit::NSDraggingSession,
            _point: NSPoint,
            rows: &NSIndexSet,
        ) {
            let state = self.ivars().state.borrow();
            let paths =
                index_set_rows(rows).into_iter().filter_map(|r| entry_at(&state, r)).map(|e| e.path.clone()).collect();
            DRAG.with(|d| *d.borrow_mut() = Some((state.fs().clone(), paths)));
        }

        #[unsafe(method(tableView:validateDrop:proposedRow:proposedDropOperation:))]
        fn validate_drop(
            &self,
            table: &NSTableView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            row: NSInteger,
            operation: NSTableViewDropOperation,
        ) -> NSDragOperation {
            let Some(target) = self.drop_target(row, operation) else {
                table.setDropRow_dropOperation(-1, NSTableViewDropOperation::On);
                return self.drop_operation(info, &self.location());
            };
            if target == self.location() {
                table.setDropRow_dropOperation(-1, NSTableViewDropOperation::On);
            }
            self.drop_operation(info, &target)
        }

        #[unsafe(method(tableView:acceptDrop:row:dropOperation:))]
        fn accept_drop(
            &self,
            _table: &NSTableView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            row: NSInteger,
            operation: NSTableViewDropOperation,
        ) -> bool {
            self.accept_drop_at(info, row, operation)
        }
    }

    unsafe impl NSControlTextEditingDelegate for PaneController {
        /// Typing in the filter field filters as you type.
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            self.filter_changed();
        }

        /// In the filter field, Escape clears it, and Return, Down, and Tab
        /// go back to the list.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn filter_command(&self, _control: &NSControl, _view: &NSTextView, command: objc2::runtime::Sel) -> bool {
            self.filter_key(command)
        }
    }

    unsafe impl NSSearchFieldDelegate for PaneController {}

    unsafe impl NSTextFieldDelegate for PaneController {}

    unsafe impl NSTableViewDelegate for PaneController {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            self.cell_for(table, column, row)
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, _notification: &NSNotification) {
            self.update_status();
            refresh_quick_look(self.mtm());
        }

        #[unsafe(method_id(tableView:typeSelectStringForTableColumn:row:))]
        fn type_select_string(
            &self,
            _table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSString>> {
            self.type_select_for(column, row)
        }
    }

    unsafe impl NSMenuDelegate for PaneController {
        /// The context menu, for the clicked row (or the empty area).
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            menu.removeAllItems();
            if self.ivars().header_menu.get().is_some_and(|m| std::ptr::eq(&**m, menu)) {
                self.build_header_menu(menu);
                return;
            }
            let table = self.table();
            let clicked = table.clickedRow();
            let on_parent = clicked == 0 && is_parent_row(&self.ivars().state.borrow(), 0);
            let clicked = if on_parent { -1 } else { clicked };
            let outside = clicked >= 0 && !table.isRowSelected(clicked);
            self.ivars().menu_row.set(if outside { usize::try_from(clicked).ok() } else { None });
            if clicked < 0 {
                // A right-click on the empty area is about the folder.
                unsafe { table.deselectAll(None) };
            }
            let has_items = outside || !self.selected_paths().is_empty();
            self.build_context_menu(menu, has_items);
        }
    }
);

impl PaneController {
    /// The context menu: commands that work where this pane is (the Mac or
    /// a server), worded for it, and disabled when they cannot apply.
    fn build_context_menu(&self, menu: &NSMenu, has_items: bool) {
        let mtm = self.mtm();
        let index = self.index();
        menu.setAutoenablesItems(false);
        let add = |command: Option<Command>| match command {
            None => menu.addItem(&NSMenuItem::separatorItem(mtm)),
            Some(command) => {
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &ns(command.title()),
                        Some(sel!(command:)),
                        &ns(""),
                    )
                };
                item.setTag(command.tag());
                unsafe { item.setTarget(Some(self)) };
                super::prepare_item(&item, command, index);
                item.setEnabled(super::is_enabled_in(command, index, has_items));
                menu.addItem(&item);
            }
        };
        use Command::*;
        let commands: &[Option<Command>] = match (self.is_local(), has_items) {
            (true, true) => &[
                Some(Open),
                Some(QuickLook),
                None,
                Some(CopyToOther),
                Some(MoveToOther),
                None,
                Some(Rename),
                Some(Duplicate),
                Some(Trash),
                None,
                Some(Permissions),
                Some(CompareFiles),
                Some(CalculateSizes),
                Some(CopySha256),
                None,
                Some(CopyFiles),
                Some(CopyPath),
                Some(RevealInFinder),
                Some(AddToSidebar),
            ],
            (true, false) => &[
                Some(NewFolder),
                Some(PasteFiles),
                None,
                Some(OpenInTerminal),
                Some(RevealInFinder),
                Some(AddToSidebar),
                None,
                Some(CalculateSizes),
                Some(ToggleHidden),
                Some(Refresh),
            ],
            (false, true) => &[
                Some(Open),
                None,
                Some(CopyToOther),
                Some(MoveToOther),
                None,
                Some(Rename),
                Some(Duplicate),
                Some(Trash),
                None,
                Some(Permissions),
                Some(CompareFiles),
                Some(CalculateSizes),
                Some(CopySha256),
                None,
                Some(CopyPath),
                Some(AddToSidebar),
            ],
            (false, false) => &[
                Some(NewFolder),
                Some(PasteFiles),
                None,
                Some(OpenInTerminal),
                Some(CopyPath),
                Some(AddToSidebar),
                None,
                Some(CalculateSizes),
                Some(ToggleHidden),
                Some(Refresh),
                None,
                Some(Disconnect),
            ],
        };
        for command in commands {
            add(*command);
        }
    }

    fn writer_for_row(&self, row: NSInteger) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
        let state = self.ivars().state.borrow();
        let path = entry_at(&state, usize::try_from(row).ok()?)?.path.clone();
        if state.is_local() {
            return Some(ProtocolObject::from_retained(file_url(&path)));
        }
        // Remote items: a marker; the paths are in DRAG.
        let item = objc2_app_kit::NSPasteboardItem::new();
        item.setString_forType(&ns(&path.to_string_lossy()), &ns(ITEMS_TYPE));
        Some(ProtocolObject::from_retained(item))
    }

    /// Where dragged items come from: another pane (any file system) or
    /// Finder (local files).
    fn drag_sources(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> Option<(FsRef, Vec<PathBuf>)> {
        let pasteboard = info.draggingPasteboard();
        let types = pasteboard.types().map(|t| t.iter().map(|t| t.to_string()).collect::<Vec<_>>()).unwrap_or_default();
        if types.iter().any(|t| t == ITEMS_TYPE) {
            return DRAG.with(|d| d.borrow().clone());
        }
        let paths = paths_on_pasteboard(&pasteboard);
        if paths.is_empty() { None } else { Some((vfs::local(), paths)) }
    }

    fn accept_drop_at(
        &self,
        info: &ProtocolObject<dyn NSDraggingInfo>,
        row: NSInteger,
        operation: NSTableViewDropOperation,
    ) -> bool {
        let target = self.drop_target(row, operation).unwrap_or_else(|| self.location());
        let kind = match self.drop_operation(info, &target) {
            op if op == NSDragOperation::Move => JobKind::Move,
            op if op == NSDragOperation::Copy => JobKind::Copy,
            _ => return false,
        };
        let Some((source_fs, sources)) = self.drag_sources(info) else { return false };
        super::start_job(super::transfer_job(kind, source_fs, sources, self.fs(), target));
        true
    }

    fn cell_for(
        &self,
        table: &NSTableView,
        column: Option<&NSTableColumn>,
        row: NSInteger,
    ) -> Option<Retained<NSView>> {
        let column_id = column?.identifier().to_string();
        let state = self.ivars().state.borrow();
        let row = usize::try_from(row).ok()?;
        let cell = reusable_cell(table, &column_id, self.mtm());
        // SAFETY: plain getters of the cell's own subviews.
        let (image_view, text_field) = unsafe { (cell.imageView(), cell.textField()) };
        if is_parent_row(&state, row) {
            // "..": the enclosing folder.
            let text = match column_id.as_str() {
                "name" => "..",
                _ => "",
            };
            if column_id == "name"
                && let Some(image) = &image_view
            {
                image.setImage(Some(&super::util::folder_icon()));
                image.setAlphaValue(1.0);
            }
            if let Some(field) = &text_field {
                field.setStringValue(&ns(text));
                field.setAlphaValue(1.0);
            }
            return Some(Retained::into_super(cell));
        }
        let entry = entry_at(&state, row)?;
        let text = match column_id.as_str() {
            "name" => {
                if let Some(image) = &image_view {
                    image.setImage(Some(&icon_of(entry, state.is_local())));
                }
                entry.name.clone()
            }
            "size" if entry.is_folder_like() => match state.folder_size(&entry.path) {
                Some(size) => fs::format_size(size),
                None => "--".into(),
            },
            "size" => fs::format_size(entry.size),
            "modified" => format_date(entry.modified),
            "permissions" => fs::format_permissions(entry),
            "owner" => entry.owner.clone().unwrap_or_default(),
            "group" => entry.group.clone().unwrap_or_default(),
            _ => kind_of(entry),
        };
        if let Some(field) = text_field {
            field.setStringValue(&ns(&text));
            // Hidden items are dimmed, like in Finder.
            let alpha = if entry.hidden { 0.5 } else { 1.0 };
            field.setAlphaValue(alpha);
            if let Some(image) = &image_view {
                image.setAlphaValue(alpha);
            }
        }
        Some(Retained::into_super(cell))
    }

    fn type_select_for(&self, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSString>> {
        if column?.identifier().to_string() != "name" {
            return None;
        }
        let state = self.ivars().state.borrow();
        entry_at(&state, usize::try_from(row).ok()?).map(|e| ns(&e.name))
    }

    pub fn new(mtm: MainThreadMarker, index: usize, state: PaneState) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PaneIvars {
            index,
            state: RefCell::new(state),
            table: OnceCell::new(),
            path_control: OnceCell::new(),
            status: OnceCell::new(),
            container: OnceCell::new(),
            active: Cell::new(false),
            menu_row: Cell::new(None),
            context_row: Cell::new(None),
            pending_select: RefCell::new(None),
            header_menu: OnceCell::new(),
            filter: OnceCell::new(),
            crumbs: RefCell::new(Vec::new()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build();
        this
    }

    fn build(&self) {
        let mtm = self.mtm();
        let table = FileTable::new(mtm);
        let _ = table.ivars().controller.set(Weak::from_retained(&self.retain()));
        // Set first: configuring the table already calls the delegate.
        let _ = self.ivars().table.set(table.clone());

        for (id, title, width, align) in [
            ("name", "Name", 200.0, NSTextAlignment::Left),
            ("modified", "Date Modified", 140.0, NSTextAlignment::Left),
            ("size", "Size", 70.0, NSTextAlignment::Right),
            ("kind", "Kind", 100.0, NSTextAlignment::Left),
            ("permissions", "Permissions", 104.0, NSTextAlignment::Left),
            ("owner", "Owner", 90.0, NSTextAlignment::Left),
            ("group", "Group", 90.0, NSTextAlignment::Left),
        ] {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
            column.setTitle(&ns(title));
            column.setWidth(width);
            column.setMinWidth(if id == "name" { 120.0 } else { 50.0 });
            column.setResizingMask(NSTableColumnResizingOptions::UserResizingMask);
            column.headerCell().setAlignment(align);
            // Size and date sort largest/newest first on the first click.
            let ascending = !matches!(id, "size" | "modified");
            if !matches!(id, "permissions" | "owner" | "group") {
                let prototype = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&ns(id)), ascending);
                column.setSortDescriptorPrototype(Some(&prototype));
            }
            table.addTableColumn(&column);
        }
        table.setColumnAutoresizingStyle(NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle);
        table.setAllowsMultipleSelection(true);
        table.setUsesAlternatingRowBackgroundColors(true);
        table.setStyle(NSTableViewStyle::FullWidth);
        table.setRowHeight(ROW_HEIGHT);
        table.setIntercellSpacing(NSSize::new(6.0, 0.0));
        table.setAutosaveName(Some(&ns(&format!("ForkzPane{}", self.index()))));
        table.setAutosaveTableColumns(true);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
            table.setTarget(Some(self));
            table.setDoubleAction(Some(sel!(rowDoubleClicked:)));
        }
        table.setDraggingSourceOperationMask_forLocal(
            NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic,
            true,
        );
        table.setDraggingSourceOperationMask_forLocal(
            NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic,
            false,
        );
        let items_type = ns(ITEMS_TYPE);
        table.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }, &*items_type]));
        let sort = self.ivars().state.borrow().sort;
        let descriptor = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&ns(sort.key.id())), sort.ascending);
        table.setSortDescriptors(&NSArray::from_slice(&[&*descriptor]));

        let menu = NSMenu::new(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        unsafe { table.setMenu(Some(&menu)) };
        // Right-click on the column titles: choose the columns.
        let header_menu = NSMenu::new(mtm);
        header_menu.setDelegate(Some(ProtocolObject::from_ref(self)));
        if let Some(header) = table.headerView() {
            unsafe { header.setMenu(Some(&header_menu)) };
        }
        let _ = self.ivars().header_menu.set(header_menu);

        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);
        scroll.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);

        let path_control = NSPathControl::new(mtm);
        unsafe {
            path_control.setTarget(Some(self));
            path_control.setAction(Some(sel!(pathClicked:)));
        }
        path_control
            .setContentCompressionResistancePriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);

        let status = small_label("", mtm);
        status.setContentCompressionResistancePriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);

        let container = NSStackView::new(mtm);
        container.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        container.setSpacing(4.0);
        container.setEdgeInsets(NSEdgeInsets { top: 4.0, left: 0.0, bottom: 4.0, right: 0.0 });
        // The filter field sits to the right of the path bar.
        let filter = objc2_app_kit::NSSearchField::new(mtm);
        filter.setPlaceholderString(Some(&ns("Filter")));
        filter.widthAnchor().constraintEqualToConstant(150.0).setActive(true);
        filter.setControlSize(objc2_app_kit::NSControlSize::Small);
        filter.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(objc2_app_kit::NSFont::smallSystemFontSize())));
        unsafe { filter.setDelegate(Some(ProtocolObject::from_ref(self))) };
        let path_row = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[&*path_control as &NSView, &*filter as &NSView]),
            mtm,
        );
        path_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        path_row.setSpacing(8.0);
        container.addArrangedSubview(&path_row);
        container.addArrangedSubview(&scroll);
        container.addArrangedSubview(&status);
        let _ = self.ivars().filter.set(filter);
        for view in [&*path_row as &NSView, &scroll, &status] {
            view.leadingAnchor().constraintEqualToAnchor_constant(&container.leadingAnchor(), 6.0).setActive(true);
            view.trailingAnchor().constraintEqualToAnchor_constant(&container.trailingAnchor(), -6.0).setActive(true);
        }

        let _ = self.ivars().path_control.set(path_control);
        let _ = self.ivars().status.set(status);
        let _ = self.ivars().container.set(container);
        self.refresh(Some(&[]));
        self.start_loading();
    }

    pub fn fs(&self) -> FsRef {
        self.ivars().state.borrow().fs().clone()
    }

    pub fn is_local(&self) -> bool {
        self.ivars().state.borrow().is_local()
    }

    pub fn show_hidden(&self) -> bool {
        self.ivars().state.borrow().show_hidden()
    }

    /// Lists a remote folder in the background if the state is waiting for it.
    fn start_loading(&self) {
        let (fs, location, generation) = {
            let state = self.ivars().state.borrow();
            if !state.loading {
                return;
            }
            (state.fs().clone(), state.location().to_path_buf(), state.generation())
        };
        let index = self.index();
        std::thread::spawn(move || {
            let result = fs.list(&location);
            DispatchQueue::main().exec_async(move || super::listing_arrived(index, generation, result));
        });
    }

    /// A background listing arrived.
    pub fn listing_arrived(&self, generation: u64, result: std::io::Result<Vec<crate::fs::Entry>>) {
        if !self.ivars().state.borrow_mut().set_listing(generation, result) {
            return;
        }
        let pending = self.ivars().pending_select.take();
        self.refresh(pending.as_deref());
    }

    fn path_item_clicked(&self, sender: &NSPathControl) {
        let Some(item) = sender.clickedPathItem() else { return };
        let path = match item.URL().and_then(|u| url_path(&u)) {
            Some(path) if self.is_local() => Some(path),
            _ => {
                let items = sender.pathItems();
                let index = items.iter().position(|i| std::ptr::eq(&*i, &*item));
                index.and_then(|i| self.ivars().crumbs.borrow().get(i).cloned())
            }
        };
        if let Some(path) = path {
            self.navigate(path);
        }
        self.focus();
    }

    /// The path bar: Finder's look for local folders; the server and each
    /// folder for remote ones.
    fn update_path_bar(&self) {
        let Some(path_control) = self.ivars().path_control.get() else { return };
        let state = self.ivars().state.borrow();
        if state.is_local() {
            path_control.setURL(Some(&file_url(state.location())));
            return;
        }
        let mut crumbs: Vec<PathBuf> = state.location().ancestors().map(Path::to_path_buf).collect();
        crumbs.reverse();
        let items: Vec<Retained<objc2_app_kit::NSPathControlItem>> = crumbs
            .iter()
            .map(|crumb| {
                let item = objc2_app_kit::NSPathControlItem::new();
                let title = if crumb.parent().is_none() { state.fs().label() } else { crate::pane::title_of(crumb) };
                item.setTitle(&ns(&title));
                let image = if crumb.parent().is_none() {
                    super::util::symbol("server.rack", &title)
                } else {
                    Some(super::util::folder_icon())
                };
                if let Some(image) = image {
                    item.setImage(Some(&image));
                }
                item
            })
            .collect();
        path_control.setPathItems(&NSArray::from_retained_slice(&items));
        *self.ivars().crumbs.borrow_mut() = crumbs;
    }

    pub fn index(&self) -> usize {
        self.ivars().index
    }

    pub fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.ivars().container.get().expect("built").clone())
    }

    pub fn table(&self) -> &FileTable {
        self.ivars().table.get().expect("built")
    }

    pub fn location(&self) -> PathBuf {
        self.ivars().state.borrow().location().to_path_buf()
    }

    pub fn can_go_back(&self) -> bool {
        self.ivars().state.borrow().can_go_back()
    }

    pub fn can_go_forward(&self) -> bool {
        self.ivars().state.borrow().can_go_forward()
    }

    pub fn focus(&self) {
        if let Some(window) = self.table().window() {
            window.makeFirstResponder(Some(self.table()));
        }
    }

    pub fn set_active(&self, active: bool) {
        self.ivars().active.set(active);
        self.update_status();
    }

    /// Selected rows' paths, in list order.
    pub fn selected_paths(&self) -> Vec<PathBuf> {
        let rows = index_set_rows(&self.table().selectedRowIndexes());
        let state = self.ivars().state.borrow();
        // ".." is never part of what commands act on.
        rows.into_iter().filter_map(|r| entry_at(&state, r)).map(|e| e.path.clone()).collect()
    }

    /// ".." is the only thing selected (to open it, going up).
    pub fn parent_row_chosen(&self) -> bool {
        let rows = index_set_rows(&self.table().selectedRowIndexes());
        rows == [0] && is_parent_row(&self.ivars().state.borrow(), 0)
    }

    /// What a command applies to: the right-clicked row if a context menu
    /// command is running for a row outside the selection, otherwise the
    /// selection.
    pub fn target_paths(&self) -> Vec<PathBuf> {
        if let Some(row) = self.ivars().context_row.get()
            && let Some(entry) = entry_at(&self.ivars().state.borrow(), row)
        {
            return vec![entry.path.clone()];
        }
        self.selected_paths()
    }

    pub fn select_paths(&self, paths: &[PathBuf]) {
        let set = NSMutableIndexSet::new();
        let mut first = None;
        {
            let state = self.ivars().state.borrow();
            for path in paths {
                if let Some(row) = state.row_of(path).map(|r| r + parent_offset(&state)) {
                    set.addIndex(row);
                    first = Some(first.map_or(row, |f: usize| f.min(row)));
                }
            }
        }
        self.table().selectRowIndexes_byExtendingSelection(&set, false);
        if let Some(row) = first {
            self.table().scrollRowToVisible(row as NSInteger);
        }
    }

    /// Updates the list from the state, then selects `select` (or keeps
    /// the selection if None).
    fn refresh(&self, select: Option<&[PathBuf]>) {
        let keep = if select.is_none() { self.selected_paths() } else { Vec::new() };
        let first_row = self.table().selectedRow();
        self.table().reloadData();
        match select {
            Some(paths) if !paths.is_empty() => self.select_paths(paths),
            Some(_) => {
                // A new folder starts with its first item selected, ready
                // for the keyboard.
                let first = {
                    let state = self.ivars().state.borrow();
                    let offset = parent_offset(&state);
                    if offset > 0 && super::start_on_parent() {
                        NSIndexSet::indexSetWithIndex(0)
                    } else if !state.entries().is_empty() {
                        NSIndexSet::indexSetWithIndex(offset)
                    } else if offset > 0 {
                        NSIndexSet::indexSetWithIndex(0)
                    } else {
                        NSIndexSet::new()
                    }
                };
                self.table().selectRowIndexes_byExtendingSelection(&first, false);
                self.table().scrollRowToVisible(0);
            }
            None => {
                self.select_paths(&keep);
                // The selected items are gone (moved or deleted): select
                // what took their place, like Finder.
                let count = {
                    let state = self.ivars().state.borrow();
                    (state.entries().len() + parent_offset(&state)) as NSInteger
                };
                if !keep.is_empty() && self.table().selectedRow() < 0 && first_row >= 0 && count > 0 {
                    let row = first_row.min(count - 1) as usize;
                    self.table().selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row), false);
                }
            }
        }
        if self.ivars().state.borrow().loading {
            // Select once the listing is there.
            let wanted = match select {
                Some(paths) => paths.to_vec(),
                None => keep,
            };
            *self.ivars().pending_select.borrow_mut() = Some(wanted);
        }
        self.apply_columns();
        self.sync_filter_field();
        self.update_path_bar();
        self.update_status();
    }

    fn filter_changed(&self) {
        let Some(field) = self.ivars().filter.get() else { return };
        let text = field.stringValue().to_string();
        let selected = self.selected_paths();
        self.ivars().state.borrow_mut().set_filter(text.trim());
        self.table().reloadData();
        if selected.is_empty() {
            self.refresh(Some(&[]));
        } else {
            self.select_paths(&selected);
            if self.table().selectedRow() < 0 {
                self.refresh(Some(&[]));
            }
        }
        self.update_status();
    }

    fn filter_key(&self, command: objc2::runtime::Sel) -> bool {
        if command == sel!(cancelOperation:) {
            if let Some(field) = self.ivars().filter.get() {
                field.setStringValue(&ns(""));
            }
            self.filter_changed();
            self.focus();
            true
        } else if command == sel!(insertNewline:) || command == sel!(moveDown:) || command == sel!(insertTab:) {
            self.focus();
            true
        } else {
            false
        }
    }

    /// Edit > Filter (Command-F): to the filter field.
    pub fn focus_filter(&self) {
        if let (Some(field), Some(window)) = (self.ivars().filter.get(), self.table().window()) {
            window.makeFirstResponder(Some(field));
        }
    }

    /// Empties the filter field (after going to another folder).
    fn sync_filter_field(&self) {
        if let Some(field) = self.ivars().filter.get() {
            let text = self.ivars().state.borrow().filter_text().to_string();
            if field.stringValue().to_string().trim() != text {
                field.setStringValue(&ns(&text));
            }
        }
    }

    /// The folders shown in the list (to calculate all their sizes).
    pub fn folder_paths(&self) -> Vec<PathBuf> {
        let state = self.ivars().state.borrow();
        state.entries().iter().filter(|e| e.kind == crate::fs::EntryKind::Folder).map(|e| e.path.clone()).collect()
    }

    /// A folder's calculated size arrived.
    pub fn set_folder_size(&self, path: &Path, size: u64) {
        let selected = self.selected_paths();
        self.ivars().state.borrow_mut().set_folder_size(path, size);
        self.table().reloadData();
        self.select_paths(&selected);
        self.update_status();
    }

    /// Shows the columns chosen for where the pane is (the Mac or a
    /// server), and fits them to the pane if that changed anything.
    pub fn apply_columns(&self) {
        let visible = super::visible_columns(self.is_local());
        let mut changed = false;
        for (id, _) in crate::settings::COLUMNS {
            if let Some(column) = self.table().tableColumnWithIdentifier(&ns(id)) {
                let hidden = !visible.iter().any(|v| v == id);
                if column.isHidden() != hidden {
                    column.setHidden(hidden);
                    changed = true;
                }
            }
        }
        if changed {
            self.fit_columns();
        }
    }

    /// The column chooser: the optional columns with check marks.
    fn build_header_menu(&self, menu: &NSMenu) {
        let visible = super::visible_columns(self.is_local());
        let where_ = if self.is_local() { "on the Mac" } else { "on servers" };
        let note = NSMenuItem::new(self.mtm());
        note.setTitle(&ns(&format!("Columns {where_}")));
        note.setEnabled(false);
        menu.setAutoenablesItems(false);
        menu.addItem(&note);
        for (index, (id, title)) in crate::settings::COLUMNS.iter().enumerate() {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(self.mtm()),
                    &ns(title),
                    Some(sel!(toggleColumn:)),
                    &ns(""),
                )
            };
            item.setTag(index as isize);
            unsafe { item.setTarget(Some(self)) };
            let on = visible.iter().any(|v| v == id);
            item.setState(if on {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
            menu.addItem(&item);
        }
    }

    /// The Name column gives or takes room so the visible columns fill the
    /// pane exactly (after a column is shown or hidden).
    pub fn fit_columns(&self) {
        let table = self.table();
        let Some(scroll) = table.enclosingScrollView() else { return };
        let available = scroll.contentSize().width;
        // Not laid out yet (the window is not shown): fitted later.
        if available < 100.0 {
            return;
        }
        let columns = table.tableColumns();
        let visible: Vec<_> = columns.iter().filter(|c| !c.isHidden()).collect();
        let spacing = table.intercellSpacing().width * visible.len() as f64;
        let total: f64 = visible.iter().map(|c| c.width()).sum::<f64>() + spacing;
        if let Some(name) = table.tableColumnWithIdentifier(&ns("name")) {
            let width = (name.width() + available - total).max(name.minWidth());
            name.setWidth(width.floor());
        }
        // The table keeps its old width otherwise (and the first column
        // would stretch back to fill it).
        let height = table.frame().size.height;
        table.setFrameSize(NSSize::new(available, height));
    }

    /// The entries shown for these paths (from the listing, no file access).
    pub fn entries_for(&self, paths: &[PathBuf]) -> Vec<crate::fs::Entry> {
        let state = self.ivars().state.borrow();
        paths.iter().filter_map(|p| state.row_of(p).and_then(|r| state.entry(r))).cloned().collect()
    }

    pub fn update_status(&self) {
        let Some(status) = self.ivars().status.get() else { return };
        let state = self.ivars().state.borrow();
        let text = match &state.error {
            Some(error) => format!("Cannot read this folder: {error}"),
            None if state.loading && state.entries().is_empty() => {
                format!("Loading from {}\u{2026}", state.fs().label())
            }
            None => {
                let offset = parent_offset(&state);
                let rows: Vec<usize> = index_set_rows(&self.table().selectedRowIndexes())
                    .into_iter()
                    .filter(|&r| r >= offset)
                    .map(|r| r - offset)
                    .collect();
                let mut text = selection_summary(state.entries(), &rows);
                if !state.filter_text().is_empty() {
                    text.push_str(&format!(" (filtered from {})", state.unfiltered_count()));
                }
                if !state.is_local() {
                    text.push_str(&format!(", {}", state.fs().label()));
                } else if let Some(free) = fs::available_space(state.location()) {
                    text.push_str(&format!(", {} available", fs::format_size(free)));
                }
                text
            }
        };
        status.setStringValue(&ns(&text));
        let color = if self.ivars().active.get() { NSColor::labelColor() } else { NSColor::tertiaryLabelColor() };
        status.setTextColor(Some(&color));
    }

    /// Reads the folder again, keeping the selection.
    pub fn reload(&self) {
        self.ivars().state.borrow_mut().load();
        self.refresh(None);
        self.start_loading();
    }

    /// Reloads and selects `paths` (for example what a job created).
    pub fn reload_selecting(&self, paths: &[PathBuf]) {
        self.ivars().state.borrow_mut().load();
        self.refresh(Some(paths));
        self.start_loading();
    }

    pub fn navigate(&self, path: PathBuf) {
        let fs = self.fs();
        self.navigate_to(fs, path);
    }

    /// Goes to a folder on any file system (a server, or back to the Mac).
    pub fn navigate_to(&self, fs: FsRef, path: PathBuf) {
        self.navigate_selecting(fs, path, &[]);
    }

    pub fn navigate_selecting(&self, fs: FsRef, path: PathBuf, select: &[PathBuf]) {
        self.ivars().state.borrow_mut().navigate_to(fs, path);
        self.refresh(Some(select));
        self.start_loading();
        super::location_changed(self.index());
    }

    /// Leaves a server: back to the Mac, forgetting its history.
    pub fn leave(&self, key: &str) {
        self.ivars().state.borrow_mut().forget(key);
        if self.fs().key() == key {
            let local = vfs::local();
            let home = local.home();
            self.ivars().state.borrow_mut().navigate_to(local, home);
            self.ivars().state.borrow_mut().forget(key);
            self.refresh(Some(&[]));
            super::location_changed(self.index());
        }
    }

    pub fn go_up(&self) {
        let came_from = self.ivars().state.borrow_mut().go_up();
        self.after_history(came_from);
    }

    pub fn go_back(&self) {
        let came_from = self.ivars().state.borrow_mut().go_back();
        self.after_history(came_from);
    }

    pub fn go_forward(&self) {
        let came_from = self.ivars().state.borrow_mut().go_forward();
        self.after_history(came_from);
    }

    /// After going up or through history: select the folder we came from
    /// if it is in the new list.
    fn after_history(&self, came_from: Option<PathBuf>) {
        if came_from.is_none() {
            return;
        }
        let select: Vec<PathBuf> = came_from.into_iter().collect();
        self.refresh(Some(&select));
        self.start_loading();
        super::location_changed(self.index());
    }

    pub fn set_show_hidden(&self, show: bool) {
        self.ivars().state.borrow_mut().set_show_hidden(show);
        self.refresh(None);
    }

    /// Opens the selection: folders in the pane, everything else with its
    /// default app.
    pub fn open_selection(&self) {
        if self.parent_row_chosen() {
            self.go_up();
            return;
        }
        let paths = self.target_paths();
        let folder = {
            let state = self.ivars().state.borrow();
            paths
                .iter()
                .find_map(|p| state.row_of(p).and_then(|r| state.entry(r)).filter(|e| e.is_folder_like()))
                .map(|e| e.path.clone())
        };
        if let (Some(folder), 1) = (&folder, paths.len()) {
            self.navigate(folder.clone());
            return;
        }
        if !self.is_local() {
            // Download to a temporary folder, then open.
            let files: Vec<PathBuf> = paths.into_iter().filter(|p| Some(p) != folder.as_ref()).collect();
            super::open_remote(self.fs(), files);
            return;
        }
        let workspace = NSWorkspace::sharedWorkspace();
        for path in paths {
            if Some(&path) == folder.as_ref() {
                continue;
            }
            workspace.openURL(&file_url(&path));
        }
    }

    fn drop_target(&self, row: NSInteger, operation: NSTableViewDropOperation) -> Option<PathBuf> {
        if operation != NSTableViewDropOperation::On || row < 0 {
            return None;
        }
        let state = self.ivars().state.borrow();
        if is_parent_row(&state, row as usize) {
            return state.location().parent().map(Path::to_path_buf);
        }
        entry_at(&state, row as usize).filter(|e| e.is_folder_like()).map(|e| e.path.clone())
    }

    /// Move within a volume, copy across volumes, like Finder; Option
    /// forces a copy, Command a move.
    fn drop_operation(&self, info: &ProtocolObject<dyn NSDraggingInfo>, target: &Path) -> NSDragOperation {
        let Some((source_fs, sources)) = self.drag_sources(info) else { return NSDragOperation::None };
        let target_fs = self.fs();
        let same_fs = vfs::same(&source_fs, &target_fs);
        if sources.is_empty() || (same_fs && sources.iter().any(|s| s == target)) {
            return NSDragOperation::None;
        }
        // Dropping items back into the folder they are in does nothing.
        if same_fs && sources.iter().all(|s| s.parent() == Some(target)) {
            return NSDragOperation::None;
        }
        let mask = info.draggingSourceOperationMask();
        let same_volume = same_fs && (!target_fs.is_local() || fs::device_of(&sources[0]) == fs::device_of(target));
        if mask == NSDragOperation::Copy {
            NSDragOperation::Copy
        } else if mask == NSDragOperation::Move || (same_volume && mask.contains(NSDragOperation::Move)) {
            NSDragOperation::Move
        } else if mask.contains(NSDragOperation::Copy) {
            NSDragOperation::Copy
        } else if mask.contains(NSDragOperation::Generic) {
            if same_volume { NSDragOperation::Move } else { NSDragOperation::Copy }
        } else {
            NSDragOperation::None
        }
    }
}

/// 1 when the list starts with "..", the enclosing folder (every folder
/// but the top one), else 0. Table rows are entry indexes plus this.
fn parent_offset(state: &PaneState) -> usize {
    usize::from(state.location().parent().is_some())
}

fn is_parent_row(state: &PaneState, row: usize) -> bool {
    row == 0 && parent_offset(state) == 1
}

/// The entry shown in a table row; None for "..".
fn entry_at(state: &PaneState, row: usize) -> Option<&crate::fs::Entry> {
    row.checked_sub(parent_offset(state)).and_then(|i| state.entry(i))
}

/// File URLs on a pasteboard (from a drag, or Edit > Copy in Finder).
pub fn paths_on_pasteboard(pasteboard: &objc2_app_kit::NSPasteboard) -> Vec<PathBuf> {
    let Some(items) = pasteboard.pasteboardItems() else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| item.stringForType(unsafe { NSPasteboardTypeFileURL }))
        .filter_map(|s| objc2_foundation::NSURL::URLWithString(&s))
        .filter_map(|url| url_path(&url))
        .collect()
}

fn index_set_rows(set: &NSIndexSet) -> Vec<usize> {
    let mut rows = Vec::with_capacity(set.count());
    let mut index = set.firstIndex();
    while index != objc2_foundation::NSNotFound as usize {
        rows.push(index);
        index = set.indexGreaterThanIndex(index);
    }
    rows
}

/// A cell view for a column, reused when the table has one to spare.
fn reusable_cell(table: &NSTableView, column_id: &str, mtm: MainThreadMarker) -> Retained<NSTableCellView> {
    let identifier = ns(column_id);
    let existing: Option<Retained<NSView>> = unsafe { table.makeViewWithIdentifier_owner(&identifier, None) };
    if let Some(cell) = existing.and_then(|v| v.downcast::<NSTableCellView>().ok()) {
        return cell;
    }
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, ROW_HEIGHT));
    let cell = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), frame);
    cell.setIdentifier(Some(&identifier));
    let text_x = if column_id == "name" { 24.0 } else { 2.0 };
    let field = NSTextField::labelWithString(&ns(""), mtm);
    field.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingMiddle);
    field.setFrame(NSRect::new(NSPoint::new(text_x, 3.0), NSSize::new(200.0 - text_x - 2.0, 16.0)));
    field.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
    if column_id == "size" {
        field.setAlignment(NSTextAlignment::Right);
    }
    if matches!(column_id, "size" | "modified" | "kind" | "permissions" | "owner" | "group") {
        field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    if column_id == "permissions" {
        let size = objc2_app_kit::NSFont::smallSystemFontSize();
        field.setFont(Some(&objc2_app_kit::NSFont::monospacedSystemFontOfSize_weight(size, unsafe {
            objc2_app_kit::NSFontWeightRegular
        })));
    }
    cell.addSubview(&field);
    unsafe { cell.setTextField(Some(&field)) };
    if column_id == "name" {
        let image = NSImageView::initWithFrame(
            NSImageView::alloc(mtm),
            NSRect::new(NSPoint::new(3.0, 3.0), NSSize::new(16.0, 16.0)),
        );
        cell.addSubview(&image);
        unsafe { cell.setImageView(Some(&image)) };
    }
    cell
}

/// Quick Look shows the new selection if it is open.
fn refresh_quick_look(mtm: MainThreadMarker) {
    if unsafe { QLPreviewPanel::sharedPreviewPanelExists(mtm) }
        && let Some(panel) = unsafe { QLPreviewPanel::sharedPreviewPanel(mtm) }
        && panel.isVisible()
    {
        unsafe { panel.reloadData() };
    }
}

/// Shows or hides Quick Look for the active pane's selection.
pub fn toggle_quick_look(controller: &PaneController) {
    let mtm = mtm();
    if !controller.is_local() {
        return;
    }
    let Some(panel) = (unsafe { QLPreviewPanel::sharedPreviewPanel(mtm) }) else { return };
    if panel.isVisible() {
        panel.orderOut(None);
    } else if !controller.selected_paths().is_empty() {
        controller.focus();
        panel.makeKeyAndOrderFront(None);
    }
}
