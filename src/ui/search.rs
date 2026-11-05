//! Search (Shift-Command-F): asks what to look for in the active pane's
//! folder, searches in the background, and shows what it found in the
//! pane as a folder of results.

use std::cell::RefCell;
use std::path::PathBuf;

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
use crate::fs::ops::CancelToken;
use crate::fs::search::{self, Kind, Mode, Query};
use crate::pane::title_of;
use crate::vfs::{self, FsRef};

struct Last {
    query: Query,
    spotlight: bool,
}

thread_local! {
    static LAST: RefCell<Last> = const { RefCell::new(Last {
        query: Query { text: String::new(), mode: Mode::Name, include_hidden: false, kind: Kind::Any, tag: None },
        spotlight: true,
    }) };
}

fn view<T: objc2::Message + AsRef<NSView>>(v: &T) -> Retained<NSView> {
    v.as_ref().retain()
}

/// What a search looks for, in words: "“report”", "images tagged Red".
fn describe(query: &Query) -> String {
    let mut parts = Vec::new();
    if !query.text.trim().is_empty() {
        parts.push(format!("\u{201c}{}\u{201d}", query.text.trim()));
    }
    if query.kind != Kind::Any {
        let kind = Kind::ALL.iter().find(|k| k.0 == query.kind).map_or("", |k| k.1).to_lowercase();
        parts.push(kind);
    }
    if let Some(tag) = &query.tag {
        parts.push(format!("tagged {tag}"));
    }
    parts.join(", ")
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
    field.setPlaceholderString(Some(&ns("report, *.pdf, or text (empty: just the kind or tag)")));
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
    let popup = |titles: &[&str], selected: usize| {
        let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        let titles: Vec<Retained<NSString>> = titles.iter().map(|t| ns(t)).collect();
        popup.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
        popup.selectItemAtIndex(selected as isize);
        popup
    };
    let kinds: Vec<&str> = Kind::ALL.iter().map(|k| k.1).collect();
    let kind = popup(&kinds, Kind::ALL.iter().position(|k| k.0 == last_query.kind).unwrap_or(0));
    let mut tags = vec!["Any tag"];
    tags.extend(crate::fs::tags::COLORS.iter().map(|c| c.0));
    let tag_index = last_query.tag.as_deref().and_then(|t| tags.iter().position(|x| *x == t)).unwrap_or(0);
    let tag = popup(&tags, tag_index);
    let hidden = checkbox("Include hidden items", last_query.include_hidden);
    let spotlight = checkbox("Use Spotlight (fast, for indexed folders)", local && last_spotlight);
    spotlight.setEnabled(local);
    let label = |text: &str| Retained::into_super(Retained::into_super(NSTextField::labelWithString(&ns(text), mtm)));
    let rows: Vec<Retained<NSArray<NSView>>> = [
        (label("Find:"), view(&*field)),
        (label("Look in:"), view(&*mode)),
        (label("Kind:"), view(&*kind)),
        (label("Tag:"), view(&*tag)),
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
    let tag_index = tag.indexOfSelectedItem().max(0) as usize;
    let query = Query {
        text,
        mode: if mode.indexOfSelectedItem() == 1 { Mode::Contents } else { Mode::Name },
        include_hidden: hidden.state() == NSControlStateValueOn,
        kind: Kind::ALL.get(kind.indexOfSelectedItem().max(0) as usize).map_or(Kind::Any, |k| k.0),
        tag: (tag_index > 0).then(|| tags[tag_index].to_string()),
    };
    // Some text, or a kind or tag.
    if query.is_empty() {
        return None;
    }
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
    let (note, cancel) =
        activity.begin_task(&format!("Searching \u{201c}{folder}\u{201d} for {}\u{2026}", describe(&query)));
    let index = pane.index();
    std::thread::spawn(move || {
        let result = find(&fs, &root, &query, use_spotlight, &cancel);
        let stopped = cancel.is_cancelled();
        DispatchQueue::main().exec_async(move || {
            if let Some(activity) = super::activity_view() {
                activity.remove(note);
            }
            show(index, fs, root, folder, query, result, stopped);
        });
    });
}

fn find(
    fs: &FsRef,
    root: &std::path::Path,
    query: &Query,
    use_spotlight: bool,
    cancel: &CancelToken,
) -> std::io::Result<Vec<crate::fs::Entry>> {
    if use_spotlight {
        match search::spotlight(root, query, cancel) {
            // Nothing can also mean the folder is not indexed: walk it.
            Ok(paths) if paths.is_empty() => tracing::info!("Spotlight found nothing; searching by walking"),
            Ok(paths) => {
                let local = vfs::local();
                return Ok(paths.iter().filter_map(|p| local.stat(p).ok()).collect());
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Ok(Vec::new()),
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
    stopped: bool,
) {
    let entries = match result {
        Ok(entries) => entries,
        Err(e) => {
            super::dialogs::show_error(format!("\u{201c}{folder}\u{201d} could not be searched."), e.to_string());
            return;
        }
    };
    let count = entries.len();
    oplog::record(
        oplog::Kind::Change,
        format!(
            "Searched \u{201c}{folder}\u{201d} for {}: {count} found{}",
            describe(&query),
            if stopped { " (stopped)" } else { "" }
        ),
        "",
    );
    // Stopped with nothing found: nothing to say.
    if count == 0 && stopped {
        return;
    }
    if count == 0 {
        super::dialogs::show_info(
            format!("Nothing was found for {}.", describe(&query)),
            format!("Searched \u{201c}{folder}\u{201d} and everything in it."),
        );
        return;
    }
    let more = if count >= search::LIMIT || stopped { "+" } else { "" };
    let title = format!("{} in {folder} ({count}{more}){}", describe(&query), if stopped { ", stopped" } else { "" });
    let results = vfs::results::ResultsFs::new(fs, root.clone(), title, entries);
    if let Some(pane) = super::pane(index) {
        pane.navigate_to(results, root);
        pane.focus();
    }
}
