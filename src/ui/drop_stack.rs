//! The drop stack (Window > Show Drop Stack): a small floating shelf to
//! gather files from several folders (or from Finder), then drag them out
//! together, or copy or move them to the active pane's folder. It keeps
//! where the files are (not copies), across launches.

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSControlTextEditingDelegate, NSDragOperation, NSDraggingInfo,
    NSDraggingSession, NSFont, NSLineBreakMode, NSPanel, NSPasteboardTypeFileURL, NSPasteboardWriting, NSScrollView,
    NSStackView, NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTableViewDropOperation,
    NSTextField, NSUserInterfaceLayoutOrientation, NSView, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSIndexSet, NSInteger, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
};

use super::util::{file_url, mtm, ns, small_label};
use crate::fs::ops::{Job, JobKind};
use crate::pane::title_of;
use crate::vfs;

thread_local! {
    static STACK: OnceCell<Retained<DropStack>> = const { OnceCell::new() };
}

struct Parts {
    panel: Retained<NSPanel>,
    table: Retained<NSTableView>,
    status: Retained<NSTextField>,
}

pub struct DropStackIvars {
    parts: OnceCell<Parts>,
    items: RefCell<Vec<PathBuf>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzDropStack"]
    #[ivars = DropStackIvars]
    struct DropStack;

    impl DropStack {
        #[unsafe(method(copyToPane:))]
        fn copy_to_pane(&self, _sender: Option<&AnyObject>) {
            self.send(JobKind::Copy);
        }

        #[unsafe(method(moveToPane:))]
        fn move_to_pane(&self, _sender: Option<&AnyObject>) {
            self.send(JobKind::Move);
        }

        #[unsafe(method(removeSelected:))]
        fn remove_selected(&self, _sender: Option<&AnyObject>) {
            let chosen = self.chosen();
            self.ivars().items.borrow_mut().retain(|p| !chosen.contains(p));
            self.save_and_reload();
        }

        #[unsafe(method(clearAll:))]
        fn clear_all(&self, _sender: Option<&AnyObject>) {
            self.ivars().items.borrow_mut().clear();
            self.save_and_reload();
        }

        /// Double-click: the item, selected in the active pane.
        #[unsafe(method(showItem:))]
        fn show_item(&self, _sender: Option<&AnyObject>) {
            let row = self.parts().table.clickedRow();
            let item = usize::try_from(row).ok().and_then(|r| self.ivars().items.borrow().get(r).cloned());
            if let Some(path) = item
                && let Some(folder) = path.parent()
            {
                super::show_in_active_pane(folder.to_path_buf(), &path);
            }
        }
    }

    unsafe impl NSObjectProtocol for DropStack {}

    unsafe impl NSControlTextEditingDelegate for DropStack {}

    unsafe impl NSWindowDelegate for DropStack {}

    unsafe impl NSTableViewDataSource for DropStack {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().items.borrow().len() as NSInteger
        }

        /// Items dragged out are the files themselves.
        #[unsafe(method_id(tableView:pasteboardWriterForRow:))]
        fn pasteboard_writer(
            &self,
            _table: &NSTableView,
            row: NSInteger,
        ) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
            let path = usize::try_from(row).ok().and_then(|r| self.ivars().items.borrow().get(r).cloned());
            path.map(|p| ProtocolObject::from_retained(file_url(&p)))
        }

        /// Files moved away (dragged to another folder) leave the stack.
        #[unsafe(method(tableView:draggingSession:endedAtPoint:operation:))]
        fn drag_ended(
            &self,
            _table: &NSTableView,
            _session: &NSDraggingSession,
            _point: NSPoint,
            _operation: NSDragOperation,
        ) {
            // The move may still be running: look a little later too.
            self.prune();
            std::thread::spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(2));
                dispatch2::DispatchQueue::main().exec_async(|| {
                    STACK.with(|s| {
                        if let Some(stack) = s.get() {
                            stack.prune();
                        }
                    })
                });
            });
        }

        #[unsafe(method(tableView:validateDrop:proposedRow:proposedDropOperation:))]
        fn validate_drop(
            &self,
            table: &NSTableView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            _row: NSInteger,
            _operation: NSTableViewDropOperation,
        ) -> NSDragOperation {
            if dropped_paths(info).is_empty() {
                NSDragOperation::None
            } else {
                // Onto the whole list (the files are not moved).
                table.setDropRow_dropOperation(-1, NSTableViewDropOperation::On);
                NSDragOperation::Copy
            }
        }

        #[unsafe(method(tableView:acceptDrop:row:dropOperation:))]
        fn accept_drop(
            &self,
            _table: &NSTableView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            _row: NSInteger,
            _operation: NSTableViewDropOperation,
        ) -> bool {
            self.add(dropped_paths(info));
            true
        }
    }

    unsafe impl NSTableViewDelegate for DropStack {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            usize::try_from(row).ok().and_then(|r| self.cell(table, r))
        }
    }
);

/// Files on the dragging pasteboard (on this Mac).
fn dropped_paths(info: &ProtocolObject<dyn NSDraggingInfo>) -> Vec<PathBuf> {
    super::pane_view::paths_on_pasteboard(&info.draggingPasteboard())
}

/// Shows the drop stack, or hides it if it shows.
pub fn toggle() {
    let stack = STACK.with(|s| s.get_or_init(|| DropStack::new(mtm())).clone());
    let panel = &stack.parts().panel;
    if panel.isVisible() {
        panel.orderOut(None);
    } else {
        stack.prune();
        panel.orderFront(None);
    }
}

/// Whether the drop stack shows (for the menu's check mark).
pub fn is_shown() -> bool {
    STACK.with(|s| s.get().is_some_and(|stack| stack.parts().panel.isVisible()))
}

impl DropStack {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let saved = super::current_settings().map(|s| s.drop_stack).unwrap_or_default();
        let this = Self::alloc(mtm).set_ivars(DropStackIvars { parts: OnceCell::new(), items: RefCell::new(saved) });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build(mtm);
        this
    }

    fn parts(&self) -> &Parts {
        self.ivars().parts.get().expect("built")
    }

    fn build(&self, mtm: MainThreadMarker) {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 420.0)),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::UtilityWindow,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(&ns("Drop Stack"));
        panel.setFloatingPanel(true);
        // Stays up while working in Finder too.
        panel.setHidesOnDeactivate(false);
        panel.setMinSize(NSSize::new(240.0, 220.0));
        panel.setDelegate(Some(ProtocolObject::from_ref(self)));

        let table = NSTableView::new(mtm);
        let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns("item"));
        column.setWidth(300.0);
        table.addTableColumn(&column);
        table.setHeaderView(None);
        table.setRowHeight(26.0);
        table.setAllowsMultipleSelection(true);
        table.setStyle(objc2_app_kit::NSTableViewStyle::Plain);
        table.setUsesAlternatingRowBackgroundColors(true);
        table.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]));
        for local in [false, true] {
            table.setDraggingSourceOperationMask_forLocal(
                NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Generic,
                local,
            );
        }
        let target: &AnyObject = self.as_ref();
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
            table.setTarget(Some(target));
            table.setDoubleAction(Some(sel!(showItem:)));
        }
        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);

        let button = |title: &str, action| unsafe {
            NSButton::buttonWithTitle_target_action(&ns(title), Some(target), Some(action), mtm)
        };
        let row = |buttons: &[Retained<NSButton>]| {
            let views: Vec<&NSView> = buttons.iter().map(|b| &**b as &NSView).collect();
            let stack = NSStackView::stackViewWithViews(&NSArray::from_slice(&views), mtm);
            stack.setSpacing(6.0);
            stack
        };
        let to_pane = row(&[button("Copy to Pane", sel!(copyToPane:)), button("Move to Pane", sel!(moveToPane:))]);
        let tidy = row(&[button("Remove", sel!(removeSelected:)), button("Clear", sel!(clearAll:))]);
        let status = small_label("", mtm);
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        status.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
        status.setPreferredMaxLayoutWidth(280.0);

        let content = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[&*scroll as &NSView, &*status as &NSView, &*to_pane as &NSView, &*tidy as &NSView]),
            mtm,
        );
        content.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        content.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
        content.setSpacing(8.0);
        content.setEdgeInsets(NSEdgeInsets { top: 8.0, left: 10.0, bottom: 10.0, right: 10.0 });
        scroll.leadingAnchor().constraintEqualToAnchor_constant(&content.leadingAnchor(), 10.0).setActive(true);
        scroll.trailingAnchor().constraintEqualToAnchor_constant(&content.trailingAnchor(), -10.0).setActive(true);
        scroll.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Vertical);
        panel.setContentView(Some(&content));
        panel.center();
        panel.setFrameAutosaveName(&ns("ForkzDropStack"));
        let _ = self.ivars().parts.set(Parts { panel, table, status });
        self.reload();
    }

    /// Adds files (each once).
    fn add(&self, paths: Vec<PathBuf>) {
        {
            let mut items = self.ivars().items.borrow_mut();
            for path in paths {
                if !items.contains(&path) {
                    items.push(path);
                }
            }
        }
        self.save_and_reload();
    }

    /// Files that are gone (moved or deleted) leave the stack.
    fn prune(&self) {
        let before = self.ivars().items.borrow().len();
        self.ivars().items.borrow_mut().retain(|p| p.symlink_metadata().is_ok());
        if self.ivars().items.borrow().len() != before {
            self.save_and_reload();
        } else {
            self.reload();
        }
    }

    fn save_and_reload(&self) {
        let items = self.ivars().items.borrow().clone();
        super::update_settings(|s| s.drop_stack = items);
        self.reload();
    }

    fn reload(&self) {
        let parts = self.parts();
        parts.table.reloadData();
        let items = self.ivars().items.borrow();
        let text = if items.is_empty() {
            "Drop files here to gather them. Then drag them out together, or copy or move them to the active pane\u{2019}s folder.".to_string()
        } else {
            let size: u64 =
                items.iter().filter_map(|p| p.metadata().ok()).filter(|m| m.is_file()).map(|m| m.len()).sum();
            let count = if items.len() == 1 { "1 item".to_string() } else { format!("{} items", items.len()) };
            if size > 0 { format!("{count}, {}", crate::fs::format_size(size)) } else { count }
        };
        parts.status.setStringValue(&ns(&text));
    }

    /// The selected items, or all of them if none are selected.
    fn chosen(&self) -> Vec<PathBuf> {
        let items = self.ivars().items.borrow();
        let rows = self.parts().table.selectedRowIndexes();
        if rows.count() == 0 {
            return items.clone();
        }
        let mut chosen = Vec::new();
        let mut index = rows.firstIndex();
        while index != objc2_foundation::NSNotFound as usize {
            if let Some(path) = items.get(index) {
                chosen.push(path.clone());
            }
            index = rows.indexGreaterThanIndex(index);
        }
        chosen
    }

    /// Copies or moves the chosen items to the active pane's folder (on
    /// this Mac or a server). Moved items leave the stack.
    fn send(&self, kind: JobKind) {
        let chosen = self.chosen();
        if chosen.is_empty() {
            objc2_app_kit::NSBeep();
            return;
        }
        let Some(pane) = super::active_pane() else { return };
        let fs = pane.fs();
        if fs.read_only() || pane.showing_results() {
            super::dialogs::show_error(
                "The active pane\u{2019}s folder cannot take items.".into(),
                "Show a folder you can change in the active pane first.".into(),
            );
            return;
        }
        super::start_job(Job::between(kind, vfs::local(), chosen.clone(), fs, pane.location()));
        if kind == JobKind::Move {
            self.ivars().items.borrow_mut().retain(|p| !chosen.contains(p));
            self.save_and_reload();
        }
        self.parts().table.selectRowIndexes_byExtendingSelection(&NSIndexSet::new(), false);
    }

    fn cell(&self, table: &NSTableView, row: usize) -> Option<Retained<NSView>> {
        let path = self.ivars().items.borrow().get(row)?.clone();
        let font = NSFont::systemFontOfSize(NSFont::systemFontSize());
        let field = super::diff_window::text_cell(table, "item", &font);
        let folder = path.parent().map(title_of).unwrap_or_default();
        field.setAttributedStringValue(&label(&path, &font));
        field.setToolTip(Some(&ns(&format!("{}\nin {folder}", path.display()))));
        Some(Retained::into_super(Retained::into_super(field)))
    }
}

/// The file's icon and name.
fn label(path: &std::path::Path, font: &NSFont) -> Retained<objc2_foundation::NSMutableAttributedString> {
    use objc2_app_kit::{NSAttributedStringAttachmentConveniences, NSTextAttachment};
    use objc2_foundation::{NSAttributedString, NSMutableAttributedString, NSRange};
    let icon = super::util::icon_for_path(path);
    let copy: Retained<objc2_app_kit::NSImage> = unsafe { msg_send![&icon, copy] };
    let side = 18.0;
    copy.setSize(NSSize::new(side, side));
    let attachment = NSTextAttachment::new();
    attachment.setImage(Some(&copy));
    attachment
        .setBounds(NSRect::new(NSPoint::new(0.0, ((font.capHeight() - side) / 2.0).round()), NSSize::new(side, side)));
    let text = NSMutableAttributedString::new();
    text.appendAttributedString(&NSAttributedString::attributedStringWithAttachment(&attachment));
    let name = NSMutableAttributedString::new();
    name.replaceCharactersInRange_withString(NSRange::new(0, 0), &ns(&format!("  {}", title_of(path))));
    text.appendAttributedString(&name);
    let all = NSRange::new(0, text.length());
    unsafe { text.addAttribute_value_range(objc2_app_kit::NSFontAttributeName, font, all) };
    text
}
