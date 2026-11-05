//! The built-in compare tool (Compare Files with "forkz (built in)"): two
//! files side by side, line by line, with the changed words marked; or two
//! folders and everything in them. Works on the Mac, on servers, and in
//! archives.

use std::cell::{Cell, OnceCell, RefCell};
use std::io::Read;
use std::path::{Path, PathBuf};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate,
    NSEventModifierFlags, NSFont, NSLineBreakMode, NSScrollView, NSStackView, NSTableColumn,
    NSTableColumnResizingOptions, NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextField,
    NSUserInterfaceItemIdentification, NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSInteger, NSMutableAttributedString, NSNotification, NSObject, NSObjectProtocol, NSPoint,
    NSRange, NSRect, NSSize,
};

use super::util::{format_date, mtm, ns, small_label};
use crate::fs::diff::{self, FolderItem, FolderOptions, LineKind, Status, TextDiff, TextOptions};
use crate::fs::ops::CancelToken;
use crate::fs::{Entry, format_size};
use crate::vfs::FsRef;

/// How much of each file is compared.
const LIMIT: u64 = 32 * 1024 * 1024;

thread_local! {
    /// Open compare windows (each closes on its own).
    static FILES: RefCell<Vec<Retained<FileDiff>>> = const { RefCell::new(Vec::new()) };
    static FOLDERS: RefCell<Vec<Retained<FolderDiff>>> = const { RefCell::new(Vec::new()) };
}

/// One side of a comparison.
#[derive(Clone)]
pub struct Side {
    pub fs: FsRef,
    pub path: PathBuf,
}

impl Side {
    fn name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| self.fs.label())
    }

    /// The path, with the server or archive it is on.
    fn describe(&self) -> String {
        if !self.fs.is_local() {
            return self.fs.display_path(&self.path);
        }
        // In the home folder: "~/...".
        match dirs::home_dir().and_then(|home| self.path.strip_prefix(home).ok().map(Path::to_path_buf)) {
            Some(inside) => format!("~/{}", inside.display()),
            None => self.path.display().to_string(),
        }
    }
}

fn window(title: &str, size: NSSize, mtm: MainThreadMarker) -> Retained<NSWindow> {
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), size),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::Miniaturizable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&ns(title));
    window.setMinSize(NSSize::new(520.0, 260.0));
    window
}

fn checkbox(title: &str, on: bool, target: &AnyObject, action: objc2::runtime::Sel) -> Retained<NSButton> {
    let button = unsafe { NSButton::checkboxWithTitle_target_action(&ns(title), Some(target), Some(action), mtm()) };
    button.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    button
}

fn is_on(button: &NSButton) -> bool {
    button.state() == NSControlStateValueOn
}

/// A bar of controls, then a table filling the rest.
fn layout(window: &NSWindow, controls: &[&NSView], table: &NSTableView) -> Retained<NSScrollView> {
    let mtm = mtm();
    let bar = NSStackView::stackViewWithViews(&NSArray::from_slice(controls), mtm);
    bar.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    bar.setSpacing(12.0);
    bar.setEdgeInsets(NSEdgeInsets { top: 8.0, left: 10.0, bottom: 8.0, right: 10.0 });
    let scroll = NSScrollView::new(mtm);
    scroll.setDocumentView(Some(table));
    scroll.setHasVerticalScroller(true);
    let content = NSStackView::stackViewWithViews(&NSArray::from_slice(&[&*bar as &NSView, &*scroll as &NSView]), mtm);
    content.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    content.setSpacing(0.0);
    for view in [&*bar as &NSView, &scroll] {
        view.leadingAnchor().constraintEqualToAnchor(&content.leadingAnchor()).setActive(true);
        view.trailingAnchor().constraintEqualToAnchor(&content.trailingAnchor()).setActive(true);
    }
    scroll.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Vertical);
    window.setContentView(Some(&content));
    scroll
}

fn column(table: &NSTableView, id: &str, title: &str, width: f64, fixed: bool) -> Retained<NSTableColumn> {
    let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm()), &ns(id));
    column.setTitle(&ns(title));
    column.setWidth(width);
    if fixed {
        column.setResizingMask(NSTableColumnResizingOptions::empty());
    }
    table.addTableColumn(&column);
    column
}

/// A reusable text cell of a table.
fn text_cell(table: &NSTableView, id: &str, font: &NSFont) -> Retained<NSTextField> {
    let reuse = ns(id);
    let existing: Option<Retained<NSView>> = unsafe { table.makeViewWithIdentifier_owner(&reuse, None) };
    match existing.and_then(|v| v.downcast::<NSTextField>().ok()) {
        Some(field) => field,
        None => {
            let field = NSTextField::labelWithString(&ns(""), mtm());
            field.setIdentifier(Some(&reuse));
            field.setFont(Some(font));
            field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            field
        }
    }
}

fn tint(color: Retained<NSColor>, alpha: f64) -> Retained<NSColor> {
    color.colorWithAlphaComponent(alpha)
}

fn monospaced() -> Retained<NSFont> {
    NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { objc2_app_kit::NSFontWeightRegular })
}

/// Finds a window's controller by identity (windows can close before
/// background work comes back).
fn find<T: objc2::Message>(
    list: &'static std::thread::LocalKey<RefCell<Vec<Retained<T>>>>,
    me: usize,
) -> Option<Retained<T>> {
    list.with(|l| l.borrow().iter().find(|x| Retained::as_ptr(x) as usize == me).cloned())
}

// Two files.

struct FileParts {
    window: Retained<NSWindow>,
    table: Retained<NSTableView>,
    scroll: Retained<NSScrollView>,
    whitespace: Retained<NSButton>,
    case: Retained<NSButton>,
    status: Retained<NSTextField>,
}

/// What was read: the texts, or a note for binary files and errors.
enum Loaded {
    Waiting,
    Texts(String, String),
    Note(String),
}

pub struct FileDiffIvars {
    parts: OnceCell<FileParts>,
    sides: OnceCell<(Side, Side)>,
    loaded: RefCell<Loaded>,
    diff: RefCell<TextDiff>,
    /// The change last moved to (Next and Previous Change).
    current: Cell<Option<usize>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzFileDiff"]
    #[ivars = FileDiffIvars]
    struct FileDiff;

    impl FileDiff {
        #[unsafe(method(optionsChanged:))]
        fn options_changed(&self, _sender: Option<&AnyObject>) {
            self.compare();
        }

        #[unsafe(method(nextChange:))]
        fn next_change(&self, _sender: Option<&AnyObject>) {
            self.go_to_change(1);
        }

        #[unsafe(method(previousChange:))]
        fn previous_change(&self, _sender: Option<&AnyObject>) {
            self.go_to_change(-1);
        }
    }

    unsafe impl NSObjectProtocol for FileDiff {}

    unsafe impl NSControlTextEditingDelegate for FileDiff {}

    unsafe impl NSWindowDelegate for FileDiff {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            let me = self as *const FileDiff as usize;
            DispatchQueue::main().exec_async(move || {
                FILES.with(|v| v.borrow_mut().retain(|x| Retained::as_ptr(x) as usize != me));
            });
        }
    }

    unsafe impl NSTableViewDataSource for FileDiff {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().diff.borrow().rows.len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for FileDiff {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            match (column, usize::try_from(row)) {
                (Some(column), Ok(row)) => self.line_cell(table, &column.identifier().to_string(), row),
                _ => None,
            }
        }

        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select_row(&self, _table: &NSTableView, _row: NSInteger) -> bool {
            false
        }
    }
);

/// Compares two files in a window of its own.
pub fn compare_files(left: Side, right: Side) {
    let mtm = mtm();
    let this = FileDiff::alloc(mtm).set_ivars(FileDiffIvars {
        parts: OnceCell::new(),
        sides: OnceCell::new(),
        loaded: RefCell::new(Loaded::Waiting),
        diff: RefCell::new(TextDiff::default()),
        current: Cell::new(None),
    });
    let this: Retained<FileDiff> = unsafe { msg_send![super(this), init] };
    this.build(&left, &right, mtm);
    let _ = this.ivars().sides.set((left.clone(), right.clone()));
    FILES.with(|v| v.borrow_mut().push(this.clone()));
    let me = Retained::as_ptr(&this) as usize;
    std::thread::spawn(move || {
        let read = |side: &Side| -> std::io::Result<(Vec<u8>, bool)> {
            let mut bytes = Vec::new();
            side.fs.open_read(&side.path)?.take(LIMIT + 1).read_to_end(&mut bytes)?;
            let cut = bytes.len() as u64 > LIMIT;
            bytes.truncate(LIMIT as usize);
            Ok((bytes, cut))
        };
        let loaded = match (read(&left), read(&right)) {
            (Err(e), _) => Loaded::Note(format!("\u{201c}{}\u{201d} could not be read: {e}", left.name())),
            (_, Err(e)) => Loaded::Note(format!("\u{201c}{}\u{201d} could not be read: {e}", right.name())),
            (Ok((a, _)), Ok((b, _))) if crate::fs::view::looks_binary(&a) || crate::fs::view::looks_binary(&b) => {
                Loaded::Note(match diff::first_difference(&a, &b) {
                    None => "These are binary files, and they are the same.".into(),
                    Some(at) => format!(
                        "These are binary files ({} and {}), and they differ from byte {at} on.",
                        format_size(a.len() as u64),
                        format_size(b.len() as u64)
                    ),
                })
            }
            (Ok((a, cut_a)), Ok((b, cut_b))) => {
                if cut_a || cut_b {
                    tracing::info!("comparing only the first {} of large files", format_size(LIMIT));
                }
                Loaded::Texts(crate::fs::view::decode(&a).0, crate::fs::view::decode(&b).0)
            }
        };
        DispatchQueue::main().exec_async(move || {
            if let Some(this) = find(&FILES, me) {
                *this.ivars().loaded.borrow_mut() = loaded;
                this.compare();
            }
        });
    });
}

impl FileDiff {
    fn parts(&self) -> &FileParts {
        self.ivars().parts.get().expect("built")
    }

    fn build(&self, left: &Side, right: &Side, mtm: MainThreadMarker) {
        let title =
            if left.name() == right.name() { left.name() } else { format!("{} and {}", left.name(), right.name()) };
        let window = window(&title, NSSize::new(1100.0, 700.0), mtm);
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let target: &AnyObject = self.as_ref();
        let arrow = |symbol: &str, description: &str, action, key: &str| {
            let image = super::util::symbol(symbol, description).unwrap_or_else(super::util::folder_icon);
            let button = unsafe { NSButton::buttonWithImage_target_action(&image, Some(target), Some(action), mtm) };
            button.setToolTip(Some(&ns(description)));
            button.setKeyEquivalent(&ns(key));
            button.setKeyEquivalentModifierMask(NSEventModifierFlags::Command);
            button
        };
        let previous = arrow("chevron.up", "Previous Change (Command-Up Arrow)", sel!(previousChange:), "\u{f700}");
        let next = arrow("chevron.down", "Next Change (Command-Down Arrow)", sel!(nextChange:), "\u{f701}");
        let whitespace = checkbox("Ignore Whitespace", false, target, sel!(optionsChanged:));
        let case = checkbox("Ignore Case", false, target, sel!(optionsChanged:));
        let status = small_label("Comparing\u{2026}", mtm);
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        status.setContentCompressionResistancePriority_forOrientation(
            1.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );

        let table = NSTableView::new(mtm);
        column(&table, "ln", "", 46.0, true);
        let l = column(&table, "left", &left.describe(), 480.0, false);
        column(&table, "rn", "", 46.0, true);
        let r = column(&table, "right", &right.describe(), 480.0, false);
        l.setHeaderToolTip(Some(&ns(&left.describe())));
        r.setHeaderToolTip(Some(&ns(&right.describe())));
        table.setColumnAutoresizingStyle(
            objc2_app_kit::NSTableViewColumnAutoresizingStyle::UniformColumnAutoresizingStyle,
        );
        table.setRowHeight(17.0);
        table.setIntercellSpacing(NSSize::new(0.0, 0.0));
        table.setStyle(objc2_app_kit::NSTableViewStyle::Plain);
        table.setAllowsColumnReordering(false);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        let scroll = layout(
            &window,
            &[
                &*previous as &NSView,
                &*next as &NSView,
                &*whitespace as &NSView,
                &*case as &NSView,
                &*status as &NSView,
            ],
            &table,
        );
        window.center();
        let _ = self.ivars().parts.set(FileParts { window: window.clone(), table, scroll, whitespace, case, status });
        window.makeKeyAndOrderFront(None);
    }

    /// Compares the texts read (again, after an option changed).
    fn compare(&self) {
        let parts = self.parts();
        let options = TextOptions { ignore_whitespace: is_on(&parts.whitespace), ignore_case: is_on(&parts.case) };
        let note = match &*self.ivars().loaded.borrow() {
            Loaded::Waiting => return,
            Loaded::Note(note) => Some(note.clone()),
            Loaded::Texts(a, b) => {
                *self.ivars().diff.borrow_mut() = diff::diff_texts(a, b, options);
                None
            }
        };
        self.ivars().current.set(None);
        parts.table.reloadData();
        parts.status.setStringValue(&ns(&note.unwrap_or_else(|| self.summary())));
        parts.window.makeFirstResponder(Some(&parts.table));
        // The first change, right away.
        if !self.ivars().diff.borrow().changes.is_empty() {
            self.go_to_change(1);
        }
    }

    /// "3 changes: 2 lines changed, 1 removed, 4 added".
    fn summary(&self) -> String {
        let diff = self.ivars().diff.borrow();
        let n = diff.changes.len();
        if n == 0 {
            return "The files are the same.".into();
        }
        let (changed, removed, added) = diff.counts();
        // "2 lines changed, 1 removed": the first count says what is counted.
        let mut parts = Vec::new();
        for (count, what) in [(changed, "changed"), (removed, "removed"), (added, "added")] {
            if count > 0 {
                let noun = match (parts.is_empty(), count) {
                    (true, 1) => " line",
                    (true, _) => " lines",
                    _ => "",
                };
                parts.push(format!("{count}{noun} {what}"));
            }
        }
        let changes = if n == 1 { "1 change".to_string() } else { format!("{n} changes") };
        let current = self.ivars().current.get().map(|c| format!("Change {} of {n}. ", c + 1)).unwrap_or_default();
        format!("{current}{changes}: {}", parts.join(", "))
    }

    /// Moves to the next (1) or previous (-1) change, near the top.
    fn go_to_change(&self, step: isize) {
        let diff = self.ivars().diff.borrow();
        let n = diff.changes.len();
        if n == 0 {
            objc2_app_kit::NSBeep();
            return;
        }
        let next = match (self.ivars().current.get(), step) {
            (None, s) if s > 0 => 0,
            (None, _) => n - 1,
            (Some(c), s) if s > 0 => (c + 1).min(n - 1),
            (Some(c), _) => c.saturating_sub(1),
        };
        if self.ivars().current.get() == Some(next) {
            objc2_app_kit::NSBeep();
            return;
        }
        let row = diff.changes[next];
        drop(diff);
        self.ivars().current.set(Some(next));
        let parts = self.parts();
        let rect = parts.table.rectOfRow(row as NSInteger);
        let clip = parts.scroll.contentView();
        // The top, below the column titles (an inset of the clip view).
        let top = -clip.contentInsets().top.max(parts.scroll.contentInsets().top);
        let height = clip.bounds().size.height + top;
        let y = (rect.origin.y - height / 4.0).max(top);
        let x = clip.bounds().origin.x;
        clip.scrollToPoint(NSPoint::new(x, y));
        parts.scroll.reflectScrolledClipView(&clip);
        // The line numbers of the current change stand out.
        parts.table.reloadData();
        parts.status.setStringValue(&ns(&self.summary()));
    }

    /// Whether a row is part of the current change.
    fn in_current(&self, row: usize) -> bool {
        let diff = self.ivars().diff.borrow();
        let Some(start) = self.ivars().current.get().and_then(|c| diff.changes.get(c).copied()) else { return false };
        row >= start && diff.rows[start..=row].iter().all(|r| r.kind != LineKind::Same)
    }

    fn line_cell(&self, table: &NSTableView, id: &str, row: usize) -> Option<Retained<NSView>> {
        let diff = self.ivars().diff.borrow();
        let line = *diff.rows.get(row)?;
        let left = id == "left" || id == "ln";
        let index = if left { line.left } else { line.right };
        let font = monospaced();
        let field = text_cell(table, id, &font);
        // The line's color: removed on the left, added on the right, a
        // grey gap where a side has no line.
        let background = match (line.kind, index) {
            (LineKind::Same, _) => None,
            (_, None) => Some(tint(NSColor::secondaryLabelColor(), 0.07)),
            (LineKind::Changed, _) => Some(tint(NSColor::systemBlueColor(), 0.13)),
            (LineKind::Removed, _) => Some(tint(NSColor::systemRedColor(), 0.15)),
            (LineKind::Added, _) => Some(tint(NSColor::systemGreenColor(), 0.17)),
        };
        field.setDrawsBackground(background.is_some());
        if let Some(color) = &background {
            field.setBackgroundColor(Some(color));
        }
        field.setToolTip(None);
        if id == "ln" || id == "rn" {
            let number = index.map(|i| (i + 1).to_string()).unwrap_or_default();
            let current = self.in_current(row);
            let bold = NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { objc2_app_kit::NSFontWeightBold });
            field.setFont(Some(if current { &bold } else { &font }));
            let color = if current { NSColor::controlAccentColor() } else { NSColor::tertiaryLabelColor() };
            field.setTextColor(Some(&color));
            field.setAlignment(objc2_app_kit::NSTextAlignment::Right);
            field.setStringValue(&ns(&format!("{number} ")));
            return Some(Retained::into_super(Retained::into_super(field)));
        }
        let text =
            index.and_then(|i| if left { diff.left.get(i) } else { diff.right.get(i) }).cloned().unwrap_or_default();
        field.setTextColor(Some(&NSColor::labelColor()));
        let shown = format!(" {text}");
        if line.kind == LineKind::Changed
            && let (Some(l), Some(r)) = (line.left, line.right)
        {
            let (left_parts, right_parts) = diff::changed_parts(&diff.left[l], &diff.right[r]);
            let parts = if left { left_parts } else { right_parts };
            let attributed = attributed_text(&shown);
            let strong = tint(NSColor::systemBlueColor(), 0.38);
            for range in parts {
                // One more for the leading space.
                let range = NSRange::new(range.start + 1, range.len());
                unsafe {
                    attributed.addAttribute_value_range(objc2_app_kit::NSBackgroundColorAttributeName, &strong, range);
                }
            }
            let all = NSRange::new(0, attributed.length());
            unsafe {
                attributed.addAttribute_value_range(objc2_app_kit::NSFontAttributeName, &font, all);
                attributed.addAttribute_value_range(
                    objc2_app_kit::NSForegroundColorAttributeName,
                    &NSColor::labelColor(),
                    all,
                );
            }
            field.setAttributedStringValue(&attributed);
        } else {
            field.setFont(Some(&font));
            field.setStringValue(&ns(&shown));
        }
        // Long lines are cut off at the column's edge: the whole line on
        // hover.
        if text.chars().count() > 60 {
            field.setToolTip(Some(&ns(&text)));
        }
        Some(Retained::into_super(Retained::into_super(field)))
    }
}

// Two folders.

struct FolderParts {
    table: Retained<NSTableView>,
    contents: Retained<NSButton>,
    hidden: Retained<NSButton>,
    identical: Retained<NSButton>,
    status: Retained<NSTextField>,
}

pub struct FolderDiffIvars {
    parts: OnceCell<FolderParts>,
    sides: OnceCell<(Side, Side)>,
    items: RefCell<Vec<FolderItem>>,
    /// The items shown (by index into `items`).
    shown: RefCell<Vec<usize>>,
    /// The comparison running, if any.
    cancel: RefCell<Option<CancelToken>>,
    generation: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzFolderDiff"]
    #[ivars = FolderDiffIvars]
    struct FolderDiff;

    impl FolderDiff {
        #[unsafe(method(compareAgain:))]
        fn compare_again(&self, _sender: Option<&AnyObject>) {
            self.compare();
        }

        #[unsafe(method(filterChanged:))]
        fn filter_changed(&self, _sender: Option<&AnyObject>) {
            self.show_items();
        }

        /// Double-click: two files are compared in a window of their own.
        #[unsafe(method(openRow:))]
        fn open_row(&self, _sender: Option<&AnyObject>) {
            self.open_clicked();
        }
    }

    unsafe impl NSObjectProtocol for FolderDiff {}

    unsafe impl NSControlTextEditingDelegate for FolderDiff {}

    unsafe impl NSWindowDelegate for FolderDiff {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if let Some(cancel) = self.ivars().cancel.borrow_mut().take() {
                cancel.cancel();
            }
            let me = self as *const FolderDiff as usize;
            DispatchQueue::main().exec_async(move || {
                FOLDERS.with(|v| v.borrow_mut().retain(|x| Retained::as_ptr(x) as usize != me));
            });
        }
    }

    unsafe impl NSTableViewDataSource for FolderDiff {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().shown.borrow().len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for FolderDiff {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            match (column, usize::try_from(row)) {
                (Some(column), Ok(row)) => self.item_cell(table, &column.identifier().to_string(), row),
                _ => None,
            }
        }
    }
);

/// Compares two folders in a window of its own.
pub fn compare_folders(left: Side, right: Side) {
    let mtm = mtm();
    let this = FolderDiff::alloc(mtm).set_ivars(FolderDiffIvars {
        parts: OnceCell::new(),
        sides: OnceCell::new(),
        items: RefCell::new(Vec::new()),
        shown: RefCell::new(Vec::new()),
        cancel: RefCell::new(None),
        generation: Cell::new(0),
    });
    let this: Retained<FolderDiff> = unsafe { msg_send![super(this), init] };
    this.build(&left, &right, mtm);
    let _ = this.ivars().sides.set((left, right));
    FOLDERS.with(|v| v.borrow_mut().push(this.clone()));
    this.compare();
}

impl FolderDiff {
    fn parts(&self) -> &FolderParts {
        self.ivars().parts.get().expect("built")
    }

    fn build(&self, left: &Side, right: &Side, mtm: MainThreadMarker) {
        let title =
            if left.name() == right.name() { left.name() } else { format!("{} and {}", left.name(), right.name()) };
        let window = window(&title, NSSize::new(1000.0, 640.0), mtm);
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let target: &AnyObject = self.as_ref();
        let contents = checkbox("Compare Contents", false, target, sel!(compareAgain:));
        contents.setToolTip(Some(&ns(
            "Files of the same size are compared byte by byte (slower, and on servers they are downloaded). Otherwise their dates are compared.",
        )));
        let hidden = checkbox("Hidden Items", false, target, sel!(compareAgain:));
        let identical = checkbox("Show Identical Items", false, target, sel!(filterChanged:));
        let again = unsafe {
            NSButton::buttonWithTitle_target_action(&ns("Compare Again"), Some(target), Some(sel!(compareAgain:)), mtm)
        };
        let status = small_label("Comparing\u{2026}", mtm);
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        status.setContentCompressionResistancePriority_forOrientation(
            1.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );

        let table = NSTableView::new(mtm);
        column(&table, "name", "Name", 330.0, false);
        let l = column(&table, "left", &left.describe(), 230.0, false);
        column(&table, "status", "Difference", 170.0, false);
        let r = column(&table, "right", &right.describe(), 230.0, false);
        l.setHeaderToolTip(Some(&ns(&left.describe())));
        r.setHeaderToolTip(Some(&ns(&right.describe())));
        table.setColumnAutoresizingStyle(
            objc2_app_kit::NSTableViewColumnAutoresizingStyle::UniformColumnAutoresizingStyle,
        );
        table.setRowHeight(20.0);
        table.setIntercellSpacing(NSSize::new(0.0, 0.0));
        table.setStyle(objc2_app_kit::NSTableViewStyle::Plain);
        table.setAllowsMultipleSelection(false);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
            table.setTarget(Some(target));
        }
        unsafe { table.setDoubleAction(Some(sel!(openRow:))) };
        layout(
            &window,
            &[
                &*contents as &NSView,
                &*hidden as &NSView,
                &*identical as &NSView,
                &*again as &NSView,
                &*status as &NSView,
            ],
            &table,
        );
        window.center();
        let _ = self.ivars().parts.set(FolderParts { table, contents, hidden, identical, status });
        window.makeKeyAndOrderFront(None);
    }

    /// Compares the folders (in the background).
    fn compare(&self) {
        let Some((left, right)) = self.ivars().sides.get().cloned() else { return };
        let parts = self.parts();
        let options = FolderOptions { contents: is_on(&parts.contents), hidden: is_on(&parts.hidden) };
        if let Some(previous) = self.ivars().cancel.borrow_mut().take() {
            previous.cancel();
        }
        let cancel = CancelToken::default();
        *self.ivars().cancel.borrow_mut() = Some(cancel.clone());
        let generation = self.ivars().generation.get() + 1;
        self.ivars().generation.set(generation);
        parts.status.setStringValue(&ns("Comparing\u{2026}"));
        let me = self as *const FolderDiff as usize;
        std::thread::spawn(move || {
            // Servers get connections of their own, so browsing stays quick.
            let own = |fs: &FsRef| {
                if fs.is_local() { fs.clone() } else { fs.connect_another().unwrap_or_else(|_| fs.clone()) }
            };
            let (lfs, rfs) = (own(&left.fs), own(&right.fs));
            let result = diff::compare_folders(&lfs, &left.path, &rfs, &right.path, options, &cancel);
            DispatchQueue::main().exec_async(move || {
                let Some(this) = find(&FOLDERS, me) else { return };
                if this.ivars().generation.get() != generation {
                    return;
                }
                this.ivars().cancel.borrow_mut().take();
                match result {
                    Ok(items) => {
                        *this.ivars().items.borrow_mut() = items;
                        this.show_items();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        this.parts().status.setStringValue(&ns(&format!("The folders could not be compared: {e}")))
                    }
                }
            });
        });
    }

    fn show_items(&self) {
        let parts = self.parts();
        let all = is_on(&parts.identical);
        let items = self.ivars().items.borrow();
        *self.ivars().shown.borrow_mut() =
            (0..items.len()).filter(|&i| all || items[i].status != Status::Same).collect();
        // Differences, not counting folders with differences inside.
        let differ = items.iter().filter(|i| !matches!(i.status, Status::Same | Status::Inside)).count();
        let status = match differ {
            0 => "The folders are the same.".to_string(),
            1 => "1 difference. Double-click two files to compare them.".to_string(),
            n => format!("{n} differences. Double-click two files to compare them."),
        };
        drop(items);
        parts.status.setStringValue(&ns(&status));
        parts.table.reloadData();
    }

    fn item(&self, row: usize) -> Option<FolderItem> {
        let index = *self.ivars().shown.borrow().get(row)?;
        self.ivars().items.borrow().get(index).cloned()
    }

    fn open_clicked(&self) {
        let parts = self.parts();
        let Some(item) = usize::try_from(parts.table.clickedRow()).ok().and_then(|r| self.item(r)) else { return };
        let Some((left, right)) = self.ivars().sides.get() else { return };
        match (&item.left, &item.right) {
            (Some(a), Some(b)) if !a.is_folder_like() && !b.is_folder_like() => compare_files(
                Side { fs: left.fs.clone(), path: left.path.join(&item.path) },
                Side { fs: right.fs.clone(), path: right.path.join(&item.path) },
            ),
            _ => objc2_app_kit::NSBeep(),
        }
    }

    fn item_cell(&self, table: &NSTableView, id: &str, row: usize) -> Option<Retained<NSView>> {
        let item = self.item(row)?;
        let font = NSFont::systemFontOfSize(NSFont::systemFontSize());
        let field = text_cell(table, id, &font);
        let quiet = matches!(item.status, Status::Same | Status::Inside);
        let text_color = if quiet && id == "status" { NSColor::secondaryLabelColor() } else { NSColor::labelColor() };
        field.setTextColor(Some(&text_color));
        let background = match item.status {
            Status::Same | Status::Inside => None,
            Status::OnlyLeft => Some(tint(NSColor::systemRedColor(), 0.13)),
            Status::OnlyRight => Some(tint(NSColor::systemGreenColor(), 0.15)),
            _ => Some(tint(NSColor::systemBlueColor(), 0.12)),
        };
        field.setDrawsBackground(background.is_some());
        if let Some(color) = &background {
            field.setBackgroundColor(Some(color));
        }
        let text = match id {
            "name" => {
                let name = item.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let entry = item.left.as_ref().or(item.right.as_ref());
                field.setAttributedStringValue(&self.name_label(&name, entry, item.depth, &font));
                return Some(Retained::into_super(Retained::into_super(field)));
            }
            "left" => describe_entry(item.left.as_ref()),
            "right" => describe_entry(item.right.as_ref()),
            _ => item.status.describe().to_string(),
        };
        field.setFont(Some(&font));
        field.setStringValue(&ns(&format!(" {text}")));
        Some(Retained::into_super(Retained::into_super(field)))
    }

    /// An icon and the name, indented by depth.
    fn name_label(
        &self,
        name: &str,
        entry: Option<&Entry>,
        depth: usize,
        font: &NSFont,
    ) -> Retained<NSMutableAttributedString> {
        use objc2_app_kit::{NSAttributedStringAttachmentConveniences, NSMutableParagraphStyle, NSTextAttachment};
        let label = NSMutableAttributedString::new();
        if let Some(entry) = entry {
            let local = self.ivars().sides.get().is_some_and(|(l, _)| l.fs.is_local());
            let icon = super::util::icon_of(entry, local);
            let copy: Retained<objc2_app_kit::NSImage> = unsafe { msg_send![&icon, copy] };
            let side = 16.0;
            copy.setSize(NSSize::new(side, side));
            let attachment = NSTextAttachment::new();
            attachment.setImage(Some(&copy));
            attachment.setBounds(NSRect::new(
                NSPoint::new(0.0, ((font.capHeight() - side) / 2.0).round()),
                NSSize::new(side, side),
            ));
            label.appendAttributedString(&objc2_foundation::NSAttributedString::attributedStringWithAttachment(
                &attachment,
            ));
        }
        let text = attributed_text(&format!(" {name}"));
        label.appendAttributedString(&text);
        let paragraph = NSMutableParagraphStyle::new();
        paragraph.setFirstLineHeadIndent(6.0 + depth as f64 * 18.0);
        paragraph.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        let all = NSRange::new(0, label.length());
        unsafe {
            label.addAttribute_value_range(objc2_app_kit::NSFontAttributeName, font, all);
            label.addAttribute_value_range(objc2_app_kit::NSParagraphStyleAttributeName, &paragraph, all);
            label.addAttribute_value_range(objc2_app_kit::NSForegroundColorAttributeName, &NSColor::labelColor(), all);
        }
        label
    }
}

/// "12 KB, 5 Nov 2026 at 10:04" (folders: the date), or "" for none.
fn describe_entry(entry: Option<&Entry>) -> String {
    match entry {
        None => String::new(),
        Some(e) if e.is_folder_like() => format_date(e.modified),
        Some(e) => format!("{}, {}", format_size(e.size), format_date(e.modified)),
    }
}

/// Text that attributes can be added to.
fn attributed_text(text: &str) -> Retained<NSMutableAttributedString> {
    let attributed = NSMutableAttributedString::new();
    attributed.replaceCharactersInRange_withString(NSRange::new(0, 0), &ns(text));
    attributed
}
