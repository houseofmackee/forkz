//! The Sync window: compares the two panes' folders, shows what would be
//! copied or deleted (each item can be left out), and returns the plan.
//! The options can be saved as a synclet.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::PathBuf;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSFont, NSLayoutAttribute, NSLineBreakMode, NSPopUpButton, NSScrollView, NSStackView,
    NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextField, NSTextFieldDelegate,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use super::util::{format_date, mtm, ns};
use crate::fs::ops::CancelToken;
use crate::fs::sync::{self, Action, Direction, Item, Options};
use crate::vfs::FsRef;

/// One side of the sync.
#[derive(Clone)]
pub struct Side {
    pub fs: FsRef,
    pub path: PathBuf,
    /// For synclets: the saved site it is on (None on the Mac), or None
    /// if it cannot be saved (an archive, an unsaved server).
    pub saved: Option<crate::settings::WorkspaceTab>,
}

const DIRECTIONS: [(&str, Direction); 3] = [
    ("Left to Right \u{2192}", Direction::LeftToRight),
    ("Right to Left \u{2190}", Direction::RightToLeft),
    ("Both Ways \u{2194}", Direction::Both),
];

struct Ui {
    window: Retained<NSWindow>,
    direction: Retained<NSPopUpButton>,
    subfolders: Retained<NSButton>,
    hidden: Retained<NSButton>,
    add: Retained<NSButton>,
    update: Retained<NSButton>,
    delete: Retained<NSButton>,
    empty: Retained<NSButton>,
    offset: Retained<NSTextField>,
    exclude: Retained<NSTextField>,
    compare: Retained<NSButton>,
    status: Retained<NSTextField>,
    table: Retained<NSTableView>,
    sync: Retained<NSButton>,
}

struct State {
    left: Side,
    right: Side,
    items: Vec<Item>,
    chosen: Vec<bool>,
    comparing: Option<CancelToken>,
    /// Counts compares, so an old one's result is dropped.
    generation: u64,
    accepted: bool,
}

pub struct Ivars {
    ui: OnceCell<Ui>,
    state: RefCell<State>,
    building: Cell<bool>,
}

thread_local! {
    static OPEN: RefCell<Option<Retained<SyncWindow>>> = const { RefCell::new(None) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzSyncWindow"]
    #[ivars = Ivars]
    struct SyncWindow;

    impl SyncWindow {
        #[unsafe(method(optionsChanged:))]
        fn options_changed(&self, _sender: Option<&AnyObject>) {
            self.update_enabled();
            self.clear();
        }

        #[unsafe(method(compare:))]
        fn compare_action(&self, _sender: Option<&AnyObject>) {
            self.start_compare();
        }

        #[unsafe(method(toggleRow:))]
        fn toggle_row(&self, sender: &NSButton) {
            self.toggle(sender.tag() as usize, sender.state() == NSControlStateValueOn);
        }

        #[unsafe(method(saveSynclet:))]
        fn save_synclet_action(&self, _sender: Option<&AnyObject>) {
            self.save_synclet();
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            self.finish(false);
        }

        #[unsafe(method(sync:))]
        fn sync_action(&self, _sender: Option<&AnyObject>) {
            self.finish(true);
        }
    }

    unsafe impl NSObjectProtocol for SyncWindow {}

    unsafe impl NSControlTextEditingDelegate for SyncWindow {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _notification: &NSNotification) {
            self.clear();
        }
    }

    unsafe impl NSTextFieldDelegate for SyncWindow {}

    unsafe impl NSWindowDelegate for SyncWindow {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            NSApplication::sharedApplication(self.mtm()).stopModal();
        }
    }

    unsafe impl NSTableViewDataSource for SyncWindow {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().state.borrow().items.len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for SyncWindow {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(&self, _table: &NSTableView, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
            self.cell(column, row)
        }
    }
);

fn view<T: Message + AsRef<NSView>>(v: &T) -> Retained<NSView> {
    v.as_ref().retain()
}

fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    NSTextField::labelWithString(&ns(text), mtm)
}

fn describe(entry: Option<&crate::fs::Entry>) -> String {
    match entry {
        None => String::new(),
        Some(e) if e.is_folder_like() => format!("Folder, {}", format_date(e.modified)),
        Some(e) => format!("{}, {}", crate::fs::format_size(e.size), format_date(e.modified)),
    }
}

impl SyncWindow {
    fn ui(&self) -> &Ui {
        self.ivars().ui.get().expect("built")
    }

    fn options(&self) -> Options {
        let ui = self.ui();
        let on = |b: &NSButton| b.state() == NSControlStateValueOn;
        Options {
            direction: DIRECTIONS
                .get(ui.direction.indexOfSelectedItem().max(0) as usize)
                .map_or(Direction::LeftToRight, |d| d.1),
            subfolders: on(&ui.subfolders),
            hidden: on(&ui.hidden),
            add: on(&ui.add),
            update: on(&ui.update),
            delete_orphans: on(&ui.delete),
            empty_folders: on(&ui.empty),
            time_offset_hours: ui.offset.stringValue().to_string().trim().parse().unwrap_or(0),
            exclude: ui.exclude.stringValue().to_string(),
        }
    }

    fn set_options(&self, options: &Options) {
        let ui = self.ui();
        let set = |b: &NSButton, on: bool| b.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        ui.direction.selectItemAtIndex(DIRECTIONS.iter().position(|d| d.1 == options.direction).unwrap_or(0) as isize);
        set(&ui.subfolders, options.subfolders);
        set(&ui.hidden, options.hidden);
        set(&ui.add, options.add);
        set(&ui.update, options.update);
        set(&ui.delete, options.delete_orphans);
        set(&ui.empty, options.empty_folders);
        ui.offset.setStringValue(&ns(&options.time_offset_hours.to_string()));
        ui.exclude.setStringValue(&ns(&options.exclude));
        self.update_enabled();
    }

    fn update_enabled(&self) {
        let ui = self.ui();
        // Both ways deletes nothing (it cannot tell new from deleted).
        let both = self.options().direction == Direction::Both;
        ui.delete.setEnabled(!both);
    }

    /// The options changed: the last comparison no longer holds.
    fn clear(&self) {
        if self.ivars().building.get() {
            return;
        }
        {
            let mut state = self.ivars().state.borrow_mut();
            if let Some(cancel) = state.comparing.take() {
                cancel.cancel();
            }
            state.generation += 1;
            state.items.clear();
            state.chosen.clear();
        }
        let ui = self.ui();
        ui.table.reloadData();
        ui.status.setStringValue(&ns("Compare to see what would change."));
        ui.compare.setEnabled(true);
        ui.sync.setEnabled(false);
    }

    fn start_compare(&self) {
        let options = self.options();
        let (left, right, generation, cancel) = {
            let mut state = self.ivars().state.borrow_mut();
            state.generation += 1;
            let cancel = CancelToken::default();
            state.comparing = Some(cancel.clone());
            (state.left.clone(), state.right.clone(), state.generation, cancel)
        };
        let ui = self.ui();
        ui.status.setStringValue(&ns("Comparing\u{2026}"));
        ui.compare.setEnabled(false);
        ui.sync.setEnabled(false);
        std::thread::spawn(move || {
            let result = sync::compare(&left.fs, &left.path, &right.fs, &right.path, &options, &cancel);
            DispatchQueue::main().exec_async(move || {
                let Some(window) = OPEN.with(|o| o.borrow().clone()) else { return };
                window.compared(generation, result);
            });
        });
    }

    fn compared(&self, generation: u64, result: std::io::Result<Vec<Item>>) {
        {
            let mut state = self.ivars().state.borrow_mut();
            if state.generation != generation {
                return;
            }
            state.comparing = None;
            match &result {
                Ok(items) => {
                    state.chosen = items.iter().map(|i| i.action.is_some()).collect();
                    state.items = items.clone();
                }
                Err(_) => {
                    state.items.clear();
                    state.chosen.clear();
                }
            }
        }
        let ui = self.ui();
        ui.compare.setEnabled(true);
        ui.table.reloadData();
        match result {
            Ok(_) => self.update_status(),
            Err(e) => {
                ui.status.setStringValue(&ns(&format!("The folders could not be compared: {e}")));
                ui.status.setTextColor(Some(&NSColor::systemRedColor()));
            }
        }
    }

    fn update_status(&self) {
        let state = self.ivars().state.borrow();
        let chosen: Vec<&Item> = state.items.iter().zip(&state.chosen).filter(|(_, c)| **c).map(|(i, _)| i).collect();
        let count = |action: Action| chosen.iter().filter(|i| i.action == Some(action)).count();
        let (right, left, del_right, del_left) = (
            count(Action::CopyToRight),
            count(Action::CopyToLeft),
            count(Action::DeleteRight),
            count(Action::DeleteLeft),
        );
        let mut parts = Vec::new();
        if right > 0 {
            parts.push(format!("{right} to copy to the right"));
        }
        if left > 0 {
            parts.push(format!("{left} to copy to the left"));
        }
        if del_right > 0 {
            parts.push(format!("{del_right} to delete on the right"));
        }
        if del_left > 0 {
            parts.push(format!("{del_left} to delete on the left"));
        }
        let text = if state.items.is_empty() {
            "The folders match.".to_string()
        } else if parts.is_empty() {
            "Nothing chosen.".to_string()
        } else {
            parts.join(", ")
        };
        let ui = self.ui();
        ui.status.setStringValue(&ns(&text));
        ui.status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        ui.sync.setEnabled(!chosen.is_empty());
    }

    fn toggle(&self, row: usize, on: bool) {
        {
            let mut state = self.ivars().state.borrow_mut();
            let allowed = state.items.get(row).is_some_and(|i| i.action.is_some());
            if let Some(chosen) = state.chosen.get_mut(row) {
                *chosen = on && allowed;
            }
        }
        self.update_status();
    }

    fn cell(&self, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
        let state = self.ivars().state.borrow();
        let index = usize::try_from(row).ok()?;
        let item = state.items.get(index)?;
        let mtm = self.mtm();
        let id = column?.identifier().to_string();
        if id == "on" {
            let check =
                unsafe { NSButton::checkboxWithTitle_target_action(&ns(""), Some(self), Some(sel!(toggleRow:)), mtm) };
            check.setTag(index as isize);
            check.setState(if state.chosen[index] { NSControlStateValueOn } else { NSControlStateValueOff });
            check.setEnabled(item.action.is_some());
            return Some(view(&*check));
        }
        let text = match id.as_str() {
            "action" => match item.action {
                Some(Action::CopyToRight) => "\u{2192} Copy to right".to_string(),
                Some(Action::CopyToLeft) => "\u{2190} Copy to left".to_string(),
                Some(Action::DeleteRight) => "Delete on right".to_string(),
                Some(Action::DeleteLeft) => "Delete on left".to_string(),
                None => "Left alone".to_string(),
            },
            "path" => {
                let path = item.path.display().to_string();
                if item.folder { format!("{path}/") } else { path }
            }
            "why" => item.reason.describe().to_string(),
            "left" => describe(item.left.as_ref()),
            _ => describe(item.right.as_ref()),
        };
        let field = label(&text, mtm);
        field.setLineBreakMode(if id == "path" {
            NSLineBreakMode::ByTruncatingMiddle
        } else {
            NSLineBreakMode::ByTruncatingTail
        });
        if id == "action" && matches!(item.action, Some(Action::DeleteLeft | Action::DeleteRight)) {
            field.setTextColor(Some(&NSColor::systemRedColor()));
        } else if id != "path" {
            field.setTextColor(Some(&NSColor::secondaryLabelColor()));
        }
        field.setToolTip(Some(&ns(&text)));
        Some(Retained::into_super(Retained::into_super(field)))
    }

    fn save_synclet(&self) {
        let (left, right) = {
            let state = self.ivars().state.borrow();
            (state.left.saved.clone(), state.right.saved.clone())
        };
        let (Some(left), Some(right)) = (left, right) else {
            super::dialogs::show_info(
                "This sync cannot be saved.".into(),
                "Synclets can use folders on the Mac and on saved sites, not archives, search results, or servers that are not saved as sites.".into(),
            );
            return;
        };
        let suggested =
            format!("{} \u{2194} {}", crate::pane::title_of(&left.path), crate::pane::title_of(&right.path));
        let Some(name) = super::dialogs::prompt(
            "Save Synclet",
            "A name for this sync, shown in the sidebar:",
            "Save",
            &suggested,
            None,
        ) else {
            return;
        };
        let synclet = crate::settings::Synclet { name: name.trim().to_string(), left, right, options: self.options() };
        super::update_settings(|s| match s.synclets.iter_mut().find(|x| x.name == synclet.name) {
            Some(existing) => *existing = synclet,
            None => s.synclets.push(synclet),
        });
        super::reload_sidebar();
    }

    fn finish(&self, accept: bool) {
        if let Some(cancel) = self.ivars().state.borrow_mut().comparing.take() {
            cancel.cancel();
        }
        self.ivars().state.borrow_mut().accepted = accept;
        NSApplication::sharedApplication(self.mtm()).stopModal();
    }

    fn build(&self, left: &str, right: &str) {
        let mtm = self.mtm();
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(940.0, 600.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&ns("Sync Folders"));
        window.setMinSize(NSSize::new(800.0, 460.0));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));

        let side = |title: &str, path: &str| {
            let name = label(title, mtm);
            name.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            let place = label(path, mtm);
            place.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
            place.setToolTip(Some(&ns(path)));
            place.setContentCompressionResistancePriority_forOrientation(
                1.0,
                objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
            );
            let stack =
                NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&[view(&*name), view(&*place)]), mtm);
            stack.setSpacing(6.0);
            stack
        };
        let direction = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        let titles: Vec<Retained<NSString>> = DIRECTIONS.iter().map(|d| ns(d.0)).collect();
        direction.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
        unsafe {
            direction.setTarget(Some(self));
            direction.setAction(Some(sel!(optionsChanged:)));
        }
        let places = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[
                view(&*side("Left:", left)),
                view(&*direction),
                view(&*side("Right:", right)),
            ]),
            mtm,
        );
        places.setSpacing(14.0);

        let check = |title: &str| unsafe {
            NSButton::checkboxWithTitle_target_action(&ns(title), Some(self), Some(sel!(optionsChanged:)), mtm)
        };
        let subfolders = check("Subfolders");
        let hidden = check("Hidden items");
        let add = check("Add new items");
        let update = check("Update changed items");
        let delete = check("Delete orphaned items");
        let empty = check("Copy empty folders");
        let checks = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[
                view(&*subfolders),
                view(&*hidden),
                view(&*add),
                view(&*update),
                view(&*delete),
                view(&*empty),
            ]),
            mtm,
        );
        checks.setSpacing(14.0);
        let field = |width: f64, placeholder: &str| {
            let field = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 22.0)),
            );
            field.widthAnchor().constraintEqualToConstant(width).setActive(true);
            field.setPlaceholderString(Some(&ns(placeholder)));
            unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
            field
        };
        let offset = field(50.0, "0");
        let exclude = field(260.0, ".DS_Store; *.tmp; node_modules");
        let compare =
            unsafe { NSButton::buttonWithTitle_target_action(&ns("Compare"), Some(self), Some(sel!(compare:)), mtm) };
        let more = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[
                view(&*label("Time offset:", mtm)),
                view(&*offset),
                view(&*label("hours", mtm)),
                view(&*label("   Leave out:", mtm)),
                view(&*exclude),
                view(&*compare),
            ]),
            mtm,
        );
        more.setSpacing(6.0);

        let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), NSRect::ZERO);
        for (id, title, width) in [
            ("on", "", 24.0),
            ("action", "Action", 115.0),
            ("path", "Item", 260.0),
            ("why", "Why", 140.0),
            ("left", "Left", 160.0),
            ("right", "Right", 160.0),
        ] {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
            column.setTitle(&ns(title));
            column.setWidth(width);
            table.addTableColumn(&column);
        }
        table.setUsesAlternatingRowBackgroundColors(true);
        // Columns share the width as the window grows or shrinks.
        table.setColumnAutoresizingStyle(
            objc2_app_kit::NSTableViewColumnAutoresizingStyle::UniformColumnAutoresizingStyle,
        );
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);
        scroll.heightAnchor().constraintGreaterThanOrEqualToConstant(220.0).setActive(true);

        let status = label("Compare to see what would change.", mtm);
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        status.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
        status.setContentCompressionResistancePriority_forOrientation(
            1.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );
        let save = unsafe {
            NSButton::buttonWithTitle_target_action(
                &ns("Save as Synclet\u{2026}"),
                Some(self),
                Some(sel!(saveSynclet:)),
                mtm,
            )
        };
        let cancel =
            unsafe { NSButton::buttonWithTitle_target_action(&ns("Cancel"), Some(self), Some(sel!(cancel:)), mtm) };
        cancel.setKeyEquivalent(&ns("\u{1b}"));
        let sync = unsafe { NSButton::buttonWithTitle_target_action(&ns("Sync"), Some(self), Some(sel!(sync:)), mtm) };
        sync.setKeyEquivalent(&ns("\r"));
        sync.setEnabled(false);
        let bottom = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[view(&*save), view(&*status), view(&*cancel), view(&*sync)]),
            mtm,
        );
        bottom.setSpacing(10.0);

        let content = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[
                view(&*places),
                view(&*checks),
                view(&*more),
                view(&*scroll),
                view(&*bottom),
            ]),
            mtm,
        );
        content.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        content.setAlignment(NSLayoutAttribute::Leading);
        content.setSpacing(12.0);
        content.setEdgeInsets(NSEdgeInsets { top: 16.0, left: 16.0, bottom: 16.0, right: 16.0 });
        for v in [view(&*places), view(&*scroll), view(&*bottom)] {
            v.leadingAnchor().constraintEqualToAnchor_constant(&content.leadingAnchor(), 16.0).setActive(true);
            v.trailingAnchor().constraintEqualToAnchor_constant(&content.trailingAnchor(), -16.0).setActive(true);
        }
        window.setContentView(Some(&content));
        let _ = self.ivars().ui.set(Ui {
            window,
            direction,
            subfolders,
            hidden,
            add,
            update,
            delete,
            empty,
            offset,
            exclude,
            compare,
            status,
            table,
            sync,
        });
    }
}

/// Shows the window for the two sides. Returns the options and the items
/// chosen if Sync was clicked. `compare_now`: compare right away.
pub fn show(left: Side, right: Side, options: &Options, compare_now: bool) -> Option<(Options, Vec<Item>)> {
    let mtm = mtm();
    let display = |side: &Side| {
        if side.fs.is_local() { side.path.display().to_string() } else { side.fs.display_path(&side.path) }
    };
    let (left_text, right_text) = (display(&left), display(&right));
    let this = SyncWindow::alloc(mtm).set_ivars(Ivars {
        ui: OnceCell::new(),
        state: RefCell::new(State {
            left,
            right,
            items: Vec::new(),
            chosen: Vec::new(),
            comparing: None,
            generation: 0,
            accepted: false,
        }),
        building: Cell::new(true),
    });
    let this: Retained<SyncWindow> = unsafe { msg_send![super(this), init] };
    this.build(&left_text, &right_text);
    this.set_options(options);
    this.ivars().building.set(false);
    OPEN.with(|o| *o.borrow_mut() = Some(this.clone()));
    let window = this.ui().window.clone();
    window.center();
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    window.makeKeyAndOrderFront(None);
    // The columns fit the table's width.
    window.layoutIfNeeded();
    this.ui().table.sizeToFit();
    if compare_now {
        this.start_compare();
    }
    app.runModalForWindow(&window);
    OPEN.with(|o| *o.borrow_mut() = None);
    window.setDelegate(None);
    window.orderOut(None);
    let options = this.options();
    let state = this.ivars().state.borrow();
    if !state.accepted {
        return None;
    }
    let chosen: Vec<Item> =
        state.items.iter().zip(&state.chosen).filter(|(_, c)| **c).map(|(i, _)| i.clone()).collect();
    Some((options, chosen))
}
