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

/// What the search dialog asked for.
struct Asked {
    query: Query,
    spotlight: bool,
    /// Save it as a smart folder.
    save: bool,
}

/// Asks what to search for. `local`: Spotlight can be used (and the search
/// can be saved as a smart folder). `editing`: a smart folder's search and
/// name, changed instead of searching.
fn ask(folder: &str, local: bool, editing: Option<(&Query, bool, &str)>) -> Option<Asked> {
    let mtm = mtm();
    let (last_query, last_spotlight) = match editing {
        Some((query, spotlight, _)) => (query.clone(), spotlight),
        None => LAST.with(|l| (l.borrow().query.clone(), l.borrow().spotlight)),
    };
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(&match editing {
        Some((_, _, name)) => format!("Edit \u{201c}{name}\u{201d}"),
        None => format!("Search in \u{201c}{folder}\u{201d}"),
    }));
    alert.setInformativeText(&ns(&format!(
        "{} For names, * matches any text and ? one character; separate several with ;.",
        if editing.is_some() {
            format!("Searches \u{201c}{folder}\u{201d} and everything in it whenever the smart folder is opened.")
        } else {
            "Searches the folder and everything in it.".to_string()
        }
    )));
    alert.addButtonWithTitle(&ns(if editing.is_some() { "Save" } else { "Search" }));
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
    let save = checkbox("Save as a smart folder in the sidebar", false);
    save.setHidden(!local || editing.is_some());
    let label = |text: &str| Retained::into_super(Retained::into_super(NSTextField::labelWithString(&ns(text), mtm)));
    let rows: Vec<Retained<NSArray<NSView>>> = [
        (label("Find:"), view(&*field)),
        (label("Look in:"), view(&*mode)),
        (label("Kind:"), view(&*kind)),
        (label("Tag:"), view(&*tag)),
        (label(""), view(&*hidden)),
        (label(""), view(&*spotlight)),
        (label(""), view(&*save)),
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
    if editing.is_none() {
        LAST.with(|l| {
            *l.borrow_mut() =
                Last { query: query.clone(), spotlight: if local { use_spotlight } else { l.borrow().spotlight } }
        });
    }
    let save = local && editing.is_none() && save.state() == NSControlStateValueOn;
    Some(Asked { query, spotlight: use_spotlight, save })
}

/// Asks, searches the pane's folder, and shows the results in the pane.
pub fn run(pane: &PaneController) {
    let fs = pane.fs();
    // Searching again from results searches the folder they came from.
    let fs = fs.browse_fs().unwrap_or(fs);
    let root = pane.location();
    let folder = if root.parent().is_none() && !fs.is_local() { fs.label() } else { title_of(&root) };
    let Some(Asked { query, spotlight: use_spotlight, save }) = ask(&folder, fs.is_local(), None) else { return };
    let name = if save { save_smart_folder(&root, &folder, &query, use_spotlight) } else { None };
    start(pane.index(), fs, root, folder, query, use_spotlight, name);
}

/// Saves a search as a smart folder (named first); returns the name.
fn save_smart_folder(root: &std::path::Path, folder: &str, query: &Query, spotlight: bool) -> Option<String> {
    let suggested = format!("{} in {folder}", describe(query)).replace(['\u{201c}', '\u{201d}'], "");
    let name = super::dialogs::prompt("Save Smart Folder", "Name in the sidebar:", "Save", &suggested, None)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let smart = crate::settings::SmartFolder {
        name: name.clone(),
        folder: root.to_path_buf(),
        query: query.clone(),
        spotlight,
    };
    super::update_settings(|s| s.smart_folders.push(smart));
    super::reload_sidebar();
    Some(name)
}

/// Opens a smart folder: searches again and shows what it finds in the
/// active pane.
pub fn open_smart_folder(index: usize) {
    let Some(smart) = super::current_settings().and_then(|s| s.smart_folders.get(index).cloned()) else { return };
    let Some(pane) = super::active_pane() else { return };
    if !smart.folder.is_dir() {
        super::dialogs::show_error(
            format!("The folder of \u{201c}{}\u{201d} is gone.", smart.name),
            format!("{} could not be found.", smart.folder.display()),
        );
        return;
    }
    let folder = title_of(&smart.folder);
    start(pane.index(), vfs::local(), smart.folder, folder, smart.query, smart.spotlight, Some(smart.name));
}

/// Changes a smart folder's search.
pub fn edit_smart_folder(index: usize) {
    let Some(smart) = super::current_settings().and_then(|s| s.smart_folders.get(index).cloned()) else { return };
    let folder = title_of(&smart.folder);
    let Some(asked) = ask(&folder, true, Some((&smart.query, smart.spotlight, &smart.name))) else { return };
    super::update_settings(|s| {
        if let Some(saved) = s.smart_folders.get_mut(index) {
            saved.query = asked.query;
            saved.spotlight = asked.spotlight;
        }
    });
}

/// The start of a flat view's title.
const FLAT_PREFIX: &str = "All files in ";

/// View > Flat View: every file in the pane's folder and the folders in it,
/// in one list (with a column for the folder each is in). Again: back to
/// the folder.
pub fn flat_view(pane: &PaneController) {
    let current = pane.fs().title_for(&pane.location());
    if current.is_some_and(|t| t.starts_with(FLAT_PREFIX)) {
        pane.go_back();
        return;
    }
    let fs = pane.fs();
    let fs = fs.browse_fs().unwrap_or(fs);
    let root = pane.location();
    let folder = if root.parent().is_none() && !fs.is_local() { fs.label() } else { title_of(&root) };
    let query = Query { kind: Kind::Files, include_hidden: pane.show_hidden(), ..Query::default() };
    let name = format!("{FLAT_PREFIX}{folder}");
    start(pane.index(), fs, root, folder, query, false, Some(name));
}

/// Searches in the background and shows the results in a pane (titled
/// with `name` for a smart folder).
fn start(
    index: usize,
    fs: FsRef,
    root: PathBuf,
    folder: String,
    query: Query,
    use_spotlight: bool,
    name: Option<String>,
) {
    let Some(activity) = super::activity_view() else { return };
    let (note, cancel) =
        activity.begin_task(&format!("Searching \u{201c}{folder}\u{201d} for {}\u{2026}", describe(&query)));
    std::thread::spawn(move || {
        let result = find(&fs, &root, &query, use_spotlight, &cancel);
        let stopped = cancel.is_cancelled();
        DispatchQueue::main().exec_async(move || {
            if let Some(activity) = super::activity_view() {
                activity.remove(note);
            }
            show(index, fs, root, folder, query, result, stopped, name);
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

#[allow(clippy::too_many_arguments)]
fn show(
    index: usize,
    fs: FsRef,
    root: PathBuf,
    folder: String,
    query: Query,
    result: std::io::Result<Vec<crate::fs::Entry>>,
    stopped: bool,
    name: Option<String>,
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
        let message = match &name {
            Some(name) if name.starts_with(FLAT_PREFIX) => "There are no files in this folder or below it.".to_string(),
            _ => format!("Nothing was found for {}.", describe(&query)),
        };
        super::dialogs::show_info(message, format!("Searched \u{201c}{folder}\u{201d} and everything in it."));
        return;
    }
    let more = if count >= search::LIMIT || stopped { "+" } else { "" };
    let what = name.unwrap_or_else(|| format!("{} in {folder}", describe(&query)));
    let title = format!("{what} ({count}{more}){}", if stopped { ", stopped" } else { "" });
    let results = vfs::results::ResultsFs::new(fs, root.clone(), title, entries);
    if let Some(pane) = super::pane(index) {
        pane.navigate_to(results, root);
        pane.focus();
    }
}

/// Renames a smart folder.
pub fn rename_smart_folder(index: usize) {
    let Some(smart) = super::current_settings().and_then(|s| s.smart_folders.get(index).cloned()) else { return };
    let Some(name) = super::dialogs::prompt("Rename Smart Folder", "New name:", "Rename", &smart.name, None) else {
        return;
    };
    let name = name.trim().to_string();
    if name.is_empty() {
        return;
    }
    super::update_settings(|s| {
        if let Some(saved) = s.smart_folders.get_mut(index) {
            saved.name = name;
        }
    });
    super::reload_sidebar();
}

/// Removes a smart folder from the sidebar (the items it finds stay).
pub fn delete_smart_folder(index: usize) {
    let Some(smart) = super::current_settings().and_then(|s| s.smart_folders.get(index).cloned()) else { return };
    if super::dialogs::confirm(
        &format!("Delete the smart folder \u{201c}{}\u{201d}?", smart.name),
        "Only the saved search is deleted; the items it finds are not changed.",
        "Delete",
        true,
    ) {
        super::update_settings(|s| {
            if index < s.smart_folders.len() {
                s.smart_folders.remove(index);
            }
        });
        super::reload_sidebar();
    }
}
