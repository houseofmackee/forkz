//! Disk Usage (Commands menu): a folder and everything in it as a tree,
//! largest first, with each item's share of its folder, in a window of its
//! own. Items can be shown in the pane or, on this Mac, moved to the Trash.

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSColor, NSControlTextEditingDelegate, NSFont, NSLevelIndicator, NSLevelIndicatorStyle, NSLineBreakMode,
    NSOutlineView, NSOutlineViewDataSource, NSOutlineViewDelegate, NSTableColumn, NSTableViewDelegate, NSTextAlignment,
    NSTextField, NSView, NSWindowDelegate,
};
use objc2_foundation::{NSInteger, NSNotification, NSNumber, NSObject, NSObjectProtocol, NSSize};

use super::diff_window::{column, find, layout, text_cell, window};
use super::util::{mtm, ns, small_label};
use crate::fs::format_size;
use crate::fs::ops::{CancelToken, Job, JobKind};
use crate::fs::usage::{self, Usage};
use crate::vfs::FsRef;

thread_local! {
    static WINDOWS: RefCell<Vec<Retained<DiskUsage>>> = const { RefCell::new(Vec::new()) };
}

struct Parts {
    outline: Retained<NSOutlineView>,
    status: Retained<NSTextField>,
    trash: Retained<NSButton>,
}

pub struct DiskUsageIvars {
    parts: OnceCell<Parts>,
    place: OnceCell<(FsRef, PathBuf)>,
    usage: RefCell<Usage>,
    /// Each node's folder (None for the top).
    parents: RefCell<Vec<Option<usize>>>,
    cancel: RefCell<Option<CancelToken>>,
    generation: Cell<u64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzDiskUsage"]
    #[ivars = DiskUsageIvars]
    struct DiskUsage;

    impl DiskUsage {
        #[unsafe(method(measureAgain:))]
        fn measure_again(&self, _sender: Option<&AnyObject>) {
            self.measure();
        }

        #[unsafe(method(showInPane:))]
        fn show_in_pane(&self, _sender: Option<&AnyObject>) {
            self.show_selected();
        }

        #[unsafe(method(trashSelected:))]
        fn trash_selected(&self, _sender: Option<&AnyObject>) {
            self.trash();
        }

        /// Double-click: the item, selected in the active pane.
        #[unsafe(method(openRow:))]
        fn open_row(&self, _sender: Option<&AnyObject>) {
            self.show_selected();
        }
    }

    unsafe impl NSObjectProtocol for DiskUsage {}

    unsafe impl NSControlTextEditingDelegate for DiskUsage {}

    unsafe impl NSWindowDelegate for DiskUsage {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            if let Some(cancel) = self.ivars().cancel.borrow_mut().take() {
                cancel.cancel();
            }
            let me = self as *const DiskUsage as usize;
            DispatchQueue::main().exec_async(move || {
                WINDOWS.with(|v| v.borrow_mut().retain(|x| Retained::as_ptr(x) as usize != me));
            });
        }
    }

    unsafe impl NSOutlineViewDataSource for DiskUsage {
        #[unsafe(method(outlineView:numberOfChildrenOfItem:))]
        fn number_of_children(&self, _outline: &NSOutlineView, item: Option<&AnyObject>) -> NSInteger {
            self.children_of(item).len() as NSInteger
        }

        #[unsafe(method_id(outlineView:child:ofItem:))]
        fn child(&self, _outline: &NSOutlineView, index: NSInteger, item: Option<&AnyObject>) -> Retained<AnyObject> {
            let child = self.children_of(item).get(index as usize).copied().unwrap_or(0);
            Retained::into_super(Retained::into_super(Retained::into_super(NSNumber::new_usize(child))))
        }

        #[unsafe(method(outlineView:isItemExpandable:))]
        fn is_expandable(&self, _outline: &NSOutlineView, item: &AnyObject) -> bool {
            let usage = self.ivars().usage.borrow();
            node_of(Some(item)).and_then(|n| usage.nodes.get(n)).is_some_and(|n| !n.children.is_empty())
        }
    }

    unsafe impl NSTableViewDelegate for DiskUsage {}

    unsafe impl NSOutlineViewDelegate for DiskUsage {
        #[unsafe(method_id(outlineView:viewForTableColumn:item:))]
        fn view_for_item(
            &self,
            outline: &NSOutlineView,
            column: Option<&NSTableColumn>,
            item: &AnyObject,
        ) -> Option<Retained<NSView>> {
            let id = column.map(|c| c.identifier().to_string()).unwrap_or_default();
            node_of(Some(item)).and_then(|node| self.cell(outline, &id, node))
        }
    }
);

/// An icon and the name.
fn name_label(node: &usage::Node, local: bool, font: &NSFont) -> Retained<objc2_foundation::NSMutableAttributedString> {
    use objc2_app_kit::{NSAttributedStringAttachmentConveniences, NSTextAttachment};
    use objc2_foundation::{NSAttributedString, NSMutableAttributedString, NSPoint, NSRange, NSRect};
    let icon = if node.summary {
        super::util::symbol("ellipsis.circle", "Smaller items").unwrap_or_else(super::util::folder_icon)
    } else if node.folder {
        super::util::folder_icon()
    } else if local {
        super::util::icon_for_path(&node.path)
    } else {
        super::util::symbol("doc", "File").unwrap_or_else(super::util::folder_icon)
    };
    let copy: Retained<objc2_app_kit::NSImage> = unsafe { msg_send![&icon, copy] };
    let side = 16.0;
    copy.setSize(NSSize::new(side, side));
    let attachment = NSTextAttachment::new();
    attachment.setImage(Some(&copy));
    attachment
        .setBounds(NSRect::new(NSPoint::new(0.0, ((font.capHeight() - side) / 2.0).round()), NSSize::new(side, side)));
    let label = NSMutableAttributedString::new();
    label.appendAttributedString(&NSAttributedString::attributedStringWithAttachment(&attachment));
    let text = NSMutableAttributedString::new();
    text.replaceCharactersInRange_withString(NSRange::new(0, 0), &ns(&format!(" {}", node.name)));
    label.appendAttributedString(&text);
    let all = NSRange::new(0, label.length());
    unsafe { label.addAttribute_value_range(objc2_app_kit::NSFontAttributeName, font, all) };
    label
}

/// The node an outline item stands for.
fn node_of(item: Option<&AnyObject>) -> Option<usize> {
    item.and_then(|i| i.downcast_ref::<NSNumber>()).map(|n| n.as_usize())
}

/// Measures a folder (the selected one, or the pane's) in a new window.
pub fn open(fs: FsRef, root: PathBuf) {
    let mtm = mtm();
    let this = DiskUsage::alloc(mtm).set_ivars(DiskUsageIvars {
        parts: OnceCell::new(),
        place: OnceCell::new(),
        usage: RefCell::new(Usage::default()),
        parents: RefCell::new(Vec::new()),
        cancel: RefCell::new(None),
        generation: Cell::new(0),
    });
    let this: Retained<DiskUsage> = unsafe { msg_send![super(this), init] };
    let local = fs.is_local();
    this.build(&root, local, mtm);
    let _ = this.ivars().place.set((fs, root));
    WINDOWS.with(|v| v.borrow_mut().push(this.clone()));
    this.measure();
}

impl DiskUsage {
    fn parts(&self) -> &Parts {
        self.ivars().parts.get().expect("built")
    }

    fn build(&self, root: &Path, local: bool, mtm: MainThreadMarker) {
        let name = crate::pane::title_of(root);
        let window = window(&format!("Disk Usage of {name}"), NSSize::new(860.0, 620.0), mtm);
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        let target: &AnyObject = self.as_ref();
        let button = |title: &str, action| unsafe {
            NSButton::buttonWithTitle_target_action(&ns(title), Some(target), Some(action), mtm)
        };
        let show = button("Show in Pane", sel!(showInPane:));
        let trash = button("Move to Trash", sel!(trashSelected:));
        trash.setEnabled(false);
        trash.setHidden(!local);
        let again = button("Measure Again", sel!(measureAgain:));
        let status = small_label("Measuring\u{2026}", mtm);
        status.setTextColor(Some(&NSColor::secondaryLabelColor()));
        status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        status.setContentCompressionResistancePriority_forOrientation(
            1.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );

        let outline = NSOutlineView::new(mtm);
        let name_column = column(&outline, "name", "Name", 380.0, false);
        column(&outline, "size", "Size", 90.0, false);
        column(&outline, "share", "Share of Folder", 150.0, false);
        column(&outline, "files", "Files", 90.0, false);
        unsafe { outline.setOutlineTableColumn(Some(&name_column)) };
        outline.setColumnAutoresizingStyle(
            objc2_app_kit::NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle,
        );
        outline.setRowHeight(22.0);
        outline.setStyle(objc2_app_kit::NSTableViewStyle::Plain);
        outline.setUsesAlternatingRowBackgroundColors(true);
        unsafe {
            outline.setDataSource(Some(ProtocolObject::from_ref(self)));
            outline.setDelegate(Some(ProtocolObject::from_ref(self)));
            outline.setTarget(Some(target));
            outline.setDoubleAction(Some(sel!(openRow:)));
        }
        layout(&window, &[&*show as &NSView, &*trash as &NSView, &*again as &NSView, &*status as &NSView], &outline);
        window.center();
        let _ = self.ivars().parts.set(Parts { outline, status, trash });
        window.makeKeyAndOrderFront(None);
    }

    /// Measures the folder (again), in the background.
    fn measure(&self) {
        let Some((fs, root)) = self.ivars().place.get().cloned() else { return };
        if let Some(previous) = self.ivars().cancel.borrow_mut().take() {
            previous.cancel();
        }
        let cancel = CancelToken::default();
        *self.ivars().cancel.borrow_mut() = Some(cancel.clone());
        let generation = self.ivars().generation.get() + 1;
        self.ivars().generation.set(generation);
        let parts = self.parts();
        parts.status.setStringValue(&ns("Measuring\u{2026}"));
        parts.trash.setEnabled(false);
        let hidden = super::current_settings().is_some_and(|s| s.show_hidden);
        let me = self as *const DiskUsage as usize;
        std::thread::spawn(move || {
            // Servers get a connection of their own, so browsing stays quick.
            let fs = if fs.is_local() { fs.clone() } else { fs.connect_another().unwrap_or(fs) };
            let mut last = Instant::now();
            let mut progress = |files: u64| {
                if last.elapsed() < Duration::from_millis(200) {
                    return;
                }
                last = Instant::now();
                DispatchQueue::main().exec_async(move || {
                    if let Some(this) = find(&WINDOWS, me)
                        && this.ivars().generation.get() == generation
                    {
                        this.parts().status.setStringValue(&ns(&format!("Measuring\u{2026} {files} files so far")));
                    }
                });
            };
            let result = usage::measure(&fs, &root, hidden, &cancel, &mut progress);
            DispatchQueue::main().exec_async(move || {
                let Some(this) = find(&WINDOWS, me) else { return };
                if this.ivars().generation.get() != generation {
                    return;
                }
                this.ivars().cancel.borrow_mut().take();
                match result {
                    Ok(usage) => this.show(usage),
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        this.parts().status.setStringValue(&ns(&format!("The folder could not be measured: {e}")))
                    }
                }
            });
        });
    }

    fn show(&self, usage: Usage) {
        let mut parents = vec![None; usage.nodes.len()];
        for (index, node) in usage.nodes.iter().enumerate() {
            for child in &node.children {
                parents[*child] = Some(index);
            }
        }
        *self.ivars().parents.borrow_mut() = parents;
        *self.ivars().usage.borrow_mut() = usage;
        let parts = self.parts();
        parts.outline.reloadData();
        // The top level open.
        unsafe { parts.outline.expandItem(Some(&NSNumber::new_usize(0))) };
        parts.trash.setEnabled(true);
        self.update_status();
    }

    fn update_status(&self) {
        let usage = self.ivars().usage.borrow();
        let Some(top) = usage.nodes.first() else { return };
        let files = if top.files == 1 { "1 file".to_string() } else { format!("{} files", top.files) };
        let mut text = format!("{} in {files}", format_size(top.size));
        if usage.unreadable > 0 {
            text.push_str(&format!(" ({} folders could not be read)", usage.unreadable));
        }
        self.parts().status.setStringValue(&ns(&text));
    }

    /// The top shows the measured folder itself.
    fn children_of(&self, item: Option<&AnyObject>) -> Vec<usize> {
        let usage = self.ivars().usage.borrow();
        match node_of(item) {
            None if usage.nodes.is_empty() => Vec::new(),
            None => vec![0],
            Some(node) => usage.nodes.get(node).map(|n| n.children.clone()).unwrap_or_default(),
        }
    }

    fn selected(&self) -> Option<usize> {
        let outline = &self.parts().outline;
        let row = outline.selectedRow();
        if row < 0 {
            return None;
        }
        node_of(outline.itemAtRow(row).as_deref())
    }

    fn show_selected(&self) {
        let Some(node) = self.selected() else { return };
        let Some((fs, _)) = self.ivars().place.get().cloned() else { return };
        let (path, summary) = {
            let usage = self.ivars().usage.borrow();
            let n = &usage.nodes[node];
            (n.path.clone(), n.summary)
        };
        if summary {
            // Its folder.
            super::navigate_active_pane(fs, path, &[]);
        } else {
            let folder = path.parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
            super::navigate_active_pane(fs, folder, &[path]);
        }
    }

    /// Moves the selected item to the Trash (on this Mac), and takes it out
    /// of the tree.
    fn trash(&self) {
        let Some(node) = self.selected().filter(|n| *n != 0) else {
            objc2_app_kit::NSBeep();
            return;
        };
        let (path, name, size, files, summary) = {
            let usage = self.ivars().usage.borrow();
            let n = &usage.nodes[node];
            (n.path.clone(), n.name.clone(), n.size, n.files, n.summary)
        };
        if summary {
            objc2_app_kit::NSBeep();
            return;
        }
        if !super::dialogs::confirm(
            &format!("Move \u{201c}{name}\u{201d} to the Trash?"),
            &format!("It takes {}.", format_size(size)),
            "Move to Trash",
            false,
        ) {
            return;
        }
        let folder = path.parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
        super::start_job(Job::local(JobKind::Trash, vec![path], folder));
        {
            let parents = self.ivars().parents.borrow();
            let mut usage = self.ivars().usage.borrow_mut();
            let mut up = parents[node];
            if let Some(parent) = up {
                usage.nodes[parent].children.retain(|c| *c != node);
            }
            while let Some(ancestor) = up {
                let n = &mut usage.nodes[ancestor];
                n.size = n.size.saturating_sub(size);
                n.files = n.files.saturating_sub(files);
                up = parents[ancestor];
            }
        }
        self.parts().outline.reloadData();
        self.update_status();
    }

    fn cell(&self, outline: &NSOutlineView, id: &str, node: usize) -> Option<Retained<NSView>> {
        let usage = self.ivars().usage.borrow();
        let n = usage.nodes.get(node)?;
        let parent = self.ivars().parents.borrow().get(node).copied().flatten();
        let share = match parent.and_then(|p| usage.nodes.get(p)) {
            Some(p) if p.size > 0 => n.size as f64 * 100.0 / p.size as f64,
            Some(_) => 0.0,
            None => 100.0,
        };
        if id == "share" {
            let bar = NSLevelIndicator::new(self.mtm());
            bar.setLevelIndicatorStyle(NSLevelIndicatorStyle::ContinuousCapacity);
            bar.setMinValue(0.0);
            bar.setMaxValue(100.0);
            bar.setDoubleValue(share);
            bar.setWarningValue(101.0);
            bar.setCriticalValue(101.0);
            bar.setToolTip(Some(&ns(&format!("{share:.1}% of its folder"))));
            return Some(Retained::into_super(Retained::into_super(bar)));
        }
        let font = NSFont::systemFontOfSize(NSFont::systemFontSize());
        let field = text_cell(outline, id, &font);
        if id == "name" {
            let local = self.ivars().place.get().is_some_and(|(fs, _)| fs.is_local());
            let color = if n.summary { NSColor::secondaryLabelColor() } else { NSColor::labelColor() };
            field.setTextColor(Some(&color));
            field.setAttributedStringValue(&name_label(n, local, &font));
            return Some(Retained::into_super(Retained::into_super(field)));
        }
        let text = match id {
            "size" => format_size(n.size),
            _ => {
                if n.folder || n.summary {
                    n.files.to_string()
                } else {
                    String::new()
                }
            }
        };
        field.setAlignment(NSTextAlignment::Right);
        let color = if n.summary { NSColor::secondaryLabelColor() } else { NSColor::labelColor() };
        field.setTextColor(Some(&color));
        field.setStringValue(&ns(&text));
        Some(Retained::into_super(Retained::into_super(field)))
    }
}
