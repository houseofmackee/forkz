//! The preview pane on the right of the window (Shift-Command-P), with four
//! tabs: Info about the selection, a Quick Look Preview (small server files
//! are fetched for it), Activity (finished transfers), and the Log.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSColor, NSControlTextEditingDelegate, NSFont, NSGridCellPlacement, NSGridView, NSImageView,
    NSLayoutAttribute, NSLineBreakMode, NSScrollView, NSSegmentSwitchTracking, NSSegmentedControl, NSStackView,
    NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextField, NSTextView,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSInteger, NSObject, NSObjectProtocol, NSRect, NSSize, NSString};
use objc2_quick_look_ui::{QLPreviewItem, QLPreviewView, QLPreviewViewStyle};

use super::oplog::{self, Kind, LogEntry};
use super::util::{file_url, format_date, icon_of, kind_of, ns};
use crate::fs::{self, Entry};
use crate::vfs::FsRef;

pub const TABS: [(&str, &str); 4] =
    [("info", "Info"), ("preview", "Preview"), ("activity", "Activity"), ("log", "Log")];

/// Server files up to this size are fetched to preview them.
const REMOTE_PREVIEW_LIMIT: u64 = 25 * 1024 * 1024;

/// Archives on servers up to this size are fetched to list them.
const REMOTE_ARCHIVE_LIMIT: u64 = 200 * 1024 * 1024;

/// Rows shown for an archive's contents.
const ARCHIVE_ROWS: usize = 5000;

/// The archive shown in Preview: where it is, and what is in it.
struct ArchiveShown {
    fs: FsRef,
    path: PathBuf,
    entries: Vec<Entry>,
}

struct Views {
    root: Retained<NSStackView>,
    /// Expand to fill the window, or shrink back.
    expand: Retained<NSButton>,
    tabs: Retained<NSSegmentedControl>,
    content: Retained<NSView>,
    info: Retained<NSStackView>,
    preview: Retained<NSView>,
    quick_look: Option<Retained<QLPreviewView>>,
    preview_note: Retained<NSTextField>,
    archive: Retained<NSStackView>,
    archive_summary: Retained<NSTextField>,
    archive_table: Retained<NSTableView>,
    activity: Retained<NSScrollView>,
    activity_table: Retained<NSTableView>,
    log: Retained<NSView>,
    log_text: Retained<NSTextView>,
}

/// What is selected: shown in Info and Preview.
#[derive(Clone)]
struct Selection {
    fs: FsRef,
    location: PathBuf,
    entries: Vec<Entry>,
    /// Items in the folder (for "nothing selected").
    count: usize,
}

pub struct PreviewIvars {
    views: OnceCell<Views>,
    tab: Cell<usize>,
    selection: RefCell<Option<Selection>>,
    /// What Info and Preview show now (to skip repeats).
    shown: RefCell<String>,
    /// Counts previews, so a slow download for an old selection is dropped.
    generation: Cell<u64>,
    transfers: RefCell<Vec<LogEntry>>,
    archive: RefCell<Option<ArchiveShown>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzPreviewPane"]
    #[ivars = PreviewIvars]
    pub struct PreviewPane;

    impl PreviewPane {
        #[unsafe(method(tabChanged:))]
        fn tab_changed(&self, sender: &NSSegmentedControl) {
            let tab = usize::try_from(sender.selectedSegment()).unwrap_or(0);
            self.select_tab(tab);
            if let Some((id, _)) = TABS.get(tab) {
                super::update_settings(|s| s.preview.tab = (*id).to_string());
            }
        }

        /// Quick Look's text preview does not set the cursor when the
        /// mouse comes in, so coming from the divider kept the resize
        /// cursor: the arrow is set here.
        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _event: &objc2_app_kit::NSEvent) {
            objc2_app_kit::NSCursor::arrowCursor().set();
        }

        #[unsafe(method(cursorUpdate:))]
        fn cursor_update(&self, _event: &objc2_app_kit::NSEvent) {
            objc2_app_kit::NSCursor::arrowCursor().set();
        }

        /// Double-clicking an item in an archive opens the archive there.
        #[unsafe(method(archiveRowOpened:))]
        fn archive_row_opened(&self, _sender: Option<&AnyObject>) {
            self.open_archive_row();
        }

        #[unsafe(method(expandPane:))]
        fn expand_pane(&self, _sender: Option<&AnyObject>) {
            super::toggle_preview_expanded();
        }

        #[unsafe(method(closePane:))]
        fn close_pane(&self, _sender: Option<&AnyObject>) {
            super::toggle_preview();
        }

        #[unsafe(method(clearLog:))]
        fn clear_log(&self, _sender: Option<&AnyObject>) {
            oplog::clear();
        }

        #[unsafe(method(copyLog:))]
        fn copy_log(&self, _sender: Option<&AnyObject>) {
            let pasteboard = objc2_app_kit::NSPasteboard::generalPasteboard();
            pasteboard.clearContents();
            pasteboard.setString_forType(&ns(&oplog::as_text()), unsafe { objc2_app_kit::NSPasteboardTypeString });
        }
    }

    unsafe impl NSObjectProtocol for PreviewPane {}

    unsafe impl NSControlTextEditingDelegate for PreviewPane {}

    unsafe impl NSTableViewDataSource for PreviewPane {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, table: &NSTableView) -> NSInteger {
            let rows = if self.is_archive_table(table) {
                self.ivars().archive.borrow().as_ref().map_or(0, |a| a.entries.len().min(ARCHIVE_ROWS))
            } else {
                self.ivars().transfers.borrow().len()
            };
            rows as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for PreviewPane {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            if self.is_archive_table(table) { self.archive_cell(column, row) } else { self.transfer_cell(row) }
        }
    }
);

fn view<T: Message + AsRef<NSView>>(v: &T) -> Retained<NSView> {
    v.as_ref().retain()
}

thread_local! {
    /// The text scale (View > Zoom In and Zoom Out; 1 is normal).
    static ZOOM: Cell<f64> = const { Cell::new(1.0) };
}

/// A size at the current zoom.
fn z(size: f64) -> f64 {
    (size * ZOOM.with(Cell::get) * 2.0).round() / 2.0
}

fn small() -> f64 {
    z(NSFont::smallSystemFontSize())
}

fn wrapping(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::wrappingLabelWithString(&ns(text), mtm);
    field.setSelectable(true);
    field.setFont(Some(&NSFont::systemFontOfSize(small())));
    field
}

fn secondary(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&ns(text), mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(small())));
    field.setTextColor(Some(&NSColor::secondaryLabelColor()));
    field
}

/// Pins `child` to all edges of `parent`.
fn fill(parent: &NSView, child: &NSView) {
    pin(parent, child, true);
}

/// Pins `child` to the top and sides of `parent` (and the bottom if asked).
fn pin(parent: &NSView, child: &NSView, bottom: bool) {
    child.setTranslatesAutoresizingMaskIntoConstraints(false);
    parent.addSubview(child);
    child.topAnchor().constraintEqualToAnchor(&parent.topAnchor()).setActive(true);
    if bottom {
        child.bottomAnchor().constraintEqualToAnchor(&parent.bottomAnchor()).setActive(true);
    }
    child.leadingAnchor().constraintEqualToAnchor(&parent.leadingAnchor()).setActive(true);
    child.trailingAnchor().constraintEqualToAnchor(&parent.trailingAnchor()).setActive(true);
}

impl PreviewPane {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PreviewIvars {
            views: OnceCell::new(),
            tab: Cell::new(0),
            selection: RefCell::new(None),
            shown: RefCell::new(String::new()),
            generation: Cell::new(0),
            transfers: RefCell::new(Vec::new()),
            archive: RefCell::new(None),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build(mtm);
        this
    }

    fn views(&self) -> &Views {
        self.ivars().views.get().expect("built")
    }

    pub fn view(&self) -> Retained<NSView> {
        view(&*self.views().root)
    }

    fn build(&self, mtm: MainThreadMarker) {
        let labels: Vec<Retained<NSString>> = TABS.iter().map(|t| ns(t.1)).collect();
        let tabs = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &NSArray::from_retained_slice(&labels),
                NSSegmentSwitchTracking::SelectOne,
                Some(self),
                Some(sel!(tabChanged:)),
                mtm,
            )
        };
        tabs.setSelectedSegment(0);
        tabs.setControlSize(objc2_app_kit::NSControlSize::Small);
        tabs.setFont(Some(&NSFont::systemFontOfSize(NSFont::smallSystemFontSize())));

        let content = NSView::new(mtm);
        content.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Vertical);

        // Info: a column of facts, top aligned.
        let info = NSStackView::new(mtm);
        info.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        info.setAlignment(NSLayoutAttribute::CenterX);
        info.setSpacing(10.0);
        info.setEdgeInsets(NSEdgeInsets { top: 16.0, left: 12.0, bottom: 12.0, right: 12.0 });

        // Preview: Quick Look, with a note when there is nothing to show.
        let preview = NSView::new(mtm);
        let quick_look = unsafe {
            QLPreviewView::initWithFrame_style(
                QLPreviewView::alloc(mtm),
                NSRect::new(objc2_foundation::NSPoint::new(0.0, 0.0), NSSize::new(260.0, 300.0)),
                QLPreviewViewStyle::Normal,
            )
        };
        if let Some(quick_look) = &quick_look {
            unsafe { quick_look.setShouldCloseWithWindow(false) };
            fill(&preview, quick_look);
        }
        {
            use objc2_app_kit::{NSTrackingArea, NSTrackingAreaOptions as O};
            let area = unsafe {
                NSTrackingArea::initWithRect_options_owner_userInfo(
                    <NSTrackingArea as objc2::AnyThread>::alloc(),
                    NSRect::ZERO,
                    O::MouseEnteredAndExited | O::CursorUpdate | O::ActiveInKeyWindow | O::InVisibleRect,
                    Some(self),
                    None,
                )
            };
            preview.addTrackingArea(&area);
        }
        // An archive's contents, in place of Quick Look.
        let archive_summary = secondary("", mtm);
        archive_summary.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        let archive_table = NSTableView::initWithFrame(NSTableView::alloc(mtm), NSRect::ZERO);
        for (id, title, width) in [("path", "Name", 150.0), ("size", "Size", 64.0)] {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
            column.setTitle(&ns(title));
            column.setWidth(width);
            archive_table.addTableColumn(&column);
        }
        archive_table.setColumnAutoresizingStyle(
            objc2_app_kit::NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle,
        );
        archive_table.setStyle(objc2_app_kit::NSTableViewStyle::Plain);
        archive_table.setUsesAlternatingRowBackgroundColors(true);
        unsafe {
            archive_table.setDataSource(Some(ProtocolObject::from_ref(self)));
            archive_table.setDelegate(Some(ProtocolObject::from_ref(self)));
            archive_table.setTarget(Some(self));
            archive_table.setDoubleAction(Some(sel!(archiveRowOpened:)));
        }
        let archive_scroll = NSScrollView::new(mtm);
        archive_scroll.setDocumentView(Some(&archive_table));
        archive_scroll.setHasVerticalScroller(true);
        let archive = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[view(&*archive_summary), view(&*archive_scroll)]),
            mtm,
        );
        archive.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        archive.setAlignment(NSLayoutAttribute::Leading);
        archive.setSpacing(6.0);
        archive.setEdgeInsets(NSEdgeInsets { top: 4.0, left: 8.0, bottom: 0.0, right: 0.0 });
        archive_scroll.leadingAnchor().constraintEqualToAnchor(&archive.leadingAnchor()).setActive(true);
        archive_scroll.trailingAnchor().constraintEqualToAnchor(&archive.trailingAnchor()).setActive(true);
        archive_scroll
            .setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Vertical);
        archive_summary
            .widthAnchor()
            .constraintLessThanOrEqualToAnchor_constant(&archive.widthAnchor(), -16.0)
            .setActive(true);
        fill(&preview, &archive);
        archive.setHidden(true);
        let preview_note = secondary("", mtm);
        preview_note.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        preview_note.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
        preview_note.setTranslatesAutoresizingMaskIntoConstraints(false);
        preview.addSubview(&preview_note);
        preview_note.centerXAnchor().constraintEqualToAnchor(&preview.centerXAnchor()).setActive(true);
        preview_note.centerYAnchor().constraintEqualToAnchor(&preview.centerYAnchor()).setActive(true);
        preview_note
            .widthAnchor()
            .constraintLessThanOrEqualToAnchor_constant(&preview.widthAnchor(), -24.0)
            .setActive(true);

        // Activity: finished transfers, newest first.
        let activity_table = NSTableView::initWithFrame(NSTableView::alloc(mtm), NSRect::ZERO);
        let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns("transfer"));
        column.setResizingMask(objc2_app_kit::NSTableColumnResizingOptions::AutoresizingMask);
        activity_table.addTableColumn(&column);
        activity_table.setHeaderView(None);
        activity_table.setRowHeight(44.0);
        activity_table.setColumnAutoresizingStyle(
            objc2_app_kit::NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle,
        );
        activity_table.setStyle(objc2_app_kit::NSTableViewStyle::Plain);
        unsafe {
            activity_table.setDataSource(Some(ProtocolObject::from_ref(self)));
            activity_table.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        let activity = NSScrollView::new(mtm);
        activity.setDocumentView(Some(&activity_table));
        activity.setHasVerticalScroller(true);

        // Log: all of it as text, with Clear and Copy.
        let log_scroll = NSTextView::scrollableTextView(mtm);
        let log_text = log_scroll
            .documentView()
            .and_then(|v| v.downcast::<NSTextView>().ok())
            .expect("a scrollable text view holds a text view");
        log_text.setEditable(false);
        log_text.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(z(10.5), unsafe {
            objc2_app_kit::NSFontWeightRegular
        })));
        log_text.setTextContainerInset(NSSize::new(6.0, 6.0));
        let clear =
            unsafe { NSButton::buttonWithTitle_target_action(&ns("Clear"), Some(self), Some(sel!(clearLog:)), mtm) };
        let copy =
            unsafe { NSButton::buttonWithTitle_target_action(&ns("Copy"), Some(self), Some(sel!(copyLog:)), mtm) };
        for button in [&clear, &copy] {
            button.setControlSize(objc2_app_kit::NSControlSize::Small);
        }
        let buttons =
            NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&[view(&*clear), view(&*copy)]), mtm);
        buttons.setEdgeInsets(NSEdgeInsets { top: 4.0, left: 8.0, bottom: 6.0, right: 8.0 });
        let log =
            NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&[view(&*log_scroll), view(&*buttons)]), mtm);
        log.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        log.setSpacing(0.0);
        log_scroll.leadingAnchor().constraintEqualToAnchor(&log.leadingAnchor()).setActive(true);
        log_scroll.trailingAnchor().constraintEqualToAnchor(&log.trailingAnchor()).setActive(true);
        log_scroll
            .setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Vertical);

        // The header: the tabs in the middle, a close button on the right.
        let close =
            unsafe { NSButton::buttonWithTitle_target_action(&ns(""), Some(self), Some(sel!(closePane:)), mtm) };
        if let Some(image) = super::util::symbol("xmark", "Close") {
            close.setImage(Some(&image));
        }
        close.setBordered(false);
        close.setToolTip(Some(&ns("Hide the preview pane")));
        close.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        // And on the left, expand to fill the window (or shrink back).
        let expand =
            unsafe { NSButton::buttonWithTitle_target_action(&ns(""), Some(self), Some(sel!(expandPane:)), mtm) };
        expand.setBordered(false);
        expand.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        let header = NSView::new(mtm);
        for control in [view(&*tabs), view(&*close), view(&*expand)] {
            control.setTranslatesAutoresizingMaskIntoConstraints(false);
            header.addSubview(&control);
            control.centerYAnchor().constraintEqualToAnchor(&header.centerYAnchor()).setActive(true);
        }
        tabs.centerXAnchor().constraintEqualToAnchor(&header.centerXAnchor()).setActive(true);
        tabs.topAnchor().constraintEqualToAnchor(&header.topAnchor()).setActive(true);
        tabs.bottomAnchor().constraintEqualToAnchor(&header.bottomAnchor()).setActive(true);
        close.trailingAnchor().constraintEqualToAnchor_constant(&header.trailingAnchor(), -8.0).setActive(true);
        expand.leadingAnchor().constraintEqualToAnchor_constant(&header.leadingAnchor(), 8.0).setActive(true);
        expand.trailingAnchor().constraintLessThanOrEqualToAnchor_constant(&tabs.leadingAnchor(), -4.0).setActive(true);
        close
            .leadingAnchor()
            .constraintGreaterThanOrEqualToAnchor_constant(&tabs.trailingAnchor(), 4.0)
            .setActive(true);

        let root =
            NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&[view(&*header), view(&*content)]), mtm);
        root.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        root.setAlignment(NSLayoutAttribute::CenterX);
        root.setSpacing(8.0);
        root.setEdgeInsets(NSEdgeInsets { top: 8.0, left: 0.0, bottom: 0.0, right: 0.0 });
        for v in [view(&*content), header.clone()] {
            v.leadingAnchor().constraintEqualToAnchor(&root.leadingAnchor()).setActive(true);
            v.trailingAnchor().constraintEqualToAnchor(&root.trailingAnchor()).setActive(true);
        }
        root.widthAnchor().constraintGreaterThanOrEqualToConstant(220.0).setActive(true);

        let _ = self.ivars().views.set(Views {
            root,
            expand,
            tabs,
            content,
            info: info.clone(),
            preview,
            quick_look,
            preview_note,
            archive,
            archive_summary,
            archive_table,
            activity,
            activity_table,
            log: Retained::into_super(log),
            log_text,
        });
        self.select_tab(0);
        self.set_expanded(false);
    }

    /// Scales the text (1 is normal); Quick Look previews keep their size.
    pub fn set_zoom(&self, zoom: f64) {
        if (ZOOM.with(Cell::get) - zoom).abs() < 0.01 {
            return;
        }
        ZOOM.with(|z| z.set(zoom));
        let views = self.views();
        views.activity_table.setRowHeight(z(44.0));
        views.archive_table.setRowHeight(z(17.0));
        views.log_text.setFont(Some(&NSFont::monospacedSystemFontOfSize_weight(z(10.5), unsafe {
            objc2_app_kit::NSFontWeightRegular
        })));
        views.archive_summary.setFont(Some(&NSFont::systemFontOfSize(small())));
        views.preview_note.setFont(Some(&NSFont::systemFontOfSize(small())));
        views.archive_table.reloadData();
        views.activity_table.reloadData();
        // Info is made again at the new size.
        self.ivars().shown.borrow_mut().clear();
        self.refresh();
    }

    /// Shows whether the pane fills the window (the button's icon).
    pub fn set_expanded(&self, expanded: bool) {
        let button = &self.views().expand;
        let (name, tip) = if expanded {
            ("arrow.down.right.and.arrow.up.left", "Shrink the preview pane back")
        } else {
            ("arrow.up.left.and.arrow.down.right", "Expand the preview pane to fill the window")
        };
        if let Some(image) = super::util::symbol(name, tip) {
            button.setImage(Some(&image));
        }
        button.setToolTip(Some(&ns(tip)));
    }

    pub fn select_tab_named(&self, id: &str) {
        let index = TABS.iter().position(|t| t.0 == id).unwrap_or(0);
        self.views().tabs.setSelectedSegment(index as isize);
        self.select_tab(index);
    }

    fn select_tab(&self, index: usize) {
        self.ivars().tab.set(index);
        let views = self.views();
        for child in views.content.subviews().iter() {
            child.removeFromSuperview();
        }
        let shown: Retained<NSView> = match index {
            1 => views.preview.clone(),
            2 => view(&*views.activity),
            3 => views.log.clone(),
            _ => view(&*views.info),
        };
        // Info sits at the top, not stretched.
        pin(&views.content, &shown, index != 0);
        self.ivars().shown.borrow_mut().clear();
        self.refresh();
    }

    fn is_visible(&self) -> bool {
        !self.views().root.isHidden()
    }

    /// The selection changed (or the pane it is in).
    pub fn set_selection(&self, fs: FsRef, location: PathBuf, entries: Vec<Entry>, count: usize) {
        *self.ivars().selection.borrow_mut() = Some(Selection { fs, location, entries, count });
        self.refresh();
    }

    /// Shows what the current tab shows, if the pane is visible.
    pub fn refresh(&self) {
        if !self.is_visible() {
            return;
        }
        match self.ivars().tab.get() {
            0 => self.show_info(),
            1 => self.show_preview(),
            2 => self.show_activity(),
            _ => self.show_log(),
        }
    }

    pub fn log_changed(&self) {
        if self.is_visible() && self.ivars().tab.get() >= 2 {
            self.refresh();
        }
    }

    fn selection_key(selection: &Selection, tab: usize) -> String {
        let paths: Vec<String> = selection.entries.iter().map(|e| e.path.display().to_string()).collect();
        format!("{tab}|{}|{}|{}", selection.fs.key(), selection.location.display(), paths.join("|"))
    }

    fn show_info(&self) {
        let Some(selection) = self.ivars().selection.borrow().clone() else { return };
        let key = Self::selection_key(&selection, 0)
            + &selection.entries.iter().map(|e| format!("{:?}{}", e.modified, e.size)).collect::<String>();
        if *self.ivars().shown.borrow() == key {
            return;
        }
        *self.ivars().shown.borrow_mut() = key;
        let mtm = self.mtm();
        let info = &self.views().info;
        for child in info.arrangedSubviews().iter() {
            info.removeArrangedSubview(&child);
            child.removeFromSuperview();
        }
        let local = selection.fs.is_local();
        let mut facts: Vec<(&str, String)> = Vec::new();
        let (icon, title) = match selection.entries.as_slice() {
            [] => {
                let name = crate::pane::title_of(&selection.location);
                facts.push(("Items", selection.count.to_string()));
                facts.push(("Where", selection.fs.display_path(&selection.location)));
                (super::util::folder_icon(), name)
            }
            [entry] => {
                facts.push(("Kind", kind_of(entry)));
                if !entry.is_folder_like() {
                    facts.push((
                        "Size",
                        format!("{} ({} bytes)", fs::format_size(entry.size), group_digits(entry.size)),
                    ));
                }
                facts.push(("Modified", format_date(entry.modified)));
                if let Some(mode) = entry.permissions {
                    facts.push(("Permissions", format!("{} ({:o})", fs::format_permissions(entry), mode & 0o7777)));
                }
                if let Some(owner) = &entry.owner {
                    facts.push(("Owner", owner.clone()));
                }
                if let Some(group) = &entry.group {
                    facts.push(("Group", group.clone()));
                }
                let parent = entry.path.parent().unwrap_or(Path::new("/"));
                facts.push(("Where", selection.fs.display_path(parent)));
                (icon_of(entry, local), entry.name.clone())
            }
            many => {
                let files: Vec<&Entry> = many.iter().filter(|e| !e.is_folder_like()).collect();
                let folders = many.len() - files.len();
                let total: u64 = files.iter().map(|e| e.size).sum();
                facts.push(("Files", files.len().to_string()));
                facts.push(("Folders", folders.to_string()));
                facts.push(("Size", format!("{} (files only)", fs::format_size(total))));
                facts.push(("Where", selection.fs.display_path(&selection.location)));
                (icon_of(&many[0], local), format!("{} items", many.len()))
            }
        };
        let image = NSImageView::imageViewWithImage(&icon, mtm);
        image.widthAnchor().constraintEqualToConstant(z(64.0)).setActive(true);
        image.heightAnchor().constraintEqualToConstant(z(64.0)).setActive(true);
        image.setImageScaling(objc2_app_kit::NSImageScaling::ScaleProportionallyUpOrDown);
        let name = NSTextField::wrappingLabelWithString(&ns(&title), mtm);
        name.setFont(Some(&NSFont::boldSystemFontOfSize(z(13.0))));
        name.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        name.setSelectable(true);
        let rows: Vec<Retained<NSArray<NSView>>> = facts
            .iter()
            .map(|(label, value)| {
                let value = wrapping(value, mtm);
                value.setPreferredMaxLayoutWidth(170.0);
                NSArray::from_retained_slice(&[view(&*secondary(&format!("{label}:"), mtm)), view(&*value)])
            })
            .collect();
        let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
        grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
        grid.setRowSpacing(4.0);
        grid.setColumnSpacing(6.0);
        info.addArrangedSubview(&image);
        info.addArrangedSubview(&name);
        info.addArrangedSubview(&grid);
        name.widthAnchor().constraintLessThanOrEqualToAnchor_constant(&info.widthAnchor(), -24.0).setActive(true);
        name.setPreferredMaxLayoutWidth(230.0);
    }

    fn set_preview(&self, path: Option<&Path>, note: &str) {
        let views = self.views();
        if let Some(quick_look) = &views.quick_look {
            let item = path.map(|p| ProtocolObject::<dyn QLPreviewItem>::from_retained(file_url(p)));
            unsafe { quick_look.setPreviewItem(item.as_deref()) };
            quick_look.setHidden(path.is_none());
        }
        views.preview_note.setStringValue(&ns(note));
        views.preview_note.setHidden(path.is_some());
        views.archive.setHidden(true);
        *self.ivars().archive.borrow_mut() = None;
    }

    fn is_archive_table(&self, table: &NSTableView) -> bool {
        // Asked while the views are still being made: not yet.
        self.ivars().views.get().is_some_and(|v| std::ptr::eq(table, &*v.archive_table))
    }

    /// Lists an archive (on this Mac, or fetched) in the background.
    fn preview_archive(&self, fs: FsRef, entry: Entry, generation: u64) {
        if !fs.is_local() && entry.size > REMOTE_ARCHIVE_LIMIT {
            let size = fs::format_size(entry.size);
            self.set_preview(
                None,
                &format!("{size} is too big to list from the server. Open the archive to browse it."),
            );
            return;
        }
        self.set_preview(None, "Reading the archive\u{2026}");
        std::thread::spawn(move || {
            let result = (|| {
                let local = if fs.is_local() { entry.path.clone() } else { fetch(&fs, &entry, generation)? };
                let archive = crate::vfs::archive::ArchiveFs::open(&local, None)?;
                Ok::<_, std::io::Error>((archive.summary(), archive.all_entries()))
            })();
            DispatchQueue::main().exec_async(move || {
                let Some(pane) = super::preview_pane() else { return };
                if pane.ivars().generation.get() != generation {
                    return;
                }
                match result {
                    Ok((summary, entries)) => pane.show_archive(fs, entry.path, summary, entries),
                    Err(e) => pane.set_preview(None, &format!("This archive cannot be read: {e}")),
                }
            });
        });
    }

    fn show_archive(&self, fs: FsRef, path: PathBuf, summary: (usize, usize, u64), entries: Vec<Entry>) {
        self.set_preview(None, "");
        let views = self.views();
        views.preview_note.setHidden(true);
        let (files, folders, size) = summary;
        let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut text = format!(
            "{}, {}, {} unpacked",
            plural(files, "file", "files"),
            plural(folders, "folder", "folders"),
            fs::format_size(size)
        );
        if entries.len() > ARCHIVE_ROWS {
            text.push_str(&format!(" (the first {ARCHIVE_ROWS} shown)"));
        }
        views.archive_summary.setStringValue(&ns(&text));
        views.archive_summary.setToolTip(Some(&ns("Double-click an item to open the archive there.")));
        *self.ivars().archive.borrow_mut() = Some(ArchiveShown { fs, path, entries });
        views.archive.setHidden(false);
        views.archive_table.reloadData();
        // The name column takes what the size column leaves.
        views.archive.layoutSubtreeIfNeeded();
        views.archive_table.sizeToFit();
        views.archive_table.scrollRowToVisible(0);
    }

    fn archive_cell(&self, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
        let archive = self.ivars().archive.borrow();
        let entry = archive.as_ref()?.entries.get(usize::try_from(row).ok()?)?;
        let size_column = column?.identifier().to_string() == "size";
        let text = match (size_column, entry.is_folder_like()) {
            (true, true) => "--".to_string(),
            (true, false) => fs::format_size(entry.size),
            (false, folder) => {
                let path = entry.path.to_string_lossy();
                let path = path.trim_start_matches('/');
                if folder { format!("{path}/") } else { path.to_string() }
            }
        };
        let field = NSTextField::labelWithString(&ns(&text), self.mtm());
        field.setFont(Some(&NSFont::systemFontOfSize(small())));
        if size_column {
            field.setAlignment(objc2_app_kit::NSTextAlignment::Right);
            field.setTextColor(Some(&NSColor::secondaryLabelColor()));
        } else {
            field.setLineBreakMode(NSLineBreakMode::ByTruncatingHead);
            field.setToolTip(Some(&ns(&text)));
        }
        Some(Retained::into_super(Retained::into_super(field)))
    }

    fn open_archive_row(&self) {
        let row = self.views().archive_table.clickedRow();
        let opened = {
            let archive = self.ivars().archive.borrow();
            archive.as_ref().and_then(|a| {
                let entry = a.entries.get(usize::try_from(row).ok()?)?;
                Some((a.fs.clone(), a.path.clone(), entry.path.clone()))
            })
        };
        if let Some((fs, path, inner)) = opened {
            super::open_archive(super::active_index(), fs, path, Some(inner));
        }
    }

    fn show_preview(&self) {
        let Some(selection) = self.ivars().selection.borrow().clone() else { return };
        let key = Self::selection_key(&selection, 1);
        if *self.ivars().shown.borrow() == key {
            return;
        }
        *self.ivars().shown.borrow_mut() = key;
        let generation = self.ivars().generation.get() + 1;
        self.ivars().generation.set(generation);
        let Some(entry) = selection.entries.first().cloned() else {
            self.set_preview(None, "Select a file to preview it.");
            return;
        };
        if !entry.is_folder_like() && crate::vfs::archive::is_archive_name(&entry.name) {
            self.preview_archive(selection.fs.clone(), entry, generation);
            return;
        }
        if selection.fs.is_local() {
            self.set_preview(Some(&entry.path), "");
            return;
        }
        if entry.is_folder_like() {
            self.set_preview(
                None,
                if selection.fs.read_only() {
                    "Folders in archives have no preview."
                } else {
                    "Folders on servers have no preview."
                },
            );
            return;
        }
        if entry.size > REMOTE_PREVIEW_LIMIT {
            self.set_preview(None, &format!("{} is too big to preview from the server.", fs::format_size(entry.size)));
            return;
        }
        self.set_preview(None, "Loading the preview\u{2026}");
        let fs = selection.fs.clone();
        std::thread::spawn(move || {
            let result = fetch(&fs, &entry, generation);
            DispatchQueue::main().exec_async(move || {
                let Some(pane) = super::preview_pane() else { return };
                if pane.ivars().generation.get() != generation {
                    return;
                }
                match result {
                    Ok(path) => pane.set_preview(Some(&path), ""),
                    Err(e) => pane.set_preview(None, &format!("No preview: {e}")),
                }
            });
        });
    }

    fn show_activity(&self) {
        let transfers: Vec<LogEntry> =
            oplog::entries().into_iter().rev().filter(|e| e.kind == Kind::Transfer).collect();
        *self.ivars().transfers.borrow_mut() = transfers;
        self.views().activity_table.reloadData();
    }

    fn transfer_cell(&self, row: NSInteger) -> Option<Retained<NSView>> {
        let transfers = self.ivars().transfers.borrow();
        let entry = transfers.get(usize::try_from(row).ok()?)?;
        let mtm = self.mtm();
        let title = NSTextField::labelWithString(&ns(&entry.text), mtm);
        title.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        title.setFont(Some(&NSFont::systemFontOfSize(z(12.0))));
        if entry.failed {
            title.setTextColor(Some(&NSColor::systemRedColor()));
        }
        let when = crate::rename::format_date("hh:mm:ss", entry.time);
        let detail_text = if entry.detail.is_empty() {
            when
        } else {
            format!("{when}  {}", entry.detail.lines().next().unwrap_or(""))
        };
        let detail = secondary(&detail_text, mtm);
        detail.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        title.setToolTip(Some(&ns(&format!("{}\n{}", entry.text, entry.detail))));
        let stack =
            NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&[view(&*title), view(&*detail)]), mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        stack.setAlignment(NSLayoutAttribute::Leading);
        stack.setSpacing(2.0);
        stack.setEdgeInsets(NSEdgeInsets { top: 4.0, left: 8.0, bottom: 4.0, right: 8.0 });
        for field in [&title, &detail] {
            field.setContentCompressionResistancePriority_forOrientation(
                1.0,
                objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
            );
        }
        Some(Retained::into_super(stack))
    }

    fn show_log(&self) {
        let views = self.views();
        let text = oplog::as_text();
        let text = if text.is_empty() {
            "Nothing yet: transfers, connections, and changes appear here.".to_string()
        } else {
            text
        };
        views.log_text.setString(&ns(&text));
        let length = views.log_text.string().length();
        views.log_text.scrollRangeToVisible(objc2_foundation::NSRange::new(length, 0));
    }
}

/// 1234567 -> "1,234,567".
fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Downloads a small server file for Quick Look, to a folder that is
/// emptied when forkz starts.
fn fetch(fs: &FsRef, entry: &Entry, generation: u64) -> std::io::Result<PathBuf> {
    let folder = cache_dir().join(generation.to_string());
    std::fs::create_dir_all(&folder)?;
    let path = folder.join(&entry.name);
    let mut reader = fs.open_read(&entry.path)?;
    let mut file = std::fs::File::create(&path)?;
    std::io::copy(&mut reader, &mut file)?;
    Ok(path)
}

pub fn cache_dir() -> PathBuf {
    std::env::temp_dir().join(format!("forkz-preview-{}", std::process::id()))
}

/// Removes the previews fetched by this and earlier runs.
pub fn clean_cache() {
    let Ok(read) = std::fs::read_dir(std::env::temp_dir()) else { return };
    for entry in read.flatten() {
        if entry.file_name().to_string_lossy().starts_with("forkz-preview-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn digit_groups() {
        assert_eq!(super::group_digits(0), "0");
        assert_eq!(super::group_digits(999), "999");
        assert_eq!(super::group_digits(1000), "1,000");
        assert_eq!(super::group_digits(1234567), "1,234,567");
    }
}
