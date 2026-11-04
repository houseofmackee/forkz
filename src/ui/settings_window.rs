//! The Settings window (Command-comma): General, Transfers, and SFTP.
//! Every change applies right away and is saved.

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate,
    NSGridCellPlacement, NSGridView, NSModalResponseOK, NSOpenPanel, NSPopUpButton, NSTabView, NSTabViewItem,
    NSTextField, NSTextFieldDelegate, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use super::tools;
use super::util::{file_url, mtm, ns, url_path};
use crate::fs::ops::Resolution;

/// The choices for "when a file exists", in popup order.
const CONFLICT_CHOICES: [(&str, Option<Resolution>); 5] = [
    ("Ask", None),
    ("Replace", Some(Resolution::Replace)),
    ("Replace if Older", Some(Resolution::ReplaceOlder)),
    ("Keep Both", Some(Resolution::KeepBoth)),
    ("Skip", Some(Resolution::Skip)),
];

const TRANSFER_LIMITS: [usize; 8] = [1, 2, 3, 4, 5, 6, 8, 10];

struct Controls {
    show_hidden: Retained<NSButton>,
    start_on_parent: Retained<NSButton>,
    terminal: Retained<NSPopUpButton>,
    terminals: Vec<&'static str>,
    compare: Retained<NSPopUpButton>,
    compare_tools: Vec<&'static str>,
    auto_updates: Retained<NSButton>,
    limit: Retained<NSPopUpButton>,
    keep_dates: Retained<NSButton>,
    download: Retained<NSPopUpButton>,
    upload: Retained<NSPopUpButton>,
    timeout: Retained<NSTextField>,
    key_files: Retained<NSTextField>,
}

pub struct WindowIvars {
    window: OnceCell<Retained<NSWindow>>,
    controls: OnceCell<Controls>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzSettingsWindow"]
    #[ivars = WindowIvars]
    struct SettingsWindow;

    impl SettingsWindow {
        #[unsafe(method(changed:))]
        fn changed(&self, _sender: Option<&AnyObject>) {
            self.save();
        }

        #[unsafe(method(addKeyFile:))]
        fn add_key_file(&self, _sender: Option<&AnyObject>) {
            self.pick_key_file();
        }

        #[unsafe(method(showKnownHosts:))]
        fn show_known_hosts(&self, _sender: Option<&AnyObject>) {
            reveal_known_hosts();
        }
    }

    unsafe impl NSObjectProtocol for SettingsWindow {}

    // Text fields save when editing ends (Return, Tab, or leaving them).
    unsafe impl NSControlTextEditingDelegate for SettingsWindow {
        #[unsafe(method(controlTextDidEndEditing:))]
        fn text_did_end_editing(&self, _notification: &NSNotification) {
            self.save();
        }
    }

    unsafe impl NSTextFieldDelegate for SettingsWindow {}
);

thread_local! {
    static WINDOW: RefCell<Option<Retained<SettingsWindow>>> = const { RefCell::new(None) };
}

/// Shows the Settings window (made the first time), with current values.
pub fn show() {
    let mtm = mtm();
    let existing = WINDOW.with(|w| w.borrow().clone());
    let settings = existing.unwrap_or_else(|| {
        let made = SettingsWindow::new(mtm);
        WINDOW.with(|w| *w.borrow_mut() = Some(made.clone()));
        made
    });
    settings.load();
    if let Some(window) = settings.ivars().window.get() {
        objc2_app_kit::NSApplication::sharedApplication(mtm).activate();
        window.makeKeyAndOrderFront(None);
    }
}

fn checkbox(title: &str, target: &SettingsWindow, mtm: MainThreadMarker) -> Retained<NSButton> {
    unsafe { NSButton::checkboxWithTitle_target_action(&ns(title), Some(target), Some(sel!(changed:)), mtm) }
}

fn popup(titles: &[&str], target: &SettingsWindow, mtm: MainThreadMarker) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    let titles: Vec<Retained<NSString>> = titles.iter().map(|t| ns(t)).collect();
    popup.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
    unsafe {
        popup.setTarget(Some(target));
        popup.setAction(Some(sel!(changed:)));
    }
    popup
}

fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    Retained::into_super(Retained::into_super(NSTextField::labelWithString(&ns(text), mtm)))
}

fn note(text: &str, mtm: MainThreadMarker) -> Retained<NSView> {
    let field = NSTextField::wrappingLabelWithString(&ns(text), mtm);
    field.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(objc2_app_kit::NSFont::smallSystemFontSize())));
    field.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
    field.widthAnchor().constraintLessThanOrEqualToConstant(300.0).setActive(true);
    Retained::into_super(Retained::into_super(field))
}

fn view<T: Message + AsRef<NSView>>(v: &T) -> Retained<NSView> {
    v.as_ref().retain()
}

fn empty(mtm: MainThreadMarker) -> Retained<NSView> {
    NSView::new(mtm)
}

/// A tab's content: rows of (label, control), labels right-aligned.
fn grid(rows: Vec<(Retained<NSView>, Retained<NSView>)>, mtm: MainThreadMarker) -> Retained<NSView> {
    let rows: Vec<Retained<NSArray<NSView>>> =
        rows.into_iter().map(|(l, c)| NSArray::from_retained_slice(&[l, c])).collect();
    let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
    grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
    grid.setRowSpacing(10.0);
    grid.setColumnSpacing(10.0);
    let container = NSView::new(mtm);
    container.addSubview(&grid);
    grid.setTranslatesAutoresizingMaskIntoConstraints(false);
    grid.topAnchor().constraintEqualToAnchor_constant(&container.topAnchor(), 20.0).setActive(true);
    grid.centerXAnchor().constraintEqualToAnchor(&container.centerXAnchor()).setActive(true);
    container
}

impl SettingsWindow {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(WindowIvars { window: OnceCell::new(), controls: OnceCell::new() });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        this.build(mtm);
        this
    }

    fn build(&self, mtm: MainThreadMarker) {
        let terminals = tools::installed_terminals();
        let compare_tools = tools::installed_compare_tools();
        let terminal_names: Vec<&str> = terminals.iter().map(|t| t.1).collect();
        let compare_names: Vec<&str> =
            if compare_tools.is_empty() { vec!["None installed"] } else { compare_tools.iter().map(|t| t.1).collect() };
        let limit_titles: Vec<String> = TRANSFER_LIMITS.iter().map(|n| n.to_string()).collect();
        let limit_titles: Vec<&str> = limit_titles.iter().map(String::as_str).collect();
        let conflict_titles: Vec<&str> = CONFLICT_CHOICES.iter().map(|c| c.0).collect();

        let controls = Controls {
            show_hidden: checkbox("Show hidden files", self, mtm),
            start_on_parent: checkbox("Select \u{201c}..\u{201d} when opening a folder", self, mtm),
            terminal: popup(&terminal_names, self, mtm),
            terminals: terminals.iter().map(|t| t.0).collect(),
            compare: popup(&compare_names, self, mtm),
            compare_tools: compare_tools.iter().map(|t| t.0).collect(),
            auto_updates: checkbox("Check for updates automatically", self, mtm),
            limit: popup(&limit_titles, self, mtm),
            keep_dates: checkbox("Keep modification dates", self, mtm),
            download: popup(&conflict_titles, self, mtm),
            upload: popup(&conflict_titles, self, mtm),
            timeout: NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(60.0, 22.0)),
            ),
            key_files: NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 60.0)),
            ),
        };
        controls.compare.setEnabled(!compare_tools.is_empty());
        controls.timeout.widthAnchor().constraintEqualToConstant(60.0).setActive(true);
        controls.key_files.widthAnchor().constraintEqualToConstant(300.0).setActive(true);
        controls.key_files.heightAnchor().constraintEqualToConstant(60.0).setActive(true);
        controls.key_files.setUsesSingleLineMode(false);
        controls.key_files.setPlaceholderString(Some(&ns("One key file per line")));
        if let Some(cell) = controls.key_files.cell() {
            cell.setWraps(true);
            cell.setScrollable(false);
        }
        for field in [&controls.timeout, &controls.key_files] {
            unsafe { field.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(self))) };
        }
        let add_key = unsafe {
            NSButton::buttonWithTitle_target_action(
                &ns("Add Key File\u{2026}"),
                Some(self),
                Some(sel!(addKeyFile:)),
                mtm,
            )
        };
        let known_hosts = unsafe {
            NSButton::buttonWithTitle_target_action(
                &ns("Show Trusted Host Keys"),
                Some(self),
                Some(sel!(showKnownHosts:)),
                mtm,
            )
        };
        let seconds = objc2_app_kit::NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[view(&*controls.timeout), label("seconds", mtm)]),
            mtm,
        );

        let general = grid(
            vec![
                (label("Files:", mtm), view(&*controls.show_hidden)),
                (empty(mtm), view(&*controls.start_on_parent)),
                (label("Open in Terminal uses:", mtm), view(&*controls.terminal)),
                (label("Compare Files uses:", mtm), view(&*controls.compare)),
                (label("Updates:", mtm), view(&*controls.auto_updates)),
            ],
            mtm,
        );
        let transfers = grid(
            vec![
                (label("Simultaneous transfers:", mtm), view(&*controls.limit)),
                (empty(mtm), note("Uploads and downloads beyond this wait their turn.", mtm)),
                (label("Dates:", mtm), view(&*controls.keep_dates)),
                (label("When a downloaded file exists:", mtm), view(&*controls.download)),
                (label("When an uploaded file exists:", mtm), view(&*controls.upload)),
            ],
            mtm,
        );
        let sftp = grid(
            vec![
                (label("Connection timeout:", mtm), view(&*seconds)),
                (label("Extra key files:", mtm), view(&*controls.key_files)),
                (empty(mtm), view(&*add_key)),
                (empty(mtm), note("Tried when a site has no key of its own, after the SSH agent and ~/.ssh.", mtm)),
                (label("Host keys:", mtm), view(&*known_hosts)),
            ],
            mtm,
        );

        let tabs = NSTabView::new(mtm);
        for (title, content) in [("General", general), ("Transfers", transfers), ("SFTP", sftp)] {
            let item = NSTabViewItem::new();
            item.setLabel(&ns(title));
            item.setView(Some(&content));
            tabs.addTabViewItem(&item);
        }

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 330.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&ns("forkz Settings"));
        window.setContentView(Some(&tabs));
        window.center();
        window.setFrameAutosaveName(&ns("ForkzSettings"));
        let _ = self.ivars().window.set(window);
        let _ = self.ivars().controls.set(controls);
    }

    /// Puts the current settings into the controls.
    fn load(&self) {
        let Some(c) = self.ivars().controls.get() else { return };
        let Some(settings) = super::current_settings() else { return };
        let set = |b: &NSButton, on: bool| b.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        set(&c.show_hidden, settings.show_hidden);
        set(&c.start_on_parent, settings.general.start_on_parent);
        let terminal = c.terminals.iter().position(|k| *k == settings.general.terminal).unwrap_or(0);
        c.terminal.selectItemAtIndex(terminal as isize);
        let compare = c.compare_tools.iter().position(|k| *k == settings.general.compare_tool).unwrap_or(0);
        c.compare.selectItemAtIndex(compare as isize);
        set(&c.auto_updates, crate::updates::checks_automatically());
        c.auto_updates.setEnabled(crate::updates::available());
        let limit = TRANSFER_LIMITS.iter().position(|n| *n == settings.transfers.max_transfers).unwrap_or(1);
        c.limit.selectItemAtIndex(limit as isize);
        set(&c.keep_dates, settings.transfers.preserve_timestamps);
        let conflict = |r: Option<Resolution>| CONFLICT_CHOICES.iter().position(|c| c.1 == r).unwrap_or(0) as isize;
        c.download.selectItemAtIndex(conflict(settings.transfers.download_conflict));
        c.upload.selectItemAtIndex(conflict(settings.transfers.upload_conflict));
        c.timeout.setStringValue(&ns(&settings.sftp.timeout_seconds.to_string()));
        let keys: Vec<String> = settings.sftp.key_files.iter().map(|k| k.display().to_string()).collect();
        c.key_files.setStringValue(&ns(&keys.join("\n")));
    }

    /// Reads the controls into the settings, saves, and applies them.
    fn save(&self) {
        let Some(c) = self.ivars().controls.get() else { return };
        let on = |b: &NSButton| b.state() == NSControlStateValueOn;
        let index = |p: &NSPopUpButton| p.indexOfSelectedItem().max(0) as usize;
        let show_hidden = on(&c.show_hidden);
        let auto_updates = on(&c.auto_updates);
        let timeout =
            c.timeout.stringValue().to_string().trim().parse::<u64>().ok().filter(|&t| (1..=600).contains(&t));
        let key_files: Vec<PathBuf> = c
            .key_files
            .stringValue()
            .to_string()
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        super::update_settings(|s| {
            s.show_hidden = show_hidden;
            s.general.start_on_parent = on(&c.start_on_parent);
            if let Some(key) = c.terminals.get(index(&c.terminal)) {
                s.general.terminal = key.to_string();
            }
            if let Some(key) = c.compare_tools.get(index(&c.compare)) {
                s.general.compare_tool = key.to_string();
            }
            s.transfers.max_transfers = TRANSFER_LIMITS.get(index(&c.limit)).copied().unwrap_or(2);
            s.transfers.preserve_timestamps = on(&c.keep_dates);
            s.transfers.download_conflict = CONFLICT_CHOICES.get(index(&c.download)).and_then(|c| c.1);
            s.transfers.upload_conflict = CONFLICT_CHOICES.get(index(&c.upload)).and_then(|c| c.1);
            if let Some(timeout) = timeout {
                s.sftp.timeout_seconds = timeout;
            }
            s.sftp.key_files = key_files;
        });
        if crate::updates::available() && auto_updates != crate::updates::checks_automatically() {
            crate::updates::set_checks_automatically(auto_updates);
        }
        // An invalid timeout goes back to the saved one.
        if timeout.is_none()
            && let Some(settings) = super::current_settings()
        {
            c.timeout.setStringValue(&ns(&settings.sftp.timeout_seconds.to_string()));
        }
    }

    fn pick_key_file(&self) {
        let Some(c) = self.ivars().controls.get() else { return };
        let panel = NSOpenPanel::openPanel(self.mtm());
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setShowsHiddenFiles(true);
        if let Some(ssh) = dirs::home_dir().map(|h| h.join(".ssh")) {
            panel.setDirectoryURL(Some(&file_url(&ssh)));
        }
        if panel.runModal() == NSModalResponseOK
            && let Some(path) = panel.URL().and_then(|u| url_path(&u))
        {
            let mut text = c.key_files.stringValue().to_string();
            if !text.trim().is_empty() {
                text.push('\n');
            }
            text.push_str(&path.display().to_string());
            c.key_files.setStringValue(&ns(&text));
            self.save();
        }
    }
}

/// Shows forkz's trusted host keys file in Finder (made empty if new).
fn reveal_known_hosts() {
    let Some(file) = crate::vfs::known_hosts::forkz_file() else { return };
    if !file.exists() {
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&file, "");
    }
    let urls = NSArray::from_retained_slice(&[file_url(&file)]);
    objc2_app_kit::NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&urls);
}
