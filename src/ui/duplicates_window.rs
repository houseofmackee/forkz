//! Find Duplicates (Commands menu): the files with the same contents in a
//! folder and everything in it, in groups, in a window of its own. Copies
//! can be marked (by hand, or all but the oldest or newest of each group)
//! and moved to the Trash; at least one of each group is always kept.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate, NSFont,
    NSLineBreakMode, NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextField, NSView,
    NSWindow, NSWindowDelegate,
};
use objc2_foundation::{NSInteger, NSNotification, NSObject, NSObjectProtocol, NSSize};

use super::diff_window::{column, find, layout, text_cell, tint, window};
use super::util::{format_date, mtm, ns, small_label};
use crate::fs::duplicates::{self, Group, Stage};
use crate::fs::ops::{CancelToken, Job, JobKind};
use crate::fs::{Entry, format_size};
use crate::vfs::{self, FsRef};

thread_local! {
    static WINDOWS: RefCell<Vec<Retained<Duplicates>>> = const { RefCell::new(Vec::new()) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A group's heading.
    Group(usize),
    /// A file of a group.
    File(usize, usize),
}

struct Parts {
    window: Retained<NSWindow>,
    table: Retained<NSTableView>,
    status: Retained<NSTextField>,
    buttons: Vec<Retained<NSButton>>,
}

pub struct DuplicatesIvars {
    parts: OnceCell<Parts>,
    root: OnceCell<PathBuf>,
    groups: RefCell<Vec<Group>>,
    rows: RefCell<Vec<Row>>,
    marked: RefCell<HashSet<PathBuf>>,
    cancel: RefCell<Option<CancelToken>>,
    done: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzDuplicates"]
    #[ivars = DuplicatesIvars]
    struct Duplicates;

    impl Duplicates {
        #[unsafe(method(toggleMark:))]
        fn toggle_mark(&self, sender: &NSButton) {
            if let Some(Row::File(g, f)) = self.ivars().rows.borrow().get(sender.tag() as usize).copied() {
                let path = self.ivars().groups.borrow()[g].files[f].path.clone();
                let mut marked = self.ivars().marked.borrow_mut();
                if sender.state() == NSControlStateValueOn {
                    marked.insert(path);
                } else {
                    marked.remove(&path);
                }
            }
            self.update_status();
        }

        #[unsafe(method(markAllButOldest:))]
        fn mark_all_but_oldest(&self, _sender: Option<&AnyObject>) {
            self.mark_all_but(true);
        }

        #[unsafe(method(markAllButNewest:))]
        fn mark_all_but_newest(&self, _sender: Option<&AnyObject>) {
            self.mark_all_but(false);
        }

        #[unsafe(method(unmarkAll:))]
        fn unmark_all(&self, _sender: Option<&AnyObject>) {
            self.ivars().marked.borrow_mut().clear();
            self.reload();
        }

        #[unsafe(method(trashMarked:))]
        fn trash_marked(&self, _sender: Option<&AnyObject>) {
            self.trash();
        }

        /// Double-click: the file, selected in the active pane.
        #[unsafe(method(showFile:))]
        fn show_file(&self, _sender: Option<&AnyObject>) {
            let row = self.parts().table.clickedRow();
            if let Some(entry) = usize::try_from(row).ok().and_then(|r| self.file_at(r))
                && let Some(parent) = entry.path.parent()
            {
                super::show_in_active_pane(parent.to_path_buf(), &entry.path);
            }
        }
    }

    unsafe impl NSObjectProtocol for Duplicates {}

    unsafe impl NSControlTextEditingDelegate for Duplicates {}

    unsafe impl NSWindowDelegate for Duplicates {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if let Some(cancel) = self.ivars().cancel.borrow_mut().take() {
                cancel.cancel();
            }
            let me = self as *const Duplicates as usize;
            DispatchQueue::main().exec_async(move || {
                WINDOWS.with(|v| v.borrow_mut().retain(|x| Retained::as_ptr(x) as usize != me));
            });
        }
    }

    unsafe impl NSTableViewDataSource for Duplicates {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().rows.borrow().len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for Duplicates {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            let id = column.map(|c| c.identifier().to_string()).unwrap_or_default();
            usize::try_from(row).ok().and_then(|row| self.cell(table, &id, row))
        }

        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group_row(&self, _table: &NSTableView, row: NSInteger) -> bool {
            matches!(self.ivars().rows.borrow().get(row as usize), Some(Row::Group(_)))
        }

        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select_row(&self, _table: &NSTableView, row: NSInteger) -> bool {
            matches!(self.ivars().rows.borrow().get(row as usize), Some(Row::File(..)))
        }
    }
);

/// Finds the duplicates in a folder on the Mac, in a new window.
pub fn open(root: PathBuf) {
    let mtm = mtm();
    let this = Duplicates::alloc(mtm).set_ivars(DuplicatesIvars {
        parts: OnceCell::new(),
        root: OnceCell::new(),
        groups: RefCell::new(Vec::new()),
        rows: RefCell::new(Vec::new()),
        marked: RefCell::new(HashSet::new()),
        cancel: RefCell::new(None),
        done: Cell::new(false),
    });
    let this: Retained<Duplicates> = unsafe { msg_send![super(this), init] };
    this.build(&root, mtm);
    let _ = this.ivars().root.set(root.clone());
    WINDOWS.with(|v| v.borrow_mut().push(this.clone()));
    let cancel = CancelToken::default();
    *this.ivars().cancel.borrow_mut() = Some(cancel.clone());
    let hidden = super::current_settings().is_some_and(|s| s.show_hidden);
    let me = Retained::as_ptr(&this) as usize;
    std::thread::spawn(move || {
        let fs: FsRef = vfs::local();
        let mut last = Instant::now();
        let mut progress = |stage: Stage| {
            if last.elapsed() < Duration::from_millis(150) {
                return;
            }
            last = Instant::now();
            let text = match stage {
                Stage::Listing(n) => format!("Looking through folders: {n} files so far\u{2026}"),
                Stage::Comparing(done, total) => format!("Comparing contents: {done} of {total} files\u{2026}"),
            };
            DispatchQueue::main().exec_async(move || {
                if let Some(this) = find(&WINDOWS, me)
                    && !this.ivars().done.get()
                {
                    this.parts().status.setStringValue(&ns(&text));
                }
            });
        };
        let result = duplicates::find(&fs, &root, hidden, &cancel, &mut progress);
        DispatchQueue::main().exec_async(move || {
            let Some(this) = find(&WINDOWS, me) else { return };
            this.ivars().done.set(true);
            this.ivars().cancel.borrow_mut().take();
            match result {
                Ok(groups) => {
                    *this.ivars().groups.borrow_mut() = groups;
                    this.reload();
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => this.parts().status.setStringValue(&ns(&format!("The folder could not be searched: {e}"))),
            }
        });
    });
}

impl Duplicates {
    fn parts(&self) -> &Parts {
        self.ivars().parts.get().expect("built")
    }

    fn build(&self, root: &Path, mtm: MainThreadMarker) {
        let name = crate::pane::title_of(root);
        let window = window(&format!("Duplicates in {name}"), NSSize::new(960.0, 620.0), mtm);
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let target: &AnyObject = self.as_ref();
        let button = |title: &str, action| unsafe {
            NSButton::buttonWithTitle_target_action(&ns(title), Some(target), Some(action), mtm)
        };
        let buttons = vec![
            button("Mark All but Oldest", sel!(markAllButOldest:)),
            button("Mark All but Newest", sel!(markAllButNewest:)),
            button("Unmark All", sel!(unmarkAll:)),
            button("Move Marked to Trash", sel!(trashMarked:)),
        ];
        for b in &buttons {
            b.setEnabled(false);
        }
        let status = small_label("Looking through folders\u{2026}", mtm);
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        status.setContentCompressionResistancePriority_forOrientation(
            1.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );

        let table = NSTableView::new(mtm);
        column(&table, "mark", "", 28.0, true);
        column(&table, "name", "Name", 250.0, false);
        column(&table, "folder", "Folder", 330.0, false);
        column(&table, "size", "Size", 90.0, false);
        column(&table, "modified", "Date Modified", 180.0, false);
        table.setColumnAutoresizingStyle(
            objc2_app_kit::NSTableViewColumnAutoresizingStyle::UniformColumnAutoresizingStyle,
        );
        table.setRowHeight(24.0);
        // Marked rows are tinted from edge to edge.
        table.setIntercellSpacing(NSSize::new(0.0, 0.0));
        table.setStyle(objc2_app_kit::NSTableViewStyle::Plain);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
            table.setTarget(Some(target));
            table.setDoubleAction(Some(sel!(showFile:)));
        }
        let mut controls: Vec<&NSView> = buttons.iter().map(|b| &**b as &NSView).collect();
        controls.push(&status);
        layout(&window, &controls, &table);
        window.center();
        let _ = self.ivars().parts.set(Parts { window: window.clone(), table, status, buttons });
        window.makeKeyAndOrderFront(None);
    }

    /// The rows again, from the groups.
    fn reload(&self) {
        let groups = self.ivars().groups.borrow();
        let mut rows = Vec::new();
        for (g, group) in groups.iter().enumerate() {
            rows.push(Row::Group(g));
            rows.extend((0..group.files.len()).map(|f| Row::File(g, f)));
        }
        drop(groups);
        *self.ivars().rows.borrow_mut() = rows;
        let parts = self.parts();
        parts.table.reloadData();
        let any = !self.ivars().groups.borrow().is_empty();
        for b in &parts.buttons {
            b.setEnabled(any);
        }
        self.update_status();
    }

    fn update_status(&self) {
        let groups = self.ivars().groups.borrow();
        let marked = self.ivars().marked.borrow();
        let text = if groups.is_empty() {
            "No duplicates were found.".to_string()
        } else {
            let copies: usize = groups.iter().map(|g| g.files.len() - 1).sum();
            let wasted: u64 = groups.iter().map(Group::wasted).sum();
            let marked_size: u64 =
                groups.iter().flat_map(|g| g.files.iter()).filter(|f| marked.contains(&f.path)).map(|f| f.size).sum();
            let sets = if groups.len() == 1 { "1 set".to_string() } else { format!("{} sets", groups.len()) };
            let extra = if copies == 1 { "1 extra copy".to_string() } else { format!("{copies} extra copies") };
            let mut text = format!("{sets} of duplicates, {extra} ({})", format_size(wasted));
            if !marked.is_empty() {
                text.push_str(&format!(". Marked: {} ({})", marked.len(), format_size(marked_size)));
            }
            text
        };
        self.parts().status.setStringValue(&ns(&text));
    }

    fn mark_all_but(&self, keep_oldest: bool) {
        {
            let groups = self.ivars().groups.borrow();
            let mut marked = self.ivars().marked.borrow_mut();
            marked.clear();
            for group in groups.iter() {
                // The files are oldest first.
                let keep = if keep_oldest { 0 } else { group.files.len() - 1 };
                for (i, file) in group.files.iter().enumerate() {
                    if i != keep {
                        marked.insert(file.path.clone());
                    }
                }
            }
        }
        self.reload();
    }

    /// Moves the marked files to the Trash, if each group keeps a copy.
    fn trash(&self) {
        let marked: Vec<PathBuf> = self.ivars().marked.borrow().iter().cloned().collect();
        if marked.is_empty() {
            objc2_app_kit::NSBeep();
            return;
        }
        let all_marked = self
            .ivars()
            .groups
            .borrow()
            .iter()
            .find(|g| g.files.iter().all(|f| self.ivars().marked.borrow().contains(&f.path)))
            .map(|g| g.files[0].name.clone());
        if let Some(name) = all_marked {
            super::dialogs::show_error(
                "Every copy of a file is marked.".into(),
                format!("Unmark at least one copy of \u{201c}{name}\u{201d}, so one is kept."),
            );
            return;
        }
        let count = marked.len();
        let what = if count == 1 { "1 file".to_string() } else { format!("{count} files") };
        if !super::dialogs::confirm(
            &format!("Move {what} to the Trash?"),
            "One copy of each file stays where it is.",
            "Move to Trash",
            false,
        ) {
            return;
        }
        super::start_job(Job::local(JobKind::Trash, marked.clone(), PathBuf::new()));
        let gone: HashSet<PathBuf> = marked.into_iter().collect();
        {
            let mut groups = self.ivars().groups.borrow_mut();
            for group in groups.iter_mut() {
                group.files.retain(|f| !gone.contains(&f.path));
            }
            groups.retain(|g| g.files.len() > 1);
        }
        self.ivars().marked.borrow_mut().clear();
        self.reload();
        self.parts().window.makeKeyAndOrderFront(None);
    }

    fn file_at(&self, row: usize) -> Option<Entry> {
        match self.ivars().rows.borrow().get(row)? {
            Row::File(g, f) => self.ivars().groups.borrow().get(*g)?.files.get(*f).cloned(),
            Row::Group(_) => None,
        }
    }

    fn cell(&self, table: &NSTableView, id: &str, row: usize) -> Option<Retained<NSView>> {
        let item = *self.ivars().rows.borrow().get(row)?;
        let font = NSFont::systemFontOfSize(NSFont::systemFontSize());
        match item {
            Row::Group(g) => {
                let group = self.ivars().groups.borrow().get(g)?.clone();
                let field = text_cell(table, "group", &NSFont::boldSystemFontOfSize(NSFont::smallSystemFontSize()));
                field.setTextColor(Some(&NSColor::secondaryLabelColor()));
                field.setStringValue(&ns(&format!(
                    "  {} copies of {} each ({} extra)",
                    group.files.len(),
                    format_size(group.size),
                    format_size(group.wasted())
                )));
                Some(Retained::into_super(Retained::into_super(field)))
            }
            Row::File(..) => {
                let entry = self.file_at(row)?;
                let marked = self.ivars().marked.borrow().contains(&entry.path);
                if id == "mark" {
                    let check = unsafe {
                        NSButton::checkboxWithTitle_target_action(
                            &ns(""),
                            Some(self.as_ref()),
                            Some(sel!(toggleMark:)),
                            self.mtm(),
                        )
                    };
                    check.setState(if marked { NSControlStateValueOn } else { NSControlStateValueOff });
                    check.setTag(row as isize);
                    check.setToolTip(Some(&ns("Mark to move to the Trash")));
                    return Some(Retained::into_super(Retained::into_super(check)));
                }
                let field = text_cell(table, id, &font);
                field.setDrawsBackground(marked);
                if marked {
                    field.setBackgroundColor(Some(&tint(NSColor::systemRedColor(), 0.12)));
                }
                let root = self.ivars().root.get()?;
                let text = match id {
                    "name" => entry.name.clone(),
                    "folder" => {
                        let parent = entry.path.parent().unwrap_or(Path::new("/"));
                        match parent.strip_prefix(root) {
                            Ok(inside) if inside.as_os_str().is_empty() => crate::pane::title_of(root),
                            Ok(inside) => format!("{}/{}", crate::pane::title_of(root), inside.display()),
                            Err(_) => parent.display().to_string(),
                        }
                    }
                    "size" => format_size(entry.size),
                    _ => format_date(entry.modified),
                };
                field.setStringValue(&ns(&format!(" {text}")));
                field.setToolTip(Some(&ns(&entry.path.display().to_string())));
                Some(Retained::into_super(Retained::into_super(field)))
            }
        }
    }
}
