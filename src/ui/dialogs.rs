//! Alerts: name conflicts and errors during jobs, and the text prompts
//! (rename, go to folder).
//!
//! Jobs ask from worker threads; their questions are queued on the main
//! thread so only one alert is open at a time.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::Path;
use std::sync::mpsc::Sender;

use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSAlert, NSAlertStyle, NSApplication, NSControlStateValueOn, NSTextField};
use objc2_foundation::{NSPoint, NSRange, NSRect, NSSize};

use super::util::{format_date, mtm, ns};
use crate::fs::ops::{Answer, Conflict, ErrorAnswer, Resolution};
use crate::pane::title_of;

thread_local! {
    static QUEUE: RefCell<VecDeque<Box<dyn FnOnce()>>> = RefCell::new(VecDeque::new());
    static PRESENTING: Cell<bool> = const { Cell::new(false) };
}

/// Runs `show` (which opens a modal alert) after any alert already open.
fn present(show: impl FnOnce() + 'static) {
    QUEUE.with(|q| q.borrow_mut().push_back(Box::new(show)));
    if PRESENTING.with(Cell::get) {
        return;
    }
    PRESENTING.with(|p| p.set(true));
    while let Some(next) = QUEUE.with(|q| q.borrow_mut().pop_front()) {
        next();
    }
    PRESENTING.with(|p| p.set(false));
}

fn alert(mtm: MainThreadMarker, message: &str, informative: &str) -> Retained<NSAlert> {
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(message));
    alert.setInformativeText(&ns(informative));
    alert
}

/// Index of the button clicked (0 for the first).
fn run(alert: &NSAlert) -> usize {
    NSApplication::sharedApplication(alert.mtm()).activate();
    let response = alert.runModal();
    tracing::debug!("alert \u{201c}{}\u{201d} answered {response}", alert.messageText());
    usize::try_from(response - objc2_app_kit::NSAlertFirstButtonReturn).unwrap_or(usize::MAX)
}

/// Asks what to do about a conflict; the answer goes to `reply`.
pub fn ask_conflict(conflict: Conflict, reply: Sender<Answer>) {
    present(move || {
        let mtm = mtm();
        let name = title_of(&conflict.existing);
        let folder = conflict.existing.parent().map(title_of).unwrap_or_default();
        let what = if conflict.existing_is_folder { "A folder" } else { "An item" };
        let message = format!("{what} named \u{201c}{name}\u{201d} already exists in \u{201c}{folder}\u{201d}.");
        let detail = format!(
            "Existing item modified {}.\nNew item modified {}.",
            format_date(conflict.existing_modified),
            format_date(conflict.source_modified)
        );
        let alert = alert(mtm, &message, &detail);
        let mut choices = vec![Resolution::Replace, Resolution::KeepBoth, Resolution::Skip];
        if conflict.can_merge() {
            choices.insert(0, Resolution::Merge);
        }
        choices.push(Resolution::ReplaceOlder);
        choices.push(Resolution::Stop);
        for choice in &choices {
            alert.addButtonWithTitle(&ns(match choice {
                Resolution::Replace => "Replace",
                Resolution::ReplaceOlder => "Replace if Older",
                Resolution::KeepBoth => "Keep Both",
                Resolution::Skip => "Skip",
                Resolution::Merge => "Merge",
                Resolution::Stop => "Stop",
            }));
        }
        alert.setShowsSuppressionButton(true);
        if let Some(button) = alert.suppressionButton() {
            button.setTitle(&ns("Apply to all"));
        }
        let index = run(&alert);
        let resolution = choices.get(index).copied().unwrap_or(Resolution::Stop);
        let apply_to_all = alert.suppressionButton().is_some_and(|b| b.state() == NSControlStateValueOn);
        let _ = reply.send(Answer { resolution, apply_to_all });
    });
}

pub fn ask_error(path: &Path, error: String, reply: Sender<ErrorAnswer>) {
    let name = title_of(path);
    present(move || {
        let alert = alert(mtm(), &format!("\u{201c}{name}\u{201d} could not be processed."), &error);
        alert.setAlertStyle(NSAlertStyle::Warning);
        for title in ["Skip", "Try Again", "Stop"] {
            alert.addButtonWithTitle(&ns(title));
        }
        let answer = match run(&alert) {
            0 => ErrorAnswer::Skip,
            1 => ErrorAnswer::Retry,
            _ => ErrorAnswer::Stop,
        };
        let _ = reply.send(answer);
    });
}

/// Tells about something that went wrong; nothing to decide.
pub fn show_error(message: String, detail: String) {
    present(move || {
        let alert = alert(mtm(), &message, &detail);
        alert.setAlertStyle(NSAlertStyle::Warning);
        run(&alert);
    });
}

/// Tells about something that worked (no warning style).
pub fn show_info(message: String, detail: String) {
    present(move || {
        let alert = alert(mtm(), &message, &detail);
        run(&alert);
    });
}

/// Asks for a line of text. `select` is how many characters to select at
/// the start (the name without its extension when renaming).
pub fn prompt(message: &str, informative: &str, ok: &str, initial: &str, select: Option<usize>) -> Option<String> {
    prompt_text(message, informative, ok, initial, select).filter(|t| !t.trim().is_empty())
}

/// A text prompt with a choice under the field (a popup). Returns the text
/// and the choice's index.
pub fn prompt_with_choice(
    message: &str,
    informative: &str,
    ok: &str,
    initial: &str,
    choices: &[&str],
    selected: usize,
) -> Option<(String, usize)> {
    let mtm = mtm();
    let alert = alert(mtm, message, informative);
    alert.addButtonWithTitle(&ns(ok));
    alert.addButtonWithTitle(&ns("Cancel"));
    let field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 32.0), NSSize::new(320.0, 24.0)),
    );
    field.setStringValue(&ns(initial));
    let popup = objc2_app_kit::NSPopUpButton::initWithFrame_pullsDown(
        objc2_app_kit::NSPopUpButton::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 26.0)),
        false,
    );
    for choice in choices {
        popup.addItemWithTitle(&ns(choice));
    }
    popup.selectItemAtIndex(selected as isize);
    let container = objc2_app_kit::NSView::initWithFrame(
        objc2_app_kit::NSView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 58.0)),
    );
    container.addSubview(&field);
    container.addSubview(&popup);
    alert.setAccessoryView(Some(&container));
    alert.layout();
    let window = alert.window();
    window.setInitialFirstResponder(Some(&field));
    window.makeFirstResponder(Some(&field));
    if run(&alert) != 0 {
        return None;
    }
    let text = field.stringValue().to_string();
    if text.trim().is_empty() { None } else { Some((text, popup.indexOfSelectedItem().max(0) as usize)) }
}

/// Like `prompt`, but an empty answer is an answer (None only if cancelled).
pub fn prompt_allow_empty(message: &str, informative: &str, ok: &str, initial: &str) -> Option<String> {
    prompt_text(message, informative, ok, initial, None)
}

fn prompt_text(message: &str, informative: &str, ok: &str, initial: &str, select: Option<usize>) -> Option<String> {
    let mtm = mtm();
    let alert = alert(mtm, message, informative);
    alert.addButtonWithTitle(&ns(ok));
    alert.addButtonWithTitle(&ns("Cancel"));
    let field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 24.0)),
    );
    field.setStringValue(&ns(initial));
    alert.setAccessoryView(Some(&field));
    alert.layout();
    let window = alert.window();
    window.setInitialFirstResponder(Some(&field));
    window.makeFirstResponder(Some(&field));
    if let (Some(length), Some(editor)) = (select, field.currentEditor()) {
        let utf16 = initial.chars().take(length).map(char::len_utf16).sum();
        editor.setSelectedRange(NSRange::new(0, utf16));
    }
    if run(&alert) != 0 {
        return None;
    }
    Some(field.stringValue().to_string())
}

/// Asks whether to trust a server's host key (never seen, or changed).
pub fn ask_host_key(
    host: String,
    port: u16,
    key_type: String,
    fingerprint: String,
    status: crate::vfs::known_hosts::Status,
    reply: Sender<bool>,
) {
    present(move || {
        use crate::vfs::known_hosts::Status;
        let address = if port == 22 { host.clone() } else { format!("{host}:{port}") };
        // "ssh-ed25519 key fingerprint", or "TLS certificate fingerprint".
        let what = if key_type.contains("certificate") { key_type.clone() } else { format!("{key_type} key") };
        let sent = if key_type.contains("certificate") { "certificate" } else { "host key" };
        let (message, detail) = if status == Status::Changed {
            (
                format!("The identity of \u{201c}{address}\u{201d} has changed!"),
                format!(
                    "The server sent a different {sent} than the one trusted before. This can happen after a server is reinstalled, but it can also mean someone is intercepting the connection.\n\nNew {what} fingerprint:\n{fingerprint}\n\nOnly continue if you know why it changed."
                ),
            )
        } else {
            (
                format!("Do you trust \u{201c}{address}\u{201d}?"),
                format!(
                    "forkz has not connected to this server before. Check that the fingerprint matches the one your server shows.\n\n{what} fingerprint:\n{fingerprint}"
                ),
            )
        };
        let alert = alert(mtm(), &message, &detail);
        if status == Status::Changed {
            alert.setAlertStyle(NSAlertStyle::Critical);
            alert.addButtonWithTitle(&ns("Cancel"));
            alert.addButtonWithTitle(&ns("Trust the New Key"));
            let _ = reply.send(run(&alert) == 1);
        } else {
            alert.addButtonWithTitle(&ns("Trust and Connect"));
            alert.addButtonWithTitle(&ns("Cancel"));
            let _ = reply.send(run(&alert) == 0);
        }
    });
}

/// Asks for a password or passphrase, with an option to keep it in the
/// Keychain. Replies None if cancelled.
pub fn ask_secret(message: String, informative: String, offer_save: bool, reply: Sender<Option<(String, bool)>>) {
    present(move || {
        let mtm = mtm();
        let alert = alert(mtm, &message, &informative);
        alert.addButtonWithTitle(&ns("Connect"));
        alert.addButtonWithTitle(&ns("Cancel"));
        let field = objc2_app_kit::NSSecureTextField::initWithFrame(
            objc2_app_kit::NSSecureTextField::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(280.0, 24.0)),
        );
        alert.setAccessoryView(Some(&field));
        if offer_save {
            alert.setShowsSuppressionButton(true);
            if let Some(button) = alert.suppressionButton() {
                button.setTitle(&ns("Save in the Keychain"));
                button.setState(NSControlStateValueOn);
            }
        }
        alert.layout();
        alert.window().setInitialFirstResponder(Some(&field));
        if run(&alert) != 0 {
            let _ = reply.send(None);
            return;
        }
        let save = offer_save && alert.suppressionButton().is_some_and(|b| b.state() == NSControlStateValueOn);
        let _ = reply.send(Some((field.stringValue().to_string(), save)));
    });
}

/// Asks a yes/no question; true for the first button.
pub fn confirm(message: &str, informative: &str, ok: &str, destructive: bool) -> bool {
    let alert = alert(mtm(), message, informative);
    // Return confirms, like Finder's Delete Immediately (a button marked
    // destructive would lose the Return key).
    alert.addButtonWithTitle(&ns(ok));
    alert.addButtonWithTitle(&ns("Cancel"));
    if destructive {
        alert.setAlertStyle(NSAlertStyle::Warning);
    }
    run(&alert) == 0
}

/// An item to choose in `choose_items`.
pub struct Choice {
    pub path: std::path::PathBuf,
    /// Shown for it ("~/Library/Caches/com.example.App").
    pub label: String,
    /// Checked at first.
    pub checked: bool,
}

/// Asks which items to act on: a check box for each, in groups under a
/// heading (an empty heading: none). The checked ones, or None for the
/// second button (or none checked).
pub fn choose_items(
    message: &str,
    informative: &str,
    ok: &str,
    cancel: &str,
    groups: &[(String, Vec<Choice>)],
) -> Option<Vec<std::path::PathBuf>> {
    use objc2_app_kit::{NSButton, NSControlStateValueOff, NSControlStateValueOn, NSScrollView, NSStackView, NSView};
    let mtm = mtm();
    let alert = alert(mtm, message, informative);
    alert.addButtonWithTitle(&ns(ok));
    alert.addButtonWithTitle(&ns(cancel));
    let mut views: Vec<Retained<NSView>> = Vec::new();
    let mut boxes: Vec<(Retained<NSButton>, std::path::PathBuf)> = Vec::new();
    for (heading, choices) in groups.iter().filter(|(_, choices)| !choices.is_empty()) {
        if !heading.is_empty() {
            let title = NSTextField::labelWithString(&ns(heading), mtm);
            title.setFont(Some(&objc2_app_kit::NSFont::boldSystemFontOfSize(
                objc2_app_kit::NSFont::smallSystemFontSize(),
            )));
            views.push(title.into_super().into_super());
        }
        for choice in choices {
            let button = unsafe { NSButton::checkboxWithTitle_target_action(&ns(&choice.label), None, None, mtm) };
            button.setState(if choice.checked { NSControlStateValueOn } else { NSControlStateValueOff });
            button.setToolTip(Some(&ns(&choice.path.display().to_string())));
            views.push(button.clone().into_super().into_super());
            boxes.push((button, choice.path.clone()));
        }
    }
    let refs: Vec<&NSView> = views.iter().map(|v| &**v).collect();
    let stack = NSStackView::stackViewWithViews(&objc2_foundation::NSArray::from_slice(&refs), mtm);
    stack.setOrientation(objc2_app_kit::NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
    stack.setSpacing(4.0);
    let fitting = stack.fittingSize();
    let width = fitting.width.clamp(300.0, 600.0);
    stack.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, fitting.height)));
    // Many items scroll.
    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, fitting.height.min(300.0))),
    );
    scroll.setDocumentView(Some(&stack));
    scroll.setHasVerticalScroller(fitting.height > 300.0);
    scroll.setDrawsBackground(false);
    alert.setAccessoryView(Some(&scroll));
    alert.layout();
    if run(&alert) != 0 {
        return None;
    }
    let chosen: Vec<std::path::PathBuf> =
        boxes.iter().filter(|(b, _)| b.state() != NSControlStateValueOff).map(|(_, p)| p.clone()).collect();
    (!chosen.is_empty()).then_some(chosen)
}
