//! The Settings window (Command-comma): General, Appearance, Transfers,
//! SFTP, and Shortcuts. Every change applies right away and is saved.

use std::cell::{OnceCell, RefCell};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate, NSEvent,
    NSEventMask, NSEventModifierFlags, NSFont, NSGridCellPlacement, NSGridView, NSModalResponseOK, NSOpenPanel,
    NSPopUpButton, NSScrollView, NSStackView, NSTabView, NSTabViewItem, NSTableColumn, NSTableView,
    NSTableViewDataSource, NSTableViewDelegate, NSTextField, NSTextFieldDelegate, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use super::commands::{self, Command};
use super::tools;
use super::util::{file_url, mtm, ns, url_path};
use crate::fs::ops::Resolution;
use crate::settings::{ROW_SPACINGS, TEXT_SIZES, THEMES};

/// The choices for "when a file exists", in popup order.
const CONFLICT_CHOICES: [(&str, Option<Resolution>); 5] = [
    ("Ask", None),
    ("Replace", Some(Resolution::Replace)),
    ("Replace if Older", Some(Resolution::ReplaceOlder)),
    ("Keep Both", Some(Resolution::KeepBoth)),
    ("Skip", Some(Resolution::Skip)),
];

const TRANSFER_LIMITS: [usize; 8] = [1, 2, 3, 4, 5, 6, 8, 10];

/// The choices for "when an error happens", in popup order.
const ERROR_RULES: [(&str, &str); 3] =
    [("Ask what to do", "ask"), ("Skip the item", "skip"), ("Try 3 more times, then skip", "retry")];

struct Controls {
    show_hidden: Retained<NSButton>,
    start_on_parent: Retained<NSButton>,
    terminal: Retained<NSPopUpButton>,
    terminals: Vec<&'static str>,
    compare: Retained<NSPopUpButton>,
    compare_tools: Vec<&'static str>,
    auto_updates: Retained<NSButton>,
    default_viewer: Retained<NSButton>,
    limit: Retained<NSPopUpButton>,
    keep_dates: Retained<NSButton>,
    download: Retained<NSPopUpButton>,
    upload: Retained<NSPopUpButton>,
    ask_upload: Retained<NSButton>,
    upload_limit: Retained<NSTextField>,
    download_limit: Retained<NSTextField>,
    on_error: Retained<NSPopUpButton>,
    timeout: Retained<NSTextField>,
    key_files: Retained<NSTextField>,
    theme: Retained<NSPopUpButton>,
    /// The theme ids of the popup's items (by item tag).
    theme_ids: RefCell<Vec<String>>,
    text_size: Retained<NSPopUpButton>,
    row_spacing: Retained<NSPopUpButton>,
    alternating: Retained<NSButton>,
    shortcuts: Retained<NSTableView>,
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

        /// Applied to macOS right away (not a forkz setting).
        #[unsafe(method(defaultViewer:))]
        fn default_viewer(&self, sender: &NSButton) {
            let on = sender.state() == NSControlStateValueOn;
            if let Err(e) = super::file_viewer::set_default(on) {
                super::dialogs::show_error("forkz could not change what \u{201c}Show in Finder\u{201d} opens.".into(), e);
            }
            sender.setState(if super::file_viewer::is_default() { NSControlStateValueOn } else { NSControlStateValueOff });
        }

        #[unsafe(method(customizeTheme:))]
        fn customize_theme(&self, _sender: Option<&AnyObject>) {
            self.customize();
        }

        #[unsafe(method(openThemes:))]
        fn open_themes(&self, _sender: Option<&AnyObject>) {
            if let Some(folder) = crate::theme::folder() {
                let _ = std::fs::create_dir_all(&folder);
                objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&file_url(&folder));
            }
        }

        #[unsafe(method(addKeyFile:))]
        fn add_key_file(&self, _sender: Option<&AnyObject>) {
            self.pick_key_file();
        }

        #[unsafe(method(showKnownHosts:))]
        fn show_known_hosts(&self, _sender: Option<&AnyObject>) {
            reveal_known_hosts();
        }

        #[unsafe(method(changeShortcut:))]
        fn change_shortcut_action(&self, _sender: Option<&AnyObject>) {
            self.change_shortcut();
        }

        #[unsafe(method(resetShortcut:))]
        fn reset_shortcut_action(&self, _sender: Option<&AnyObject>) {
            self.reset_shortcut(false);
        }

        #[unsafe(method(resetAllShortcuts:))]
        fn reset_all_shortcuts_action(&self, _sender: Option<&AnyObject>) {
            self.reset_shortcut(true);
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

    unsafe impl NSTableViewDataSource for SettingsWindow {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            Command::ALL.len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for SettingsWindow {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            _table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            self.shortcut_cell(column, row)
        }
    }
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

/// A field followed by "KB/s".
fn kbps(field: &NSTextField, mtm: MainThreadMarker) -> Retained<NSStackView> {
    NSStackView::stackViewWithViews(
        &NSArray::from_retained_slice(&[view(field), label("KB/s (empty for none)", mtm)]),
        mtm,
    )
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
            default_viewer: unsafe {
                NSButton::checkboxWithTitle_target_action(
                    &ns("Use forkz for \u{201c}Show in Finder\u{201d} in other apps"),
                    Some(self),
                    Some(sel!(defaultViewer:)),
                    mtm,
                )
            },
            limit: popup(&limit_titles, self, mtm),
            keep_dates: checkbox("Keep modification dates", self, mtm),
            download: popup(&conflict_titles, self, mtm),
            upload: popup(&conflict_titles, self, mtm),
            ask_upload: checkbox("Ask before uploading a saved server file", self, mtm),
            upload_limit: NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(80.0, 22.0)),
            ),
            download_limit: NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(80.0, 22.0)),
            ),
            on_error: popup(&ERROR_RULES.iter().map(|e| e.0).collect::<Vec<_>>(), self, mtm),
            timeout: NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(60.0, 22.0)),
            ),
            key_files: NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 60.0)),
            ),
            theme: popup(&[], self, mtm),
            theme_ids: RefCell::new(Vec::new()),
            text_size: popup(
                &TEXT_SIZES
                    .iter()
                    .map(|s| format!("{s} pt"))
                    .collect::<Vec<_>>()
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                self,
                mtm,
            ),
            row_spacing: popup(&ROW_SPACINGS.iter().map(|r| r.1).collect::<Vec<_>>(), self, mtm),
            alternating: checkbox("Alternating row colors", self, mtm),
            shortcuts: NSTableView::initWithFrame(NSTableView::alloc(mtm), NSRect::ZERO),
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
        for field in [&controls.upload_limit, &controls.download_limit] {
            field.widthAnchor().constraintEqualToConstant(80.0).setActive(true);
            field.setPlaceholderString(Some(&ns("No limit")));
        }
        for field in [&controls.timeout, &controls.key_files, &controls.upload_limit, &controls.download_limit] {
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
                (label("Folders:", mtm), view(&*controls.default_viewer)),
                (
                    empty(mtm),
                    note(
                        "Files and folders that other apps show \u{201c}in Finder\u{201d} open in forkz instead. macOS keeps Finder as the app that opens folders. Only the forkz app can be used, not a build run from source.",
                        mtm,
                    ),
                ),
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
                (label("Opened server files:", mtm), view(&*controls.ask_upload)),
                (
                    empty(mtm),
                    note("Saving a server file you opened uploads it back, unless you choose to be asked.", mtm),
                ),
                (label("Upload speed limit:", mtm), view(&*kbps(&controls.upload_limit, mtm))),
                (label("Download speed limit:", mtm), view(&*kbps(&controls.download_limit, mtm))),
                (label("When an error happens:", mtm), view(&*controls.on_error)),
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

        let customize = unsafe {
            NSButton::buttonWithTitle_target_action(
                &ns("Customize\u{2026}"),
                Some(self),
                Some(sel!(customizeTheme:)),
                mtm,
            )
        };
        let themes_folder = unsafe {
            NSButton::buttonWithTitle_target_action(&ns("Themes Folder"), Some(self), Some(sel!(openThemes:)), mtm)
        };
        let theme_row = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[view(&*controls.theme), view(&*customize), view(&*themes_folder)]),
            mtm,
        );
        theme_row.setSpacing(8.0);
        let appearance = grid(
            vec![
                (label("Theme:", mtm), view(&*theme_row)),
                (
                    empty(mtm),
                    note(
                        "Colors for the file lists and the sidebar; a light or dark theme also sets macOS\u{2019}s appearance for forkz. Your own themes are TOML files in the themes folder.",
                        mtm,
                    ),
                ),
                (label("Text size:", mtm), view(&*controls.text_size)),
                (label("Row spacing:", mtm), view(&*controls.row_spacing)),
                (empty(mtm), view(&*controls.alternating)),
                (empty(mtm), note("For the file lists.", mtm)),
            ],
            mtm,
        );
        let shortcuts = self.shortcuts_tab(&controls.shortcuts, mtm);

        let tabs = NSTabView::new(mtm);
        for (title, content) in [
            ("General", general),
            ("Appearance", appearance),
            ("Transfers", transfers),
            ("SFTP", sftp),
            ("Shortcuts", shortcuts),
        ] {
            let item = NSTabViewItem::new();
            item.setLabel(&ns(title));
            item.setView(Some(&content));
            tabs.addTabViewItem(&item);
        }

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(600.0, 460.0)),
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

    /// The theme popup: macOS's choices, the built-in themes, then the
    /// user's, with `selected` chosen.
    fn fill_themes(&self, selected: &str) {
        let Some(c) = self.ivars().controls.get() else { return };
        let mtm = self.mtm();
        let mut entries: Vec<Option<(String, String)>> =
            THEMES.iter().map(|(id, title)| Some(((*id).to_string(), (*title).to_string()))).collect();
        entries.push(None);
        entries.extend(crate::theme::built_in().into_iter().map(|(id, t)| Some((id, t.name))));
        let user = crate::theme::user_themes();
        if !user.is_empty() {
            entries.push(None);
            entries.extend(user.into_iter().map(|(id, t)| Some((id, t.name))));
        }
        c.theme.removeAllItems();
        let menu = c.theme.menu().expect("a popup has a menu");
        let mut ids = Vec::new();
        for entry in entries {
            match entry {
                None => menu.addItem(&objc2_app_kit::NSMenuItem::separatorItem(mtm)),
                Some((id, title)) => {
                    let item = unsafe {
                        objc2_app_kit::NSMenuItem::initWithTitle_action_keyEquivalent(
                            objc2_app_kit::NSMenuItem::alloc(mtm),
                            &ns(&title),
                            None,
                            &ns(""),
                        )
                    };
                    item.setTag(ids.len() as isize);
                    menu.addItem(&item);
                    ids.push(id);
                }
            }
        }
        let index = ids.iter().position(|id| id == selected).unwrap_or(0);
        c.theme.selectItemWithTag(index as isize);
        *c.theme_ids.borrow_mut() = ids;
    }

    /// Customize…: edits the chosen theme (a built-in one or macOS's colors
    /// start a new theme of the user's own) and switches to it.
    fn customize(&self) {
        let Some(current) = super::current_settings().map(|s| s.appearance.theme) else { return };
        let mut start = crate::theme::find(&current).unwrap_or_else(|| {
            let dark = objc2_app_kit::NSApplication::sharedApplication(self.mtm())
                .effectiveAppearance()
                .name()
                .to_string()
                .contains("Dark");
            crate::theme::find(if dark { "one-dark" } else { "github-light" }).expect("built in")
        });
        if !current.starts_with("user:") {
            start.name = format!(
                "{} Custom",
                if crate::theme::find(&current).is_some() { start.name.as_str() } else { "My Theme" }
            );
        }
        let Some(theme) = super::theme_editor::edit(&start) else { return };
        match crate::theme::save_user(&theme) {
            Ok(id) => {
                super::update_settings(|s| s.appearance.theme = id.clone());
                // Edited in place: the same id, new colors.
                super::refresh_appearance();
                self.fill_themes(&id);
            }
            Err(e) => super::dialogs::show_error("The theme could not be saved.".into(), e.to_string()),
        }
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
        set(&c.default_viewer, super::file_viewer::is_default());
        c.default_viewer.setEnabled(super::file_viewer::available());
        c.auto_updates.setEnabled(crate::updates::available());
        let limit = TRANSFER_LIMITS.iter().position(|n| *n == settings.transfers.max_transfers).unwrap_or(1);
        c.limit.selectItemAtIndex(limit as isize);
        set(&c.keep_dates, settings.transfers.preserve_timestamps);
        let conflict = |r: Option<Resolution>| CONFLICT_CHOICES.iter().position(|c| c.1 == r).unwrap_or(0) as isize;
        c.download.selectItemAtIndex(conflict(settings.transfers.download_conflict));
        c.upload.selectItemAtIndex(conflict(settings.transfers.upload_conflict));
        set(&c.ask_upload, settings.transfers.ask_before_upload_on_save);
        let limit_text = |kbps: u64| if kbps == 0 { String::new() } else { kbps.to_string() };
        c.upload_limit.setStringValue(&ns(&limit_text(settings.transfers.upload_limit_kbps)));
        c.download_limit.setStringValue(&ns(&limit_text(settings.transfers.download_limit_kbps)));
        c.on_error.selectItemAtIndex(
            ERROR_RULES.iter().position(|e| e.1 == settings.transfers.on_error).unwrap_or(0) as isize
        );
        c.timeout.setStringValue(&ns(&settings.sftp.timeout_seconds.to_string()));
        let keys: Vec<String> = settings.sftp.key_files.iter().map(|k| k.display().to_string()).collect();
        c.key_files.setStringValue(&ns(&keys.join("\n")));
        let a = &settings.appearance;
        self.fill_themes(&a.theme);
        let size = TEXT_SIZES.iter().position(|s| (*s - a.text_size).abs() < 0.1).unwrap_or(2);
        c.text_size.selectItemAtIndex(size as isize);
        c.row_spacing.selectItemAtIndex(ROW_SPACINGS.iter().position(|r| r.0 == a.row_spacing).unwrap_or(1) as isize);
        set(&c.alternating, a.alternating_rows);
        c.shortcuts.reloadData();
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
            s.transfers.ask_before_upload_on_save = on(&c.ask_upload);
            let limit = |f: &NSTextField| f.stringValue().to_string().trim().parse::<u64>().unwrap_or(0);
            s.transfers.upload_limit_kbps = limit(&c.upload_limit);
            s.transfers.download_limit_kbps = limit(&c.download_limit);
            if let Some(rule) = ERROR_RULES.get(index(&c.on_error)) {
                s.transfers.on_error = rule.1.to_string();
            }
            if let Some(timeout) = timeout {
                s.sftp.timeout_seconds = timeout;
            }
            s.sftp.key_files = key_files;
            let tag = c.theme.selectedItem().map_or(0, |item| item.tag());
            if let Some(theme) = usize::try_from(tag).ok().and_then(|t| c.theme_ids.borrow().get(t).cloned()) {
                s.appearance.theme = theme;
            }
            if let Some(size) = TEXT_SIZES.get(index(&c.text_size)) {
                s.appearance.text_size = *size;
            }
            if let Some(spacing) = ROW_SPACINGS.get(index(&c.row_spacing)) {
                s.appearance.row_spacing = spacing.0.to_string();
            }
            s.appearance.alternating_rows = on(&c.alternating);
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

impl SettingsWindow {
    fn shortcuts_tab(&self, table: &NSTableView, mtm: MainThreadMarker) -> Retained<NSView> {
        for (id, title, width) in [("command", "Command", 330.0), ("shortcut", "Shortcut", 150.0)] {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
            column.setTitle(&ns(title));
            column.setWidth(width);
            table.addTableColumn(&column);
        }
        table.setUsesAlternatingRowBackgroundColors(true);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
            table.setTarget(Some(self));
            table.setDoubleAction(Some(sel!(changeShortcut:)));
        }
        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(table));
        scroll.setHasVerticalScroller(true);
        let button = |title: &str, action| unsafe {
            NSButton::buttonWithTitle_target_action(&ns(title), Some(self), Some(action), mtm)
        };
        let buttons = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[
                view(&*button("Change Shortcut\u{2026}", sel!(changeShortcut:))),
                view(&*button("Use Default", sel!(resetShortcut:))),
                view(&*button("Reset All", sel!(resetAllShortcuts:))),
            ]),
            mtm,
        );
        let stack = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[
                view(&*scroll),
                view(&*buttons),
                note("Double-click a command to change its shortcut. Changed shortcuts are in bold.", mtm),
            ]),
            mtm,
        );
        stack.setOrientation(objc2_app_kit::NSUserInterfaceLayoutOrientation::Vertical);
        stack.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
        stack.setEdgeInsets(NSEdgeInsets { top: 16.0, left: 16.0, bottom: 16.0, right: 16.0 });
        scroll.leadingAnchor().constraintEqualToAnchor_constant(&stack.leadingAnchor(), 16.0).setActive(true);
        scroll.trailingAnchor().constraintEqualToAnchor_constant(&stack.trailingAnchor(), -16.0).setActive(true);
        scroll.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Vertical);
        scroll.heightAnchor().constraintGreaterThanOrEqualToConstant(200.0).setActive(true);
        // The tab sizes its view by its frame: the stack fills a container.
        let container = NSView::new(mtm);
        stack.setTranslatesAutoresizingMaskIntoConstraints(false);
        container.addSubview(&stack);
        stack.topAnchor().constraintEqualToAnchor(&container.topAnchor()).setActive(true);
        stack.bottomAnchor().constraintEqualToAnchor(&container.bottomAnchor()).setActive(true);
        stack.leadingAnchor().constraintEqualToAnchor(&container.leadingAnchor()).setActive(true);
        stack.trailingAnchor().constraintEqualToAnchor(&container.trailingAnchor()).setActive(true);
        container
    }

    fn shortcut_cell(&self, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
        let command = *Command::ALL.get(usize::try_from(row).ok()?)?;
        let overrides = super::current_settings().map(|s| s.shortcuts).unwrap_or_default();
        let changed = overrides.contains_key(&command.id());
        let field = if column?.identifier().to_string() == "shortcut" {
            let (key, modifiers) = command.effective_shortcut(&overrides);
            let text = commands::display(&key, modifiers);
            let field = NSTextField::labelWithString(&ns(if text.is_empty() { "None" } else { &text }), self.mtm());
            if text.is_empty() {
                field.setTextColor(Some(&objc2_app_kit::NSColor::tertiaryLabelColor()));
            }
            field
        } else {
            NSTextField::labelWithString(&ns(command.title()), self.mtm())
        };
        if changed {
            field.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        }
        Some(Retained::into_super(Retained::into_super(field)))
    }

    fn selected_command(&self) -> Option<Command> {
        let c = self.ivars().controls.get()?;
        let row = usize::try_from(c.shortcuts.selectedRow()).ok()?;
        Command::ALL.get(row).copied()
    }

    fn change_shortcut(&self) {
        let Some(command) = self.selected_command() else { return };
        let Some((key, modifiers)) = record_shortcut(command.title()) else { return };
        let overrides = super::current_settings().map(|s| s.shortcuts).unwrap_or_default();
        if commands::RESERVED.iter().any(|(k, m)| *k == key && *m == modifiers) {
            super::dialogs::show_info(
                format!("{} is taken by the app menu or Edit menu.", commands::display(&key, modifiers)),
                "Choose another shortcut.".into(),
            );
            return;
        }
        let taken = commands::command_using(&key, modifiers, &overrides, command);
        if let Some(other) = taken {
            let message = format!(
                "{} is used by \u{201c}{}\u{201d}.",
                commands::display(&key, modifiers),
                other.title().trim_end_matches('\u{2026}')
            );
            let detail =
                format!("Use it for \u{201c}{}\u{201d} instead?", command.title().trim_end_matches('\u{2026}'));
            if !super::dialogs::confirm(&message, &detail, "Use It", false) {
                return;
            }
        }
        let spec = if key.is_empty() { String::new() } else { commands::spec(&key, modifiers) };
        super::update_settings(|s| {
            // Only a menu shortcut is taken away (a list key just loses to
            // the menu).
            if let Some(other) = taken
                && other.effective_shortcut(&s.shortcuts).0 == key
            {
                s.shortcuts.insert(other.id(), String::new());
            }
            let (default_key, default_modifiers) = command.shortcut();
            let default =
                if default_key.is_empty() { String::new() } else { commands::spec(default_key, default_modifiers) };
            if spec == default {
                s.shortcuts.remove(&command.id());
            } else {
                s.shortcuts.insert(command.id(), spec.clone());
            }
        });
        if let Some(c) = self.ivars().controls.get() {
            c.shortcuts.reloadData();
        }
    }

    fn reset_shortcut(&self, all: bool) {
        if all {
            if !super::dialogs::confirm("Use the default shortcuts for every command?", "", "Reset All", false) {
                return;
            }
            super::update_settings(|s| s.shortcuts.clear());
        } else if let Some(command) = self.selected_command() {
            super::update_settings(|s| {
                s.shortcuts.remove(&command.id());
            });
        }
        if let Some(c) = self.ivars().controls.get() {
            c.shortcuts.reloadData();
        }
    }
}

/// Asks for a new shortcut by recording the keys pressed. None if
/// cancelled; an empty key to remove the shortcut.
fn record_shortcut(title: &str) -> Option<(String, NSEventModifierFlags)> {
    let mtm = mtm();
    let alert = objc2_app_kit::NSAlert::new(mtm);
    alert.setMessageText(&ns(&format!("New shortcut for \u{201c}{}\u{201d}", title.trim_end_matches('\u{2026}'))));
    alert.setInformativeText(&ns("Press the keys now. Shortcuts need Command, Control, or Option, or a function key."));
    alert.addButtonWithTitle(&ns("Cancel"));
    alert.addButtonWithTitle(&ns("Remove Shortcut"));
    let shown = NSTextField::labelWithString(&ns("\u{2026}"), mtm);
    shown.setFont(Some(&NSFont::systemFontOfSize_weight(22.0, unsafe { objc2_app_kit::NSFontWeightMedium })));
    shown.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, 32.0)));
    alert.setAccessoryView(Some(&shown));
    let captured: Rc<RefCell<Option<(String, NSEventModifierFlags)>>> = Rc::new(RefCell::new(None));
    const RECORDED: isize = 4242;
    let block = {
        let captured = captured.clone();
        let shown = shown.clone();
        block2::RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            let event = unsafe { event.as_ref() };
            let modifiers = commands::shortcut_modifiers(event.modifierFlags());
            let code = event.keyCode();
            let named = match code {
                51 => Some("\u{8}"),
                117 => Some("\u{f728}"),
                36 | 76 => Some("\r"),
                48 => Some("\t"),
                49 => Some(" "),
                53 => Some("\u{1b}"),
                123 => Some("\u{f702}"),
                124 => Some("\u{f703}"),
                125 => Some("\u{f701}"),
                126 => Some("\u{f700}"),
                115 => Some("\u{f729}"),
                119 => Some("\u{f72b}"),
                116 => Some("\u{f72c}"),
                121 => Some("\u{f72d}"),
                _ => None,
            };
            // Function keys by key code (F1 to F20).
            const FUNCTION_CODES: [u16; 20] =
                [122, 120, 99, 118, 96, 97, 98, 100, 101, 109, 103, 111, 105, 107, 113, 106, 64, 79, 80, 90];
            let function = FUNCTION_CODES
                .iter()
                .position(|c| *c == code)
                .and_then(|n| char::from_u32(0xf704 + n as u32))
                .map(String::from);
            let key = match named.map(str::to_string).or(function) {
                Some(key) => key,
                None => {
                    let plain = event.charactersByApplyingModifiers(NSEventModifierFlags(0)).map(|s| s.to_string());
                    plain
                        .or_else(|| event.charactersIgnoringModifiers().map(|s| s.to_string()))
                        .unwrap_or_default()
                        .to_lowercase()
                }
            };
            let app = objc2_app_kit::NSApplication::sharedApplication(unsafe { MainThreadMarker::new_unchecked() });
            if key == "\u{1b}" && modifiers.0 == 0 {
                app.stopModalWithCode(objc2_app_kit::NSAlertFirstButtonReturn);
                return std::ptr::null_mut();
            }
            let strong = NSEventModifierFlags::Command | NSEventModifierFlags::Control | NSEventModifierFlags::Option;
            if !key.is_empty() && (modifiers.intersects(strong) || commands::is_function_key(&key)) {
                shown.setStringValue(&ns(&commands::display(&key, modifiers)));
                *captured.borrow_mut() = Some((key, modifiers));
                app.stopModalWithCode(RECORDED);
            } else {
                shown.setStringValue(&ns("Add \u{2318}, \u{2303}, or \u{2325}"));
            }
            std::ptr::null_mut()
        })
    };
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block) };
    let response = alert.runModal();
    if let Some(monitor) = monitor {
        unsafe { NSEvent::removeMonitor(&monitor) };
    }
    match response {
        RECORDED => captured.borrow_mut().take(),
        r if r == objc2_app_kit::NSAlertSecondButtonReturn => Some((String::new(), NSEventModifierFlags::Command)),
        _ => None,
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
