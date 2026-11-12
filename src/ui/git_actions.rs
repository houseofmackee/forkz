//! The Git menu (and the context menu's Git submenu): stage, unstage,
//! commit, pull, push, and switch branches in the repository the active
//! pane's folder is in. git runs in the background (it never waits for
//! input); what it says when something goes wrong is shown.

use std::io;
use std::path::{Path, PathBuf};

use dispatch2::DispatchQueue;
use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSButton, NSControlStateValueOn, NSFont, NSPopUpButton,
    NSScrollView, NSStackView, NSTextField, NSTextView, NSUserInterfaceLayoutOrientation,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use super::commands::Command;
use super::pane_view::PaneController;
use super::util::{mtm, ns};
use super::{app, dialogs, oplog};
use crate::fs::git;

/// The repository a pane's folder is in (on this Mac).
pub fn repository_of(pane: &PaneController) -> Option<PathBuf> {
    if !pane.is_local() || pane.showing_results() {
        return None;
    }
    git::repository_root(&pane.location())
}

/// Whether a Git command can run in a pane now.
pub fn is_enabled(command: Command, pane: &PaneController, has_selection: bool) -> bool {
    if repository_of(pane).is_none() {
        return false;
    }
    match command {
        Command::GitStage | Command::GitUnstage => has_selection,
        _ => true,
    }
}

pub fn run(command: Command, pane: &PaneController) {
    let Some(root) = repository_of(pane) else { return };
    match command {
        Command::GitStage | Command::GitUnstage => {
            let paths = pane.target_paths();
            if paths.is_empty() {
                return;
            }
            let stage = command == Command::GitStage;
            let what = items(&paths);
            let (note, done) = if stage {
                (format!("Staging {what}\u{2026}"), format!("Staged {what}"))
            } else {
                (format!("Unstaging {what}\u{2026}"), format!("Unstaged {what}"))
            };
            in_background(root, note, done, move |root| {
                if stage { git::stage(root, &paths) } else { git::unstage(root, &paths) }.map(|()| String::new())
            });
        }
        Command::GitCommit => commit(root),
        Command::GitPull => in_background(root, "Pulling\u{2026}".into(), "Pulled".into(), git::pull),
        Command::GitPush => in_background(root, "Pushing\u{2026}".into(), "Pushed".into(), git::push),
        Command::GitSwitchBranch => switch_branch(root),
        _ => {}
    }
}

/// "\u{201c}name\u{201d}" or "3 items".
fn items(paths: &[PathBuf]) -> String {
    match paths {
        [one] => format!("\u{201c}{}\u{201d}", crate::pane::title_of(one)),
        many => format!("{} items", many.len()),
    }
}

/// The repository's name, for messages ("forkz").
fn name_of(root: &Path) -> String {
    crate::pane::title_of(root)
}

/// Runs a git action off the main thread, with a note while it runs; then
/// the panes in the repository show its new state.
fn in_background(
    root: PathBuf,
    note: String,
    done: String,
    work: impl FnOnce(&Path) -> io::Result<String> + Send + 'static,
) {
    let Some(id) = app().map(|a| a.activity.begin_note(&note)) else { return };
    std::thread::spawn(move || {
        let result = work(&root);
        DispatchQueue::main().exec_async(move || {
            if let Some(app) = app() {
                app.activity.remove(id);
            }
            let place = name_of(&root);
            match result {
                Ok(_) => oplog::record(oplog::Kind::Change, format!("{done} in {place}"), ""),
                Err(e) => {
                    let message = format!("{} failed in \u{201c}{place}\u{201d}", note.trim_end_matches('\u{2026}'));
                    oplog::record(oplog::Kind::Problem, message.clone(), e.to_string());
                    dialogs::show_error(format!("{message}."), e.to_string());
                }
            }
            repository_changed(&root);
        });
    });
}

/// The panes in a repository show its new state (Git column and branch).
fn repository_changed(root: &Path) {
    let Some(app) = app() else { return };
    for pane in &app.panes {
        if pane.is_local() && pane.location().starts_with(root) {
            pane.git_changed();
        }
    }
}

/// Asks for a message (and whether to stage everything first), then
/// commits.
fn commit(root: PathBuf) {
    let (staged, unstaged) = match git::change_counts(&root) {
        Ok(counts) => counts,
        Err(e) => {
            dialogs::show_error("The repository could not be read.".into(), e.to_string());
            return;
        }
    };
    if staged == 0 && unstaged == 0 {
        dialogs::show_info(format!("Nothing to commit in \u{201c}{}\u{201d}.", name_of(&root)), String::new());
        return;
    }
    let mtm = mtm();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(&format!("Commit to \u{201c}{}\u{201d}", name_of(&root))));
    let count = |n: usize| if n == 1 { "1 change".to_string() } else { format!("{n} changes") };
    alert.setInformativeText(&ns(&format!("{} staged, {} not staged.", count(staged), count(unstaged))));
    alert.addButtonWithTitle(&ns("Commit"));
    alert.addButtonWithTitle(&ns("Cancel"));

    // A message of several lines: the first is the summary.
    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(360.0, 110.0)),
    );
    scroll.setHasVerticalScroller(true);
    scroll.setBorderType(objc2_app_kit::NSBorderType::BezelBorder);
    let text = NSTextView::initWithFrame(
        NSTextView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(360.0, 110.0)),
    );
    text.setRichText(false);
    text.setFont(Some(&NSFont::systemFontOfSize(13.0)));
    text.setAutomaticQuoteSubstitutionEnabled(false);
    text.setAutomaticDashSubstitutionEnabled(false);
    text.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
    scroll.setDocumentView(Some(&text));
    let all = unsafe { NSButton::checkboxWithTitle_target_action(&ns("Stage all changes first"), None, None, mtm) };
    // Nothing staged: committing means everything.
    all.setState(if staged == 0 { NSControlStateValueOn } else { objc2_app_kit::NSControlStateValueOff });
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
    stack.setSpacing(8.0);
    stack.addArrangedSubview(&scroll);
    stack.addArrangedSubview(&all);
    scroll.widthAnchor().constraintEqualToConstant(360.0).setActive(true);
    scroll.heightAnchor().constraintEqualToConstant(110.0).setActive(true);
    stack.setFrameSize(stack.fittingSize());
    alert.setAccessoryView(Some(&stack));
    alert.layout();
    alert.window().setInitialFirstResponder(Some(&text));
    NSApplication::sharedApplication(mtm).activate();
    loop {
        alert.window().makeFirstResponder(Some(&text));
        if alert.runModal() != NSAlertFirstButtonReturn {
            return;
        }
        let message = text.string().to_string();
        if message.trim().is_empty() {
            alert.setInformativeText(&ns("Write a message for the commit."));
            continue;
        }
        let stage_all = all.state() == NSControlStateValueOn;
        if !stage_all && staged == 0 {
            alert.setInformativeText(&ns("Nothing is staged: stage changes, or choose to stage them all."));
            continue;
        }
        let summary = message.lines().next().unwrap_or_default().trim().to_string();
        in_background(root, "Committing\u{2026}".into(), format!("Committed \u{201c}{summary}\u{201d}"), move |root| {
            git::commit(root, &message, stage_all)
        });
        return;
    }
}

/// Picks a branch to switch to, or names a new one.
fn switch_branch(root: PathBuf) {
    let (branches, current) = match (git::branches(&root), git::branch(&root)) {
        (Ok(branches), Ok(current)) => (branches, current),
        (Err(e), _) | (_, Err(e)) => {
            dialogs::show_error("The repository could not be read.".into(), e.to_string());
            return;
        }
    };
    let mtm = mtm();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(&format!("Switch Branch in \u{201c}{}\u{201d}", name_of(&root))));
    alert.setInformativeText(&ns(&format!(
        "Now on {}. Choose a branch, or name a new one (made from the current branch).",
        current.summary()
    )));
    alert.addButtonWithTitle(&ns("Switch"));
    alert.addButtonWithTitle(&ns("Cancel"));
    let popup = NSPopUpButton::initWithFrame_pullsDown(
        NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 26.0)),
        false,
    );
    for branch in &branches {
        popup.addItemWithTitle(&ns(branch));
    }
    if let Some(index) = current.name.as_ref().and_then(|name| branches.iter().position(|b| b == name)) {
        popup.selectItemAtIndex(index as isize);
    }
    let new_name =
        NSTextField::initWithFrame(NSTextField::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(300.0, 24.0)));
    new_name.setPlaceholderString(Some(&ns("New branch (optional)")));
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
    stack.setSpacing(8.0);
    stack.addArrangedSubview(&popup);
    stack.addArrangedSubview(&new_name);
    for view in [&*popup as &objc2_app_kit::NSView, &*new_name] {
        view.widthAnchor().constraintEqualToConstant(300.0).setActive(true);
    }
    stack.setFrameSize(stack.fittingSize());
    alert.setAccessoryView(Some(&stack));
    alert.layout();
    NSApplication::sharedApplication(mtm).activate();
    if alert.runModal() != NSAlertFirstButtonReturn {
        return;
    }
    let typed = new_name.stringValue().to_string().trim().to_string();
    let (name, create) = if typed.is_empty() {
        let index = popup.indexOfSelectedItem();
        match usize::try_from(index).ok().and_then(|i| branches.get(i)) {
            Some(name) => (name.clone(), false),
            None => return,
        }
    } else {
        (typed, true)
    };
    if !create && current.name.as_deref() == Some(name.as_str()) {
        return;
    }
    let (note, done) = if create {
        (
            format!("Making branch \u{201c}{name}\u{201d}\u{2026}"),
            format!("Made and switched to branch \u{201c}{name}\u{201d}"),
        )
    } else {
        (format!("Switching to \u{201c}{name}\u{201d}\u{2026}"), format!("Switched to branch \u{201c}{name}\u{201d}"))
    };
    in_background(root, note, done, move |root| git::switch(root, &name, create));
}
