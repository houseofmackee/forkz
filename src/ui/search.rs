//! Search (Shift-Command-F): asks what to look for in the active pane's
//! folder, searches in the background, and shows what it found in the
//! pane as a folder of results.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::{MainThreadOnly, Message};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSButton, NSControlStateValueOff, NSControlStateValueOn,
    NSGridCellPlacement, NSGridView, NSPopUpButton, NSTextField, NSView,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};

use super::oplog;
use super::pane_view::PaneController;
use super::util::{mtm, ns};
use crate::fs::search::{self, Mode, Query};
use crate::pane::title_of;
use crate::vfs::{self, FsRef};

struct Last {
    query: Query,
    spotlight: bool,
}

thread_local! {
    static LAST: RefCell<Last> = const { RefCell::new(Last {
        query: Query { text: String::new(), mode: Mode::Name, include_hidden: false },
        spotlight: true,
    }) };
}

fn view<T: objc2::Message + AsRef<NSView>>(v: &T) -> Retained<NSView> {
    v.as_ref().retain()
}

/// Asks what to search for. `local`: Spotlight can be used.
fn ask(folder: &str, local: bool) -> Option<(Query, bool)> {
    let mtm = mtm();
    let (last_query, last_spotlight) = LAST.with(|l| (l.borrow().query.clone(), l.borrow().spotlight));
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(&format!("Search in \u{201c}{folder}\u{201d}")));
    alert.setInformativeText(&ns(
        "Searches the folder and everything in it. For names, * matches any text and ? one character; separate several with ;.",
    ));
    alert.addButtonWithTitle(&ns("Search"));
    alert.addButtonWithTitle(&ns("Cancel"));
    let field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 24.0)),
    );
    field.setStringValue(&ns(&last_query.text));
    field.setPlaceholderString(Some(&ns("report, *.pdf, or text in files")));
    field.widthAnchor().constraintEqualToConstant(300.0).setActive(true);
    let mode = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    let titles: Vec<Retained<NSString>> = ["Names", "Contents of files"].iter().map(|t| ns(t)).collect();
    mode.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
    mode.selectItemAtIndex(if last_query.mode == Mode::Contents { 1 } else { 0 });
    let checkbox = |title: &str, on: bool| {
        let button = unsafe { NSButton::checkboxWithTitle_target_action(&ns(title), None, None, mtm) };
        button.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        button
    };
    let hidden = checkbox("Include hidden items", last_query.include_hidden);
    let spotlight = checkbox("Use Spotlight (fast, for indexed folders)", local && last_spotlight);
    spotlight.setEnabled(local);
    let label = |text: &str| Retained::into_super(Retained::into_super(NSTextField::labelWithString(&ns(text), mtm)));
    let rows: Vec<Retained<NSArray<NSView>>> = [
        (label("Find:"), view(&*field)),
        (label("Look in:"), view(&*mode)),
        (label(""), view(&*hidden)),
        (label(""), view(&*spotlight)),
    ]
    .into_iter()
    .map(|(l, c)| NSArray::from_retained_slice(&[l, c]))
    .collect();
    let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
    grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
    grid.setRowSpacing(8.0);
    grid.setColumnSpacing(8.0);
    grid.setFrameSize(grid.fittingSize());
    alert.setAccessoryView(Some(&grid));
    alert.layout();
    alert.window().setInitialFirstResponder(Some(&field));
    NSApplication::sharedApplication(mtm).activate();
    if alert.runModal() != NSAlertFirstButtonReturn {
        return None;
    }
    let text = field.stringValue().to_string().trim().to_string();
    if text.is_empty() {
        return None;
    }
    let query = Query {
        text,
        mode: if mode.indexOfSelectedItem() == 1 { Mode::Contents } else { Mode::Name },
        include_hidden: hidden.state() == NSControlStateValueOn,
    };
    let use_spotlight = local && spotlight.state() == NSControlStateValueOn;
    LAST.with(|l| {
        *l.borrow_mut() =
            Last { query: query.clone(), spotlight: if local { use_spotlight } else { l.borrow().spotlight } }
    });
    Some((query, use_spotlight))
}

/// Asks, searches the pane's folder, and shows the results in the pane.
pub fn run(pane: &PaneController) {
    let fs = pane.fs();
    // Searching again from results searches the folder they came from.
    let fs = fs.browse_fs().unwrap_or(fs);
    let root = pane.location();
    let folder = if root.parent().is_none() && !fs.is_local() { fs.label() } else { title_of(&root) };
    let Some((query, use_spotlight)) = ask(&folder, fs.is_local()) else { return };
    let Some(activity) = super::activity_view() else { return };
    let note =
        activity.begin_note(&format!("Searching \u{201c}{folder}\u{201d} for \u{201c}{}\u{201d}\u{2026}", query.text));
    let index = pane.index();
    std::thread::spawn(move || {
        let cancel = Arc::new(AtomicBool::new(false));
        let result = find(&fs, &root, &query, use_spotlight, &cancel);
        DispatchQueue::main().exec_async(move || {
            if let Some(activity) = super::activity_view() {
                activity.remove(note);
            }
            show(index, fs, root, folder, query, result);
        });
    });
}

fn find(
    fs: &FsRef,
    root: &std::path::Path,
    query: &Query,
    use_spotlight: bool,
    cancel: &Arc<AtomicBool>,
) -> std::io::Result<Vec<crate::fs::Entry>> {
    if use_spotlight {
        match search::spotlight(root, query) {
            // Nothing can also mean the folder is not indexed: walk it.
            Ok(paths) if paths.is_empty() => tracing::info!("Spotlight found nothing; searching by walking"),
            Ok(paths) => {
                let local = vfs::local();
                return Ok(paths.iter().filter_map(|p| local.stat(p).ok()).collect());
            }
            Err(e) => tracing::warn!("Spotlight failed, searching by walking: {e}"),
        }
    }
    search::walk(fs, root, query, cancel)
}

fn show(
    index: usize,
    fs: FsRef,
    root: PathBuf,
    folder: String,
    query: Query,
    result: std::io::Result<Vec<crate::fs::Entry>>,
) {
    let entries = match result {
        Ok(entries) => entries,
        Err(e) => {
            super::dialogs::show_error(format!("\u{201c}{folder}\u{201d} could not be searched."), e.to_string());
            return;
        }
    };
    let what = if query.mode == Mode::Contents { "containing" } else { "named" };
    let count = entries.len();
    oplog::record(
        oplog::Kind::Change,
        format!("Searched \u{201c}{folder}\u{201d} for items {what} \u{201c}{}\u{201d}: {count} found", query.text),
        "",
    );
    if count == 0 {
        super::dialogs::show_info(
            format!("Nothing {what} \u{201c}{}\u{201d} was found.", query.text),
            format!("Searched \u{201c}{folder}\u{201d} and everything in it."),
        );
        return;
    }
    let more = if count >= search::LIMIT { "+" } else { "" };
    let title = format!("\u{201c}{}\u{201d} in {folder} ({count}{more})", query.text);
    let results = vfs::results::ResultsFs::new(fs, root.clone(), title, entries);
    if let Some(pane) = super::pane(index) {
        pane.navigate_to(results, root);
        pane.focus();
    }
}
