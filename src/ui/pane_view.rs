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
    NSButton, NSColor, NSControl, NSControlTextEditingDelegate, NSDragOperation, NSDraggingInfo, NSEvent,
    NSEventModifierFlags, NSImageView, NSLayoutConstraintOrientation, NSMenu, NSMenuDelegate, NSMenuItem,
    NSMenuItemValidation, NSPasteboardTypeFileURL, NSPasteboardWriting, NSPathControl, NSResponder, NSScrollView,
    NSSearchFieldDelegate, NSStackView, NSTableCellView, NSTableColumn, NSTableColumnResizingOptions, NSTableView,
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

/// How the file lists look: text size, row height, and a version that
/// changes with them (cells are reused per version).
#[derive(Debug, Clone, Copy)]
struct Look {
    text: f64,
    row: f64,
    version: u32,
}

thread_local! {
    static LOOK: Cell<Look> = const { Cell::new(Look { text: 13.0, row: 22.0, version: 0 }) };
}

fn look() -> Look {
    LOOK.with(Cell::get)
}

/// The lists' text size and row height (for the column view).
pub fn look_metrics() -> (f64, f64) {
    let look = look();
    (look.text, look.row)
}

/// Sets the text size and row height of the file lists (applied by
/// `PaneController::apply_appearance`).
pub fn set_look(text: f64, row: f64) {
    LOOK.with(|l| {
        let old = l.get();
        if old.text != text || old.row != row {
            l.set(Look { text, row, version: old.version + 1 });
        }
    });
}

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
            match key_command(event) {
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
            match self.controller() {
                // In column view the (hidden) list passes the focus on.
                Some(controller) if controller.view_mode() != crate::pane::ViewMode::List => {
                    let index = controller.index();
                    DispatchQueue::main().exec_async(move || super::focus_pane(index));
                    false
                }
                controller => {
                    let accepted: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
                    if accepted && let Some(controller) = controller {
                        super::set_active(controller.index());
                    }
                    accepted
                }
            }
        }

        /// Edit > Copy: the selected files, to paste in Finder or the other pane.
        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            if let Some(c) = self.controller() {
                super::run_command(Command::CopyFiles, Some(c.index()));
            }
        }

        /// Select All: every item, but not "..", which is not an item.
        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            let set = NSMutableIndexSet::new();
            if let Some(c) = self.controller() {
                for row in item_rows(&c.ivars().state.borrow()) {
                    set.addIndex(row);
                }
            }
            self.selectRowIndexes_byExtendingSelection(&set, false);
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
    /// Every tab's state, in order; the active tab's slot is empty (its
    /// state is in `state`).
    tabs: RefCell<Vec<Option<PaneState>>>,
    /// Each tab's selection while it is not shown.
    tab_selections: RefCell<Vec<Vec<PathBuf>>>,
    active_tab: Cell<usize>,
    /// The tab buttons, above the path bar (hidden with one tab).
    tab_bar: OnceCell<Retained<NSStackView>>,
    /// View > Group By, for every tab of the pane.
    group: Cell<crate::pane::GroupBy>,
    /// The list's scroll view, and the column view shown instead of it.
    list_scroll: OnceCell<Retained<NSScrollView>>,
    columns: OnceCell<Retained<super::column_view::ColumnBrowser>>,
    icons: OnceCell<Retained<super::icon_view::IconView>>,
    icon_scroll: OnceCell<Retained<NSScrollView>>,
    mode: Cell<crate::pane::ViewMode>,
    /// Set while the column view changes the pane (it shows that itself).
    from_columns: Cell<bool>,
    /// The Git column: what `git status` says in this folder's repository.
    git: RefCell<crate::fs::git::Statuses>,
    git_running: Cell<bool>,
    git_again: Cell<bool>,
    /// A thin accent-colored line along the top, shown while this is the
    /// active pane (where commands, connections, and Go to Folder go).
    active_bar: OnceCell<Retained<objc2_app_kit::NSBox>>,
    /// Each tab button's hover area and its close button (shown while the
    /// mouse is over the tab).
    tab_close: RefCell<Vec<(Retained<objc2_app_kit::NSTrackingArea>, Retained<NSButton>)>>,
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
        /// The filter field's action (its clear button too).
        #[unsafe(method(filterAction:))]
        fn filter_action(&self, _sender: Option<&AnyObject>) {
            self.filter_changed();
        }

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

        #[unsafe(method(selectTab:))]
        fn select_tab_action(&self, sender: &NSButton) {
            self.select_tab(sender.tag() as usize);
            self.focus();
        }

        #[unsafe(method(newTabButton:))]
        fn new_tab_button(&self, _sender: Option<&AnyObject>) {
            self.new_tab(self.fs(), self.location());
            self.focus();
        }

        #[unsafe(method(closeTabFromMenu:))]
        fn close_tab_from_menu(&self, sender: &NSMenuItem) {
            self.close_tab(sender.tag() as usize);
        }

        #[unsafe(method(closeTabButton:))]
        fn close_tab_button(&self, sender: &NSButton) {
            self.close_tab(sender.tag() as usize);
        }

        /// The mouse moved onto a tab: show its close button.
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, event: &NSEvent) {
            self.show_tab_close(event, true);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            self.show_tab_close(event, false);
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
            self.ivars().state.borrow().rows().len() as NSInteger
        }

        #[unsafe(method(tableView:sortDescriptorsDidChange:))]
        fn sort_descriptors_did_change(&self, table: &NSTableView, _old: &NSArray<NSSortDescriptor>) {
            let Some(descriptor) = table.sortDescriptors().firstObject() else { return };
            let key = descriptor.key().and_then(|k| SortKey::from_id(&k.to_string())).unwrap_or_default();
            let sort = Sort { key, ascending: descriptor.ascending() };
            let selected = self.selected_paths();
            if let Some(columns) = self.ivars().columns.get() {
                columns.forget_listings();
            }
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

        /// ".." can be selected on its own (to open it), but is left out
        /// when other rows are selected too (Shift-click, Command-click),
        /// so a selection is only items.
        #[unsafe(method_id(tableView:selectionIndexesForProposedSelection:))]
        fn proposed_selection(&self, _table: &NSTableView, proposed: &NSIndexSet) -> Retained<NSIndexSet> {
            // Group titles are never selected, and ".." only on its own.
            let state = self.ivars().state.borrow();
            let rows = index_set_rows(proposed);
            let drop_parent = rows.len() > 1;
            let keep: Vec<usize> = rows
                .into_iter()
                .filter(|&r| !is_header_row(&state, r) && !(drop_parent && is_parent_row(&state, r)))
                .collect();
            let set = NSMutableIndexSet::new();
            for row in keep {
                set.addIndex(row);
            }
            Retained::into_super(set)
        }

        /// Group titles (View > Group By) float at the top while scrolling.
        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group_row(&self, _table: &NSTableView, row: NSInteger) -> bool {
            usize::try_from(row).is_ok_and(|r| is_header_row(&self.ivars().state.borrow(), r))
        }

        /// Rows in the theme's colors (macOS's own rows without a theme).
        #[unsafe(method_id(tableView:rowViewForRow:))]
        fn row_view(&self, _table: &NSTableView, row: NSInteger) -> Option<Retained<objc2_app_kit::NSTableRowView>> {
            super::themed::colors()
                .map(|_| Retained::into_super(super::themed::ThemedRowView::new(self.mtm(), row % 2 == 1)))
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
            // "..", a group title, and the empty area are about the folder.
            let on_parent = usize::try_from(clicked).is_ok_and(|r| {
                let state = self.ivars().state.borrow();
                is_parent_row(&state, r) || is_header_row(&state, r)
            });
            let clicked = if on_parent { -1 } else { clicked };
            let outside = clicked >= 0 && !table.isRowSelected(clicked);
            self.ivars().menu_row.set(if outside { usize::try_from(clicked).ok() } else { None });
            let in_list = self.ivars().mode.get() == crate::pane::ViewMode::List;
            if clicked < 0 && in_list {
                // A right-click on the empty area is about the folder.
                unsafe { table.deselectAll(None) };
            }
            let outside = outside && in_list;
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
                Some(OpenInNewTab),
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
                Some(NewTab),
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
                Some(OpenInNewTab),
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
                Some(NewTab),
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

    /// Items dragged from the column or icon view: file URLs on the Mac,
    /// file promises from servers (downloaded where they are dropped), and
    /// remembered for drops on panes.
    pub fn write_drag(&self, entries: &[crate::fs::Entry], pasteboard: &objc2_app_kit::NSPasteboard) -> bool {
        if entries.is_empty() {
            return false;
        }
        let fs = self.fs();
        DRAG.with(|d| *d.borrow_mut() = Some((fs.clone(), entries.iter().map(|e| e.path.clone()).collect())));
        let writers: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = entries
            .iter()
            .map(|e| -> Retained<ProtocolObject<dyn NSPasteboardWriting>> {
                if fs.is_local() {
                    ProtocolObject::from_retained(file_url(&e.path))
                } else {
                    ProtocolObject::from_retained(super::promises::provider(fs.clone(), e))
                }
            })
            .collect();
        pasteboard.clearContents();
        pasteboard.writeObjects(&NSArray::from_retained_slice(&writers))
    }

    fn writer_for_row(&self, row: NSInteger) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
        let state = self.ivars().state.borrow();
        let entry = entry_at(&state, usize::try_from(row).ok()?)?;
        if state.is_local() {
            return Some(ProtocolObject::from_retained(file_url(&entry.path)));
        }
        // Server items: file promises, downloaded where they are dropped.
        // Panes take the paths from DRAG instead.
        Some(ProtocolObject::from_retained(super::promises::provider(state.fs().clone(), entry)))
    }

    /// Where dragged items come from: another pane (any file system) or
    /// Finder (local files).
    fn drag_sources(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> Option<(FsRef, Vec<PathBuf>)> {
        dragged_items(info)
    }

    fn accept_drop_at(
        &self,
        info: &ProtocolObject<dyn NSDraggingInfo>,
        row: NSInteger,
        operation: NSTableViewDropOperation,
    ) -> bool {
        let target = self.drop_target(row, operation).unwrap_or_else(|| self.location());
        self.drop_into(info, target)
    }

    /// Items dropped on a folder of this pane: copied or moved there.
    pub fn drop_into(&self, info: &ProtocolObject<dyn NSDraggingInfo>, target: PathBuf) -> bool {
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
        let state = self.ivars().state.borrow();
        let row = usize::try_from(row).ok()?;
        // A group's title spans the row (the table asks without a column).
        if let Some(crate::pane::Row::Header(title)) = state.rows().get(row) {
            return Some(group_title(title, self.mtm()));
        }
        let column_id = column?.identifier().to_string();
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
                    let icon = icon_of(entry, state.is_local());
                    // Tag colors on the icon, like Finder's dots.
                    let colors: Vec<u8> = if state.is_local() {
                        crate::fs::tags::read(&entry.path).iter().map(|t| t.color).filter(|c| *c > 0).collect()
                    } else {
                        Vec::new()
                    };
                    let icon = if colors.is_empty() { icon } else { super::util::icon_with_tags(&icon, &colors) };
                    image.setImage(Some(&icon));
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
            "tags" if state.is_local() => super::util::tags_of(&entry.path).join(", "),
            "git" => {
                let status = self.ivars().git.borrow().of(&entry.path, entry.is_folder_like());
                if let Some(field) = &text_field {
                    field.setTextColor(Some(&git_color(status)));
                }
                status.map(|s| s.title().to_string()).unwrap_or_default()
            }
            "tags" => String::new(),
            "folder" => {
                let parent = entry.path.parent().unwrap_or(Path::new("/"));
                match parent.strip_prefix(state.location()) {
                    Ok(inside) if inside.as_os_str().is_empty() => ".".to_string(),
                    Ok(inside) => inside.display().to_string(),
                    Err(_) => parent.display().to_string(),
                }
            }
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
            tabs: RefCell::new(vec![None]),
            tab_selections: RefCell::new(vec![Vec::new()]),
            active_tab: Cell::new(0),
            tab_bar: OnceCell::new(),
            active_bar: OnceCell::new(),
            tab_close: RefCell::new(Vec::new()),
            group: Cell::new(crate::pane::GroupBy::None),
            list_scroll: OnceCell::new(),
            columns: OnceCell::new(),
            icons: OnceCell::new(),
            icon_scroll: OnceCell::new(),
            mode: Cell::new(crate::pane::ViewMode::List),
            from_columns: Cell::new(false),
            git: RefCell::new(crate::fs::git::Statuses::default()),
            git_running: Cell::new(false),
            git_again: Cell::new(false),
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
            ("tags", "Tags", 110.0, NSTextAlignment::Left),
            ("git", "Git", 80.0, NSTextAlignment::Left),
            // Search results: where each item is.
            ("folder", "Folder", 170.0, NSTextAlignment::Left),
        ] {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
            column.setTitle(&ns(title));
            column.setWidth(width);
            column.setMinWidth(if id == "name" { 120.0 } else { 50.0 });
            column.setResizingMask(NSTableColumnResizingOptions::UserResizingMask);
            column.headerCell().setAlignment(align);
            // Size and date sort largest/newest first on the first click.
            let ascending = !matches!(id, "size" | "modified");
            if !matches!(id, "permissions" | "owner" | "group" | "tags" | "folder" | "git") {
                let prototype = NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&ns(id)), ascending);
                column.setSortDescriptorPrototype(Some(&prototype));
            }
            table.addTableColumn(&column);
        }
        table.setColumnAutoresizingStyle(NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle);
        table.setAllowsMultipleSelection(true);
        table.setUsesAlternatingRowBackgroundColors(true);
        table.setStyle(NSTableViewStyle::FullWidth);
        table.setRowHeight(look().row);
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
        table.registerForDraggedTypes(&NSArray::from_retained_slice(&drag_types()));
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
        unsafe {
            filter.setDelegate(Some(ProtocolObject::from_ref(self)));
            filter.setTarget(Some(self));
            filter.setAction(Some(sel!(filterAction:)));
        }
        let path_row = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[&*path_control as &NSView, &*filter as &NSView]),
            mtm,
        );
        path_row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        path_row.setSpacing(8.0);
        let tab_bar = NSStackView::new(mtm);
        tab_bar.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        tab_bar.setSpacing(2.0);
        tab_bar.setDistribution(objc2_app_kit::NSStackViewDistribution::FillEqually);
        tab_bar.setHidden(true);
        container.addArrangedSubview(&tab_bar);
        container.addArrangedSubview(&path_row);
        container.addArrangedSubview(&scroll);
        container.addArrangedSubview(&status);
        let _ = self.ivars().filter.set(filter);
        let _ = self.ivars().tab_bar.set(tab_bar.clone());
        let _ = self.ivars().list_scroll.set(scroll.clone());
        for view in [&*tab_bar as &NSView, &*path_row as &NSView, &scroll, &status] {
            view.leadingAnchor().constraintEqualToAnchor_constant(&container.leadingAnchor(), 6.0).setActive(true);
            view.trailingAnchor().constraintEqualToAnchor_constant(&container.trailingAnchor(), -6.0).setActive(true);
        }

        // In the container's top inset, over the full width of the pane.
        let bar = objc2_app_kit::NSBox::new(mtm);
        bar.setBoxType(objc2_app_kit::NSBoxType::Custom);
        bar.setBorderWidth(0.0);
        bar.setFillColor(&NSColor::controlAccentColor());
        bar.setTranslatesAutoresizingMaskIntoConstraints(false);
        container.addSubview(&bar);
        bar.topAnchor().constraintEqualToAnchor(&container.topAnchor()).setActive(true);
        bar.leadingAnchor().constraintEqualToAnchor(&container.leadingAnchor()).setActive(true);
        bar.trailingAnchor().constraintEqualToAnchor(&container.trailingAnchor()).setActive(true);
        bar.heightAnchor().constraintEqualToConstant(2.0).setActive(true);
        bar.setHidden(!self.ivars().active.get());
        let _ = self.ivars().active_bar.set(bar);

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

    pub fn tab_count(&self) -> usize {
        self.ivars().tabs.borrow().len()
    }

    pub fn active_tab(&self) -> usize {
        self.ivars().active_tab.get()
    }

    /// Every tab's file system and folder, in order.
    pub fn tab_locations(&self) -> Vec<(FsRef, PathBuf)> {
        let active = self.active_tab();
        let tabs = self.ivars().tabs.borrow();
        tabs.iter()
            .enumerate()
            .map(|(i, tab)| match tab {
                Some(state) if i != active => (state.fs().clone(), state.location().to_path_buf()),
                _ => (self.fs(), self.location()),
            })
            .collect()
    }

    /// A new tab after the current one, showing `path`, and switches to it.
    pub fn new_tab(&self, fs: FsRef, path: PathBuf) {
        let (sort, show_hidden) = {
            let state = self.ivars().state.borrow();
            (state.sort, state.show_hidden())
        };
        let index = self.active_tab() + 1;
        self.ivars().tabs.borrow_mut().insert(index, Some(PaneState::new(fs, path, sort, show_hidden)));
        self.ivars().tab_selections.borrow_mut().insert(index, Vec::new());
        // The slot of the current tab moves only if it is after `index`.
        self.select_tab(index);
    }

    /// Adds a tab at the end without showing it (restoring tabs at launch).
    pub fn add_background_tab(&self, state: PaneState) {
        self.ivars().tabs.borrow_mut().push(Some(state));
        self.ivars().tab_selections.borrow_mut().push(Vec::new());
        self.rebuild_tab_bar();
    }

    /// Shows tab `index`: the current tab's state goes back into its slot.
    pub fn select_tab(&self, index: usize) {
        let active = self.active_tab();
        if index == active || index >= self.tab_count() {
            return;
        }
        let selection = self.selected_paths();
        self.ivars().tab_selections.borrow_mut()[active] = selection;
        let Some(incoming) = self.ivars().tabs.borrow_mut()[index].take() else { return };
        let outgoing = std::mem::replace(&mut *self.ivars().state.borrow_mut(), incoming);
        self.ivars().tabs.borrow_mut()[active] = Some(outgoing);
        self.ivars().active_tab.set(index);
        // Up to date, with the hidden-files setting it may have missed.
        let show_hidden = super::current_settings().is_none_or(|s| s.show_hidden);
        {
            let mut state = self.ivars().state.borrow_mut();
            if state.show_hidden() != show_hidden {
                state.set_show_hidden(show_hidden);
            }
            state.load();
        }
        let selection = self.ivars().tab_selections.borrow()[index].clone();
        self.refresh(Some(&selection));
        self.start_loading();
        super::location_changed(self.index());
    }

    /// The next (1) or previous (-1) tab, wrapping around.
    pub fn cycle_tab(&self, step: isize) {
        let count = self.tab_count() as isize;
        if count > 1 {
            self.select_tab((self.active_tab() as isize + step).rem_euclid(count) as usize);
        }
    }

    /// Closes a tab (not the last one). Returns false if it was the last.
    pub fn close_tab(&self, index: usize) -> bool {
        let count = self.tab_count();
        if count <= 1 || index >= count {
            return false;
        }
        if let Some((fs, path)) = self.tab_locations().get(index).cloned() {
            super::remember_closed_tab(self.index(), fs, path);
        }
        if index == self.active_tab() {
            self.select_tab(if index + 1 < count { index + 1 } else { index - 1 });
        }
        self.ivars().tabs.borrow_mut().remove(index);
        self.ivars().tab_selections.borrow_mut().remove(index);
        if self.active_tab() > index {
            self.ivars().active_tab.set(self.active_tab() - 1);
        }
        self.rebuild_tab_bar();
        super::location_changed(self.index());
        true
    }

    /// The tab buttons: one per tab (the active one pressed) and a "+".
    fn rebuild_tab_bar(&self) {
        let Some(bar) = self.ivars().tab_bar.get() else { return };
        let mtm = self.mtm();
        for view in bar.arrangedSubviews().iter() {
            view.removeFromSuperview();
        }
        self.ivars().tab_close.borrow_mut().clear();
        let count = self.tab_count();
        bar.setHidden(count <= 1);
        if count <= 1 {
            return;
        }
        let active = self.active_tab();
        let titles: Vec<(String, bool)> = {
            let tabs = self.ivars().tabs.borrow();
            let state = self.ivars().state.borrow();
            tabs.iter()
                .enumerate()
                .map(|(i, tab)| {
                    let state = if i == active { &*state } else { tab.as_ref().unwrap_or(&*state) };
                    (state.title(), state.is_local())
                })
                .collect()
        };
        for (index, (title, local)) in titles.into_iter().enumerate() {
            let button = unsafe {
                NSButton::buttonWithTitle_target_action(&ns(&title), Some(self), Some(sel!(selectTab:)), mtm)
            };
            button.setButtonType(objc2_app_kit::NSButtonType::PushOnPushOff);
            button.setBezelStyle(objc2_app_kit::NSBezelStyle::Push);
            button.setControlSize(objc2_app_kit::NSControlSize::Small);
            button
                .setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(objc2_app_kit::NSFont::smallSystemFontSize())));
            button.setTag(index as isize);
            button.setState(if index == active {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
            if let Some(cell) = button.cell() {
                cell.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingMiddle);
            }
            if !local && let Some(image) = super::util::symbol("server.rack", &title) {
                button.setImage(Some(&image));
                button.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageLeading);
            }
            button.setToolTip(Some(&ns(&title)));
            let menu = NSMenu::new(mtm);
            let close = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &ns("Close Tab"),
                    Some(sel!(closeTabFromMenu:)),
                    &ns(""),
                )
            };
            close.setTag(index as isize);
            unsafe { close.setTarget(Some(self)) };
            menu.addItem(&close);
            unsafe { button.setMenu(Some(&menu)) };
            button
                .setContentCompressionResistancePriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
            bar.addArrangedSubview(&button);
            self.add_tab_close(&button, index, index == active);
        }
        let add =
            unsafe { NSButton::buttonWithTitle_target_action(&ns(""), Some(self), Some(sel!(newTabButton:)), mtm) };
        if let Some(image) = super::util::symbol("plus", "New Tab") {
            add.setImage(Some(&image));
        }
        add.setBezelStyle(objc2_app_kit::NSBezelStyle::Push);
        add.setControlSize(objc2_app_kit::NSControlSize::Small);
        add.setToolTip(Some(&ns("New Tab")));
        add.widthAnchor().constraintEqualToConstant(28.0).setActive(true);
        bar.setDistribution(objc2_app_kit::NSStackViewDistribution::Fill);
        bar.addArrangedSubview(&add);
        // Tabs share the width equally; the "+" keeps its own.
        let buttons = bar.arrangedSubviews();
        let first = buttons.firstObject();
        if let Some(first) = first {
            for other in buttons.iter().skip(1).take(buttons.count().saturating_sub(2)) {
                other.widthAnchor().constraintEqualToAnchor(&first.widthAnchor()).setActive(true);
            }
        }
    }

    /// A close button at the tab's leading edge (as in Safari), hidden
    /// until the mouse is over the tab.
    fn add_tab_close(&self, tab: &NSButton, index: usize, active: bool) {
        use objc2_app_kit::{NSTrackingArea, NSTrackingAreaOptions as O};
        let mtm = self.mtm();
        let close =
            unsafe { NSButton::buttonWithTitle_target_action(&ns(""), Some(self), Some(sel!(closeTabButton:)), mtm) };
        if let Some(image) = super::util::symbol("xmark", "Close Tab") {
            let config = objc2_app_kit::NSImageSymbolConfiguration::configurationWithPointSize_weight(9.0, unsafe {
                objc2_app_kit::NSFontWeightSemibold
            });
            close.setImage(Some(&image.imageWithSymbolConfiguration(&config).unwrap_or(image)));
        }
        close.setBordered(false);
        // Readable on the active tab's accent color, quiet on the others.
        let tint = if active { NSColor::alternateSelectedControlTextColor() } else { NSColor::secondaryLabelColor() };
        close.setContentTintColor(Some(&tint));
        close.setTag(index as isize);
        close.setToolTip(Some(&ns("Close Tab")));
        close.setHidden(true);
        close.setTranslatesAutoresizingMaskIntoConstraints(false);
        tab.addSubview(&close);
        close.leadingAnchor().constraintEqualToAnchor_constant(&tab.leadingAnchor(), 6.0).setActive(true);
        close.centerYAnchor().constraintEqualToAnchor(&tab.centerYAnchor()).setActive(true);
        close.widthAnchor().constraintEqualToConstant(16.0).setActive(true);
        close.heightAnchor().constraintEqualToConstant(16.0).setActive(true);
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                <NSTrackingArea as objc2::AnyThread>::alloc(),
                NSRect::ZERO,
                O::MouseEnteredAndExited | O::ActiveInActiveApp | O::InVisibleRect,
                Some(self),
                None,
            )
        };
        tab.addTrackingArea(&area);
        self.ivars().tab_close.borrow_mut().push((area, close));
    }

    fn show_tab_close(&self, event: &NSEvent, show: bool) {
        let Some(area) = event.trackingArea() else { return };
        for (tab_area, close) in self.ivars().tab_close.borrow().iter() {
            if std::ptr::eq(&**tab_area, &*area) {
                close.setHidden(!show);
            }
        }
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

    pub fn can_go_up(&self) -> bool {
        self.ivars().state.borrow().can_go_up()
    }

    pub fn can_go_back(&self) -> bool {
        self.ivars().state.borrow().can_go_back()
    }

    pub fn can_go_forward(&self) -> bool {
        self.ivars().state.borrow().can_go_forward()
    }

    pub fn focus(&self) {
        let Some(window) = self.table().window() else { return };
        use crate::pane::ViewMode;
        match self.ivars().mode.get() {
            ViewMode::Columns if let Some(columns) = self.ivars().columns.get() => {
                window.makeFirstResponder(Some(columns))
            }
            ViewMode::Icons if let Some(icons) = self.ivars().icons.get() => window.makeFirstResponder(Some(icons)),
            _ => window.makeFirstResponder(Some(self.table())),
        };
    }

    pub fn column_browser(&self) -> Option<Retained<super::column_view::ColumnBrowser>> {
        self.ivars().columns.get().cloned()
    }

    pub fn icon_view(&self) -> Option<Retained<super::icon_view::IconView>> {
        self.ivars().icons.get().cloned()
    }

    pub fn view_mode(&self) -> crate::pane::ViewMode {
        self.ivars().mode.get()
    }

    pub fn is_column_view(&self) -> bool {
        self.ivars().mode.get() == crate::pane::ViewMode::Columns
    }

    /// View > as List, as Icons, or as Columns.
    pub fn set_view_mode(&self, mode: crate::pane::ViewMode) {
        use crate::pane::ViewMode;
        if mode == self.ivars().mode.get() {
            return;
        }
        let (Some(container), Some(scroll)) = (self.ivars().container.get(), self.ivars().list_scroll.get()) else {
            return;
        };
        let focused = self.has_focus();
        let place = |view: &NSView| {
            container.insertArrangedSubview_atIndex(view, 3);
            view.leadingAnchor().constraintEqualToAnchor_constant(&container.leadingAnchor(), 6.0).setActive(true);
            view.trailingAnchor().constraintEqualToAnchor_constant(&container.trailingAnchor(), -6.0).setActive(true);
            view.setContentHuggingPriority_forOrientation(1.0, NSLayoutConstraintOrientation::Vertical);
            view.setHidden(true);
        };
        if mode == ViewMode::Columns && self.ivars().columns.get().is_none() {
            let columns = super::column_view::ColumnBrowser::new(self.mtm(), self);
            // The list's context menu, for the selection.
            unsafe { columns.setMenu(self.table().menu().as_deref()) };
            place(&columns);
            let _ = self.ivars().columns.set(columns);
        }
        if mode == ViewMode::Icons && self.ivars().icons.get().is_none() {
            let icons = super::icon_view::IconView::new(self.mtm(), self);
            unsafe { icons.setMenu(self.table().menu().as_deref()) };
            let icon_scroll = super::icon_view::scroll_view(&icons, self.mtm());
            place(&icon_scroll);
            let _ = self.ivars().icons.set(icons);
            let _ = self.ivars().icon_scroll.set(icon_scroll);
        }
        self.ivars().mode.set(mode);
        scroll.setHidden(mode != ViewMode::List);
        if let Some(columns) = self.ivars().columns.get() {
            columns.setHidden(mode != ViewMode::Columns);
        }
        if let Some(icon_scroll) = self.ivars().icon_scroll.get() {
            icon_scroll.setHidden(mode != ViewMode::Icons);
        }
        match mode {
            ViewMode::Columns => {
                self.sync_columns();
                // Again once the window shows (at launch it is not on screen yet).
                let index = self.index();
                DispatchQueue::main().exec_async(move || super::sync_pane_columns(index));
            }
            ViewMode::Icons => self.sync_icons(),
            ViewMode::List => {}
        }
        if focused {
            self.focus();
        }
    }

    /// Whether the pane's list, columns, or icons have the keyboard focus.
    fn has_focus(&self) -> bool {
        let Some(responder) = self.table().window().and_then(|w| w.firstResponder()) else { return false };
        let ptr = Retained::as_ptr(&responder).cast::<AnyObject>();
        std::ptr::eq(ptr, (self.table() as &FileTable as *const FileTable).cast())
            || self.ivars().columns.get().is_some_and(|c| std::ptr::eq(ptr, Retained::as_ptr(c).cast()))
            || self.ivars().icons.get().is_some_and(|i| std::ptr::eq(ptr, Retained::as_ptr(i).cast()))
    }

    /// Shows the pane's items and selection in the icon view.
    pub fn sync_icons(&self) {
        if self.ivars().mode.get() != crate::pane::ViewMode::Icons {
            return;
        }
        let Some(icons) = self.ivars().icons.get() else { return };
        let selected = self.selected_paths();
        let (entries, local) = {
            let state = self.ivars().state.borrow();
            (state.entries().to_vec(), state.is_local())
        };
        icons.sync(entries, local, &selected);
    }

    /// What a dragged item puts on the pasteboard: its file URL, or from a
    /// server a file promise (downloaded where it is dropped).
    pub fn drag_writer(&self, entry: &crate::fs::Entry) -> Retained<ProtocolObject<dyn NSPasteboardWriting>> {
        let fs = self.fs();
        if fs.is_local() {
            ProtocolObject::from_retained(file_url(&entry.path))
        } else {
            ProtocolObject::from_retained(super::promises::provider(fs, entry))
        }
    }

    /// Remembers dragged items, for drops on panes.
    pub fn remember_drag(&self, paths: Vec<PathBuf>) {
        let fs = self.fs();
        DRAG.with(|d| *d.borrow_mut() = Some((fs, paths)));
    }

    /// Shows the pane's folder and selection in the column view.
    pub fn sync_columns(&self) {
        let Some(columns) = self.ivars().columns.get().filter(|_| self.is_column_view()) else { return };
        let selected = self.selected_paths();
        let (fs, location, entries) = {
            let state = self.ivars().state.borrow();
            // While a server listing is on its way, the column keeps its own.
            let entries = (!state.loading).then(|| {
                let mut list: Vec<crate::fs::Entry> = state.entries().to_vec();
                crate::fs::sort_entries(&mut list, state.sort);
                list
            });
            (state.fs().clone(), state.location().to_path_buf(), entries)
        };
        columns.sync(&fs, &location, entries, &selected);
    }

    /// The column view chose items: their folder becomes the pane's (no
    /// history: the columns show the way), and they its selection.
    pub fn browse_select(&self, folder: PathBuf, paths: Vec<PathBuf>) {
        self.ivars().from_columns.set(true);
        let moved = self.ivars().state.borrow().location() != folder;
        if moved {
            self.ivars().state.borrow_mut().browse(folder);
        }
        self.refresh(Some(&paths));
        if moved {
            self.start_loading();
            super::location_changed(self.index());
        }
        self.ivars().from_columns.set(false);
    }

    /// Quick Look from the column view: the hidden list provides items.
    pub fn hand_quick_look(&self, panel: &QLPreviewPanel, begin: bool) {
        unsafe {
            if begin {
                panel.setDataSource(Some(ProtocolObject::from_ref(self.table())));
                panel.setDelegate(Some(self.table().as_ref()));
            } else {
                panel.setDataSource(None);
                panel.setDelegate(None);
            }
        }
    }

    pub fn set_active(&self, active: bool) {
        self.ivars().active.set(active);
        if let Some(bar) = self.ivars().active_bar.get() {
            bar.setHidden(!active);
        }
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
                if let Some(row) = state.table_row_of(path) {
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
        self.refresh_from(select, None);
    }

    /// The first selected row (selectedRow is the last one), or -1.
    fn first_selected_row(&self) -> NSInteger {
        let rows = self.table().selectedRowIndexes();
        if rows.count() == 0 { -1 } else { rows.firstIndex() as NSInteger }
    }

    /// `refresh`, with the selection as it was before the listing changed
    /// (paths and first row), if it did.
    fn refresh_from(&self, select: Option<&[PathBuf]>, previous: Option<(Vec<PathBuf>, NSInteger)>) {
        let (keep, first_row) = match previous {
            Some((paths, row)) => (if select.is_none() { paths } else { Vec::new() }, row),
            None => (if select.is_none() { self.selected_paths() } else { Vec::new() }, self.first_selected_row()),
        };
        // Another tab may show (each keeps the pane's grouping).
        self.ivars().state.borrow_mut().set_group_by(self.ivars().group.get(), kind_of);
        let (exclusions, version) = super::listing_exclusions();
        self.ivars().state.borrow_mut().set_exclusions(exclusions, version);
        self.table().reloadData();
        match select {
            Some(paths) if !paths.is_empty() => self.select_paths(paths),
            Some(_) => {
                // A new folder starts with its first item selected, ready
                // for the keyboard.
                let first = {
                    let state = self.ivars().state.borrow();
                    let parent = is_parent_row(&state, 0);
                    match item_rows(&state).first() {
                        _ if parent && super::start_on_parent() => NSIndexSet::indexSetWithIndex(0),
                        Some(first) => NSIndexSet::indexSetWithIndex(*first),
                        None if parent => NSIndexSet::indexSetWithIndex(0),
                        None => NSIndexSet::new(),
                    }
                };
                self.table().selectRowIndexes_byExtendingSelection(&first, false);
                self.table().scrollRowToVisible(0);
            }
            None => {
                self.select_paths(&keep);
                // The selected items are gone (moved or deleted): select
                // what took their place, like Finder.
                let count = self.ivars().state.borrow().rows().len() as NSInteger;
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
        self.rebuild_tab_bar();
        self.sync_filter_field();
        self.update_path_bar();
        self.update_status();
        if !self.ivars().from_columns.get() {
            self.sync_columns();
            self.sync_icons();
        }
        self.update_git();
        // Go > Recent Folders (once the folder could be listed).
        let listed = {
            let state = self.ivars().state.borrow();
            !state.loading && state.error.is_none()
        };
        if listed {
            super::note_recent_folder(&self.fs(), &self.location());
        }
    }

    /// Runs `git status` in the background for the Git column (when it
    /// shows, on the Mac); one at a time, again if asked meanwhile.
    fn update_git(&self) {
        let shown = self.is_local() && super::visible_columns(true).iter().any(|c| c == "git");
        if !shown {
            return;
        }
        if self.ivars().git_running.replace(true) {
            self.ivars().git_again.set(true);
            return;
        }
        let (location, index) = (self.location(), self.index());
        std::thread::spawn(move || {
            let statuses = crate::fs::git::repository_root(&location)
                .and_then(|root| crate::fs::git::status(&root).ok())
                .unwrap_or_default();
            DispatchQueue::main().exec_async(move || super::git_status_arrived(index, statuses));
        });
    }

    /// `git status` finished: the Git column shows it.
    pub fn git_arrived(&self, statuses: crate::fs::git::Statuses) {
        *self.ivars().git.borrow_mut() = statuses;
        self.ivars().git_running.set(false);
        let table = self.table();
        let column = table.columnWithIdentifier(&ns("git"));
        if column >= 0 {
            let rows = NSIndexSet::indexSetWithIndexesInRange(objc2_foundation::NSRange::new(
                0,
                table.numberOfRows() as usize,
            ));
            table.reloadDataForRowIndexes_columnIndexes(&rows, &NSIndexSet::indexSetWithIndex(column as usize));
        }
        if self.ivars().git_again.replace(false) {
            self.update_git();
        }
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
        // The icon and column views show the filtered items too.
        self.sync_columns();
        self.sync_icons();
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
        if let Some(column) = self.table().tableColumnWithIdentifier(&ns("folder")) {
            let hidden = !self.showing_results();
            if column.isHidden() != hidden {
                column.setHidden(hidden);
                changed = true;
            }
        }
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
            self.update_git();
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

    /// The names of everything in the folder (hidden items too).
    pub fn all_names(&self) -> Vec<String> {
        self.ivars().state.borrow().all_entries().iter().map(|e| e.name.clone()).collect()
    }

    /// The entries a command applies to (see `target_paths`).
    pub fn target_entries(&self) -> Vec<crate::fs::Entry> {
        self.entries_for(&self.target_paths())
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
                let rows: Vec<usize> = index_set_rows(&self.table().selectedRowIndexes())
                    .into_iter()
                    .filter_map(|r| match state.rows().get(r) {
                        Some(crate::pane::Row::Entry(index)) => Some(*index),
                        _ => None,
                    })
                    .collect();
                let mut text = selection_summary(state.entries(), &rows);
                if !state.filter_text().is_empty() {
                    text.push_str(&format!(" (filtered from {})", state.unfiltered_count()));
                }
                match state.filtered_out() {
                    0 => {}
                    1 => text.push_str(", 1 hidden by filters"),
                    n => text.push_str(&format!(", {n} hidden by filters")),
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
        let active = self.ivars().active.get();
        let color = match super::themed::colors() {
            Some(c) if active => c.text,
            Some(c) => c.secondary,
            None if active => NSColor::labelColor(),
            None => NSColor::tertiaryLabelColor(),
        };
        status.setTextColor(Some(&color));
        drop(state);
        super::selection_changed(self.index());
    }

    /// Applies the look set with `set_look`, and alternating row colors.
    pub fn apply_appearance(&self, alternating: bool) {
        let table = self.table();
        table.setRowHeight(look().row);
        match super::themed::colors() {
            // The rows draw the theme's stripes.
            Some(colors) => {
                table.setUsesAlternatingRowBackgroundColors(false);
                table.setBackgroundColor(&colors.background);
            }
            None => {
                table.setUsesAlternatingRowBackgroundColors(alternating);
                table.setBackgroundColor(&NSColor::controlBackgroundColor());
            }
        }
        if let Some(bar) = self.ivars().active_bar.get() {
            bar.setFillColor(&super::themed::colors().map_or_else(NSColor::controlAccentColor, |c| c.accent));
        }
        self.update_status();
        let selected = self.selected_paths();
        table.reloadData();
        self.select_paths(&selected);
        if let Some(columns) = self.ivars().columns.get() {
            columns.forget_listings();
            columns.apply_look();
            self.sync_columns();
        }
        if let Some(icons) = self.ivars().icons.get() {
            icons.apply_look();
            self.sync_icons();
        }
    }

    /// The filename filters changed: the list (and columns) again.
    pub fn filters_changed(&self) {
        if let Some(columns) = self.ivars().columns.get() {
            columns.forget_listings();
        }
        self.refresh(None);
    }

    pub fn group_by(&self) -> crate::pane::GroupBy {
        self.ivars().group.get()
    }

    /// Groups the list (and the pane's other tabs, when shown).
    pub fn set_group_by(&self, group: crate::pane::GroupBy) {
        self.ivars().group.set(group);
        let selected = self.selected_paths();
        self.ivars().state.borrow_mut().set_group_by(group, kind_of);
        self.table().reloadData();
        self.select_paths(&selected);
        self.update_status();
    }

    pub fn sort(&self) -> Sort {
        self.ivars().state.borrow().sort
    }

    /// Whether the pane shows search results.
    pub fn showing_results(&self) -> bool {
        let state = self.ivars().state.borrow();
        state.fs().title_for(state.location()).is_some()
    }

    /// Selects the items whose names match; returns how many.
    /// Selects the items whose names match: only them (`add` false,
    /// `remove` false), adding them to the selection, or removing them from
    /// it. Returns how many matched.
    pub fn select_matching(&self, patterns: &crate::fs::glob::Patterns, add: bool, remove: bool) -> usize {
        let rows: Vec<usize> = {
            let state = self.ivars().state.borrow();
            item_rows(&state)
                .into_iter()
                .filter(|&row| state.entry_at_row(row).is_some_and(|e| patterns.matches(&e.name)))
                .collect()
        };
        let set = NSMutableIndexSet::new();
        if add || remove {
            for row in index_set_rows(&self.table().selectedRowIndexes()) {
                set.addIndex(row);
            }
        }
        for row in &rows {
            if remove {
                set.removeIndex(*row);
            } else {
                set.addIndex(*row);
            }
        }
        self.table().selectRowIndexes_byExtendingSelection(&set, false);
        if let (Some(first), false) = (rows.first(), remove) {
            self.table().scrollRowToVisible(*first as NSInteger);
        }
        rows.len()
    }

    /// The items the folder shows (without "..").
    pub fn items(&self) -> Vec<crate::fs::Entry> {
        self.ivars().state.borrow().entries().to_vec()
    }

    /// How many items the folder shows.
    pub fn item_count(&self) -> usize {
        self.ivars().state.borrow().entries().len()
    }

    /// Reads the folder again, keeping the selection.
    pub fn reload(&self) {
        // The column view lists every column again (a drop or change may
        // be in any of them).
        if let Some(columns) = self.ivars().columns.get() {
            columns.forget_listings();
        }
        // The selection by path, before the listing changes under it (its
        // rows would then be other items).
        let previous = (self.selected_paths(), self.first_selected_row());
        self.ivars().state.borrow_mut().load();
        self.refresh_from(None, Some(previous));
        self.start_loading();
    }

    /// Reloads and selects `paths` (for example what a job created).
    pub fn reload_selecting(&self, paths: &[PathBuf]) {
        if let Some(columns) = self.ivars().columns.get() {
            columns.forget_listings();
        }
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
        // Tabs not shown that are on that server go back to the Mac.
        for state in self.ivars().tabs.borrow_mut().iter_mut().flatten() {
            if state.fs().key() == key {
                let local = vfs::local();
                let home = local.home();
                state.navigate_to(local, home);
            }
            state.forget(key);
        }
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
        if let Some(columns) = self.ivars().columns.get() {
            columns.forget_listings();
        }
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
        // A server in Network: connect to it.
        if let [one] = paths.as_slice()
            && let Some(address) = self.fs().link_target(one)
        {
            super::connect_address(&address);
            return;
        }
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
        // An archive opens like a folder.
        if let [one] = paths.as_slice()
            && vfs::archive::is_archive_name(&crate::pane::title_of(one))
        {
            super::open_archive(self.index(), self.fs(), one.clone(), None);
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
    pub fn drop_operation(&self, info: &ProtocolObject<dyn NSDraggingInfo>, target: &Path) -> NSDragOperation {
        let Some((source_fs, sources)) = self.drag_sources(info) else { return NSDragOperation::None };
        let target_fs = self.fs();
        if target_fs.read_only() {
            return NSDragOperation::None;
        }
        let same_fs = vfs::same(&source_fs, &target_fs);
        if sources.is_empty() || (same_fs && sources.iter().any(|s| s == target)) {
            return NSDragOperation::None;
        }
        // Dropping items back into the folder they are in does nothing.
        if same_fs && sources.iter().all(|s| s.parent() == Some(target)) {
            return NSDragOperation::None;
        }
        let mask = info.draggingSourceOperationMask();
        // Nothing can be moved out of an archive, only copied.
        if source_fs.read_only() {
            return if mask.intersects(NSDragOperation::Copy | NSDragOperation::Generic | NSDragOperation::Move) {
                NSDragOperation::Copy
            } else {
                NSDragOperation::None
            };
        }
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

/// The command a key runs in a file list (or column view); None for
/// keys the view handles itself (arrows, type-to-select).
pub fn key_command(event: &NSEvent) -> Option<Command> {
    let flags = event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
    let plain = (flags & !(NSEventModifierFlags::Function | NSEventModifierFlags::NumericPad)).is_empty();
    let shift = flags.contains(NSEventModifierFlags::Shift);
    match event.keyCode() {
        30 if flags.contains(NSEventModifierFlags::Command) && shift => Some(Command::NextTab), // Shift-Command-]
        33 if flags.contains(NSEventModifierFlags::Command) && shift => Some(Command::PreviousTab), // Shift-Command-[
        48 if plain => Some(Command::SwitchPane),                                               // Tab
        36 | 76 if plain => Some(Command::Rename),                                              // Return, Enter
        49 if plain => Some(Command::QuickLook),                                                // Space
        125 if flags.contains(NSEventModifierFlags::Command) => Some(Command::Open),            // Command-Down
        120 => Some(Command::Rename),                                                           // F2
        99 => Some(Command::ViewFile),                                                          // F3
        96 => Some(Command::CopyToOther),                                                       // F5
        97 => Some(Command::MoveToOther),                                                       // F6
        98 => Some(Command::NewFolder),                                                         // F7
        100 => Some(Command::Trash),                                                            // F8
        _ => None,
    }
}

/// Whether a table row is "..", the enclosing folder (first in every
/// folder but the top one).
fn is_parent_row(state: &PaneState, row: usize) -> bool {
    state.rows().get(row) == Some(&crate::pane::Row::Parent)
}

/// Whether a table row is a group's title (View > Group By).
fn is_header_row(state: &PaneState, row: usize) -> bool {
    matches!(state.rows().get(row), Some(crate::pane::Row::Header(_)))
}

/// The entry shown in a table row; None for ".." and group titles.
fn entry_at(state: &PaneState, row: usize) -> Option<&crate::fs::Entry> {
    state.entry_at_row(row)
}

/// The Git column's text color for a status.
fn git_color(status: Option<crate::fs::git::Status>) -> Retained<NSColor> {
    use crate::fs::git::Status;
    match status {
        Some(Status::Modified) => NSColor::systemOrangeColor(),
        Some(Status::Added | Status::Untracked) => NSColor::systemGreenColor(),
        Some(Status::Deleted | Status::Conflicted) => NSColor::systemRedColor(),
        Some(Status::Renamed) => NSColor::systemBlueColor(),
        Some(Status::Ignored) | None => NSColor::tertiaryLabelColor(),
    }
}

/// A group title's view: small, bold, secondary text.
fn group_title(title: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    let look = look();
    let field = NSTextField::labelWithString(&ns(title), mtm);
    field.setFont(Some(&objc2_app_kit::NSFont::boldSystemFontOfSize((look.text - 1.0).max(10.0))));
    let color = super::themed::colors().map_or_else(NSColor::secondaryLabelColor, |c| c.secondary);
    field.setTextColor(Some(&color));
    let cell = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), NSRect::ZERO);
    let height = (look.text * 1.25).ceil() + 1.0;
    field.setFrame(NSRect::new(NSPoint::new(6.0, ((look.row - height) / 2.0).floor()), NSSize::new(600.0, height)));
    field.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
    cell.addSubview(&field);
    Retained::into_super(cell)
}

/// The rows that are items (not ".." or group titles).
fn item_rows(state: &PaneState) -> Vec<usize> {
    (0..state.rows().len()).filter(|&r| state.entry_at_row(r).is_some()).collect()
}

/// File URLs on a pasteboard (from a drag, or Edit > Copy in Finder).
/// What the file views accept dropped: files, items from panes, and file
/// promises.
pub fn drag_types() -> Vec<Retained<NSString>> {
    let mut types: Vec<Retained<NSString>> = vec![unsafe { NSPasteboardTypeFileURL }.retain(), ns(ITEMS_TYPE)];
    types.extend(objc2_app_kit::NSFilePromiseReceiver::readableDraggedTypes().iter());
    types
}

/// Where dragged items come from: a pane of this app (any file system) or
/// Finder (local files).
pub fn dragged_items(info: &ProtocolObject<dyn NSDraggingInfo>) -> Option<(FsRef, Vec<PathBuf>)> {
    let pasteboard = info.draggingPasteboard();
    let types = pasteboard.types().map(|t| t.iter().map(|t| t.to_string()).collect::<Vec<_>>()).unwrap_or_default();
    // From a pane of this app (its list, columns, or icons): what DRAG says
    // (server items are file promises on the pasteboard, which panes do
    // not need).
    let from_pane = info.draggingSource().is_some_and(|source| {
        source.downcast_ref::<FileTable>().is_some()
            || source.downcast_ref::<super::column_view::ColumnBrowser>().is_some()
            || source.downcast_ref::<super::icon_view::IconView>().is_some()
    });
    if from_pane || types.iter().any(|t| t == ITEMS_TYPE) {
        return DRAG.with(|d| d.borrow().clone());
    }
    let paths = paths_on_pasteboard(&pasteboard);
    if paths.is_empty() { None } else { Some((vfs::local(), paths)) }
}

pub fn paths_on_pasteboard(pasteboard: &objc2_app_kit::NSPasteboard) -> Vec<PathBuf> {
    let Some(items) = pasteboard.pasteboardItems() else { return Vec::new() };
    items
        .iter()
        .filter_map(|item| item.stringForType(unsafe { NSPasteboardTypeFileURL }))
        .filter_map(|s| objc2_foundation::NSURL::URLWithString(&s))
        .filter_map(|url| url_path(&url))
        .collect()
}

/// The indexes in a set, in order.
pub fn index_rows(set: &NSIndexSet) -> Vec<usize> {
    index_set_rows(set)
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
    let look = look();
    let identifier = ns(&format!("{column_id}-{}-{}", look.version, super::themed::version()));
    let existing: Option<Retained<NSView>> = unsafe { table.makeViewWithIdentifier_owner(&identifier, None) };
    if let Some(cell) = existing.and_then(|v| v.downcast::<NSTableCellView>().ok()) {
        return cell;
    }
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, look.row));
    let secondary = matches!(column_id, "size" | "modified" | "kind" | "permissions" | "owner" | "group");
    let themed = super::themed::ThemedCell::new(mtm, frame, secondary);
    let cell: Retained<NSTableCellView> = Retained::into_super(themed.clone());
    cell.setIdentifier(Some(&identifier));
    let icon = (look.row - 6.0).clamp(16.0, 32.0).floor();
    let text_x = if column_id == "name" { icon + 8.0 } else { 2.0 };
    let field = NSTextField::labelWithString(&ns(""), mtm);
    field.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(look.text)));
    field.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingMiddle);
    let text_height = (look.text * 1.25).ceil() + 1.0;
    field.setFrame(NSRect::new(
        NSPoint::new(text_x, ((look.row - text_height) / 2.0).floor()),
        NSSize::new(200.0 - text_x - 2.0, text_height),
    ));
    field.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
    if column_id == "size" {
        field.setAlignment(NSTextAlignment::Right);
    }
    if secondary {
        field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    if column_id == "permissions" {
        let size = look.text - 2.0;
        field.setFont(Some(&objc2_app_kit::NSFont::monospacedSystemFontOfSize_weight(size, unsafe {
            objc2_app_kit::NSFontWeightRegular
        })));
    }
    cell.addSubview(&field);
    unsafe { cell.setTextField(Some(&field)) };
    themed.recolor(false);
    if column_id == "name" {
        let image = NSImageView::initWithFrame(
            NSImageView::alloc(mtm),
            NSRect::new(NSPoint::new(3.0, ((look.row - icon) / 2.0).floor()), NSSize::new(icon, icon)),
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
