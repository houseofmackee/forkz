//! The app: one window with a sidebar, two file panes, and running jobs at
//! the bottom; the menu bar and toolbar; and folder watching.

mod activity;
mod column_view;
pub mod commands;
mod custom_tools;
mod dialogs;
mod diff_window;
mod drop_stack;
mod duplicates_window;
mod file_viewer;
mod filters_editor;
mod icon_view;
pub mod intents;
mod multi_rename;
mod oplog;
mod palette;
mod pane_view;
mod permissions;
mod preview;
mod promises;
mod remote_edit;
mod search;
mod settings_window;
mod sidebar;
mod site_form;
mod site_transfer;
mod sounds;
mod sync_window;
mod theme_editor;
mod themed;
mod tools;
mod tools_editor;
mod usage_window;
mod util;
mod viewer;
mod workspaces;

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use dispatch2::{DispatchQueue, DispatchTime};
use notify::{RecursiveMode, Watcher};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBackingStoreType, NSControlStateValueOff,
    NSControlStateValueOn, NSMenu, NSMenuDelegate, NSMenuItem, NSMenuItemValidation, NSPasteboard,
    NSPasteboardTypeString, NSSplitView, NSSplitViewDividerStyle, NSStackView, NSToolbar, NSToolbarDelegate,
    NSToolbarDisplayMode, NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem, NSToolbarItemValidation,
    NSToolbarSpaceItemIdentifier, NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
    NSWindowToolbarStyle, NSWorkspace, NSWorkspaceDidMountNotification, NSWorkspaceDidUnmountNotification,
};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use self::activity::Activity;
use self::commands::Command;
use self::pane_view::PaneController;
use self::sidebar::Sidebar;
use self::util::{file_url, mtm, ns, symbol};
use crate::fs::Sort;
use crate::fs::ops::{self, Job, JobKind, Outcome, Resolution};
use crate::pane::{PaneState, title_of};
use crate::settings::Settings;
use crate::sites::SiteStore;
use crate::vfs::{self, FsRef};

struct App {
    window: Retained<NSWindow>,
    panes: [Retained<PaneController>; 2],
    active: Cell<usize>,
    sidebar: Retained<Sidebar>,
    activity: Retained<Activity>,
    preview: Retained<preview::PreviewPane>,
    _delegate: Retained<AppDelegate>,
    settings: RefCell<Settings>,
    watcher: RefCell<Option<notify::RecommendedWatcher>>,
    watched: RefCell<Vec<PathBuf>>,
    /// A reload is scheduled for the pane (changes come in bursts).
    reload_pending: [Cell<bool>; 2],
    sites: RefCell<SiteStore>,
    /// Open connections, by the id of the site they were opened from.
    connections: RefCell<Vec<(String, FsRef)>>,
}

thread_local! {
    static APP: OnceCell<Rc<App>> = const { OnceCell::new() };
}

fn app() -> Option<Rc<App>> {
    APP.with(|a| a.get().cloned())
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzAppDelegate"]
    pub struct AppDelegate;

    impl AppDelegate {
        #[unsafe(method(command:))]
        fn command(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            if let Some(command) = Command::from_tag(tag) {
                run_command(command, None);
            }
        }

        #[unsafe(method(checkForUpdates:))]
        fn check_for_updates(&self, _sender: Option<&AnyObject>) {
            crate::updates::check(self.mtm());
        }

        #[unsafe(method(toggleAutomaticUpdates:))]
        fn toggle_automatic_updates(&self, _sender: Option<&AnyObject>) {
            crate::updates::set_checks_automatically(!crate::updates::checks_automatically());
        }

        /// View > Columns: for the active pane's kind (Mac or server).
        #[unsafe(method(toggleColumn:))]
        fn toggle_column_menu(&self, sender: &NSMenuItem) {
            let Some(app) = app() else { return };
            let local = app.panes[app.active.get()].is_local();
            if let Some((id, _)) = crate::settings::COLUMNS.get(sender.tag() as usize) {
                toggle_column(local, id);
            }
        }

        /// Window > Workspaces.
        #[unsafe(method(openWorkspace:))]
        fn open_workspace(&self, sender: &NSMenuItem) {
            workspaces::open((sender.tag() - WORKSPACE_TAG) as usize);
        }

        #[unsafe(method(deleteWorkspace:))]
        fn delete_workspace(&self, sender: &NSMenuItem) {
            workspaces::delete((sender.tag() - WORKSPACE_TAG) as usize);
        }

        /// Go > Recent Folders.
        #[unsafe(method(openRecentFolder:))]
        fn open_recent_folder(&self, sender: &NSMenuItem) {
            open_recent((sender.tag() - RECENT_TAG) as usize);
        }

        /// Commands > Tools.
        #[unsafe(method(runTool:))]
        fn run_tool_menu(&self, sender: &NSMenuItem) {
            run_tool((sender.tag() - TOOL_TAG) as usize);
        }

        /// A tool on the toolbar (found by its name in the identifier).
        #[unsafe(method(runToolItem:))]
        fn run_tool_item(&self, sender: &NSToolbarItem) {
            let id = sender.itemIdentifier().to_string();
            let name = id.strip_prefix(TOOL_ITEM_PREFIX).unwrap_or_default().to_string();
            let index = current_settings().and_then(|s| s.tools.iter().position(|t| t.name == name));
            match index {
                Some(index) => run_tool(index),
                None => dialogs::show_error(format!("There is no tool named \u{201c}{name}\u{201d} any more."), "Remove it from the toolbar with View > Customize Toolbar.".into()),
            }
        }

        #[unsafe(method(editTools:))]
        fn edit_tools(&self, _sender: Option<&AnyObject>) {
            settings_window::show_tab("Tools");
        }

        #[unsafe(method(clearRecentFolders:))]
        fn clear_recent_folders(&self, _sender: Option<&AnyObject>) {
            update_settings(|s| s.recent_folders.clear());
        }

        /// Commands > Simultaneous Transfers.
        #[unsafe(method(transferLimit:))]
        fn transfer_limit(&self, sender: &NSMenuItem) {
            set_transfer_limit((sender.tag() - TRANSFER_LIMIT_TAG) as usize);
        }

        #[unsafe(method(volumesChanged:))]
        fn volumes_changed(&self, _notification: &NSNotification) {
            reload_sidebar();
        }

        /// "Show in Finder" in another app, when forkz is the folder viewer.
        #[unsafe(method(handleReveal:withReplyEvent:))]
        fn handle_reveal(&self, event: &objc2_foundation::NSAppleEventDescriptor, _reply: &AnyObject) {
            show_paths(&file_viewer::paths_of(event), true);
        }
    }

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }

        /// Folders opened with forkz (it can be the default folder app),
        /// and files: shown in their folder.
        #[unsafe(method(application:openURLs:))]
        fn open_urls(&self, _app: &NSApplication, urls: &NSArray<objc2_foundation::NSURL>) {
            // forkz:// links (from Shortcuts, scripts, and web pages).
            let (links, files): (Vec<_>, Vec<_>) =
                urls.iter().partition(|u| u.scheme().is_some_and(|s| s.to_string().eq_ignore_ascii_case("forkz")));
            for link in links {
                let text = link.absoluteString().map(|s| s.to_string()).unwrap_or_default();
                // Once the window is up (a link can start forkz).
                DispatchQueue::main().exec_async(move || open_link(&text));
            }
            let paths: Vec<PathBuf> = files.iter().filter_map(|u| util::url_path(u)).collect();
            if !paths.is_empty() {
                show_paths(&paths, false);
            }
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            if let Some(app) = app() {
                app.settings.borrow().save();
            }
        }
    }

    /// The Recent Folders menu is filled when it opens.
    unsafe impl NSMenuDelegate for AppDelegate {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            if menu.title().to_string() == "Tools" {
                fill_tools_menu(menu, self);
            } else {
                fill_recent_menu(menu, self);
            }
        }
    }

    unsafe impl NSMenuItemValidation for AppDelegate {
        #[unsafe(method(validateMenuItem:))]
        fn validate_menu_item(&self, item: &NSMenuItem) -> bool {
            validate_menu_item(item)
        }
    }

    unsafe impl NSToolbarItemValidation for AppDelegate {
        #[unsafe(method(validateToolbarItem:))]
        fn validate_toolbar_item(&self, item: &NSToolbarItem) -> bool {
            validate_toolbar_item(item)
        }
    }

    unsafe impl NSToolbarDelegate for AppDelegate {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn toolbar_item(&self, _toolbar: &NSToolbar, identifier: &NSString, _insert: bool) -> Option<Retained<NSToolbarItem>> {
            self.toolbar_item_for(identifier)
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn default_identifiers(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            toolbar_identifiers(true)
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn allowed_identifiers(&self, _toolbar: &NSToolbar) -> Retained<NSArray<NSString>> {
            toolbar_identifiers(false)
        }
    }
);

impl AppDelegate {
    fn toolbar_item_for(&self, identifier: &NSString) -> Option<Retained<NSToolbarItem>> {
        if let Some(name) = identifier.to_string().strip_prefix(TOOL_ITEM_PREFIX) {
            let tool = current_settings().and_then(|s| s.tools.into_iter().find(|t| t.name == name));
            let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(self.mtm()), identifier);
            item.setLabel(&ns(name));
            item.setPaletteLabel(&ns(name));
            item.setToolTip(Some(&ns(&format!("Run \u{201c}{name}\u{201d}"))));
            let symbol_name = tool.map_or_else(|| "hammer".to_string(), |t| t.symbol);
            if let Some(image) = symbol(&symbol_name, name).or_else(|| symbol("hammer", name)) {
                item.setImage(Some(&image));
            }
            item.setBordered(true);
            unsafe {
                item.setTarget(Some(self));
                item.setAction(Some(sel!(runToolItem:)));
            }
            return Some(item);
        }
        let command = TOOLBAR.iter().copied().find(|c| toolbar_id(*c) == identifier.to_string())?;
        let item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(self.mtm()), identifier);
        item.setLabel(&ns(command.toolbar_label()));
        item.setPaletteLabel(&ns(command.toolbar_label()));
        item.setToolTip(Some(&ns(command.title())));
        if let Some(image) = symbol(command.symbol(), command.title()) {
            item.setImage(Some(&image));
        }
        item.setTag(command.tag());
        item.setBordered(true);
        unsafe {
            item.setTarget(Some(self));
            item.setAction(Some(sel!(command:)));
        }
        Some(item)
    }
}

/// The optional columns shown in panes on the Mac or on servers.
pub fn visible_columns(local: bool) -> Vec<String> {
    app().map_or_else(
        || crate::settings::ColumnSettings::default().visible(local).to_vec(),
        |app| app.settings.borrow().columns.visible(local).to_vec(),
    )
}

/// Shows or hides a column in every pane of that kind (Mac or server).
pub fn toggle_column(local: bool, id: &str) {
    let Some(app) = app() else { return };
    {
        let mut settings = app.settings.borrow_mut();
        settings.columns.toggle(local, id);
        settings.save();
    }
    for pane in &app.panes {
        if pane.is_local() == local {
            pane.apply_columns();
        }
    }
}

/// Tags of the workspace menu items: this plus the workspace's index.
const WORKSPACE_TAG: isize = 3000;
const RECENT_TAG: isize = 4000;
const TOOL_TAG: isize = 5000;
/// Toolbar items for tools: this and the tool's name.
const TOOL_ITEM_PREFIX: &str = "forkz.tool.";

/// Tags of the Simultaneous Transfers items: this plus the number.
const TRANSFER_LIMIT_TAG: isize = 2000;

fn set_transfer_limit(limit: usize) {
    let Some(app) = app() else { return };
    let limit = limit.max(1);
    app.activity.set_limit(limit);
    let mut settings = app.settings.borrow_mut();
    settings.transfers.max_transfers = limit;
    settings.save();
}

fn validate_menu_item(item: &NSMenuItem) -> bool {
    if item.action() == Some(sel!(checkForUpdates:)) {
        return crate::updates::available();
    }
    if item.action() == Some(sel!(toggleAutomaticUpdates:)) {
        let on = crate::updates::checks_automatically();
        item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        return crate::updates::available();
    }
    if item.action() == Some(sel!(toggleColumn:)) {
        let Some(app) = app() else { return false };
        let local = app.panes[app.active.get()].is_local();
        let id = crate::settings::COLUMNS.get(item.tag() as usize).map_or("", |c| c.0);
        let on = app.settings.borrow().columns.visible(local).iter().any(|v| v == id);
        item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        return true;
    }
    if item.action() == Some(sel!(transferLimit:)) {
        let current = app().map_or(0, |a| a.activity.limit()) as isize;
        let on = item.tag() - TRANSFER_LIMIT_TAG == current;
        item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        return true;
    }
    let Some(command) = Command::from_tag(item.tag()) else { return true };
    let Some(app) = app() else { return false };
    let active = app.active.get();
    prepare_item(item, command, active);
    is_enabled(command)
}

fn validate_toolbar_item(item: &NSToolbarItem) -> bool {
    let Some(command) = Command::from_tag(item.tag()) else { return true };
    // The hidden files button shows the state: an eye when they show.
    if command == Command::ToggleHidden
        && let Some(app) = app()
    {
        let shown = app.settings.borrow().show_hidden;
        let (name, tip) =
            if shown { ("eye", "Hidden files are shown") } else { ("eye.slash", "Hidden files are hidden") };
        if let Some(image) = symbol(name, tip) {
            item.setImage(Some(&image));
        }
    }
    if let Some(app) = app() {
        item.setToolTip(Some(&ns(&command_title(command, app.active.get()))));
    }
    is_enabled(command)
}

/// Title and check mark of a menu item for a pane (the wording changes
/// between the Mac and servers).
pub fn prepare_item(item: &NSMenuItem, command: Command, pane: usize) {
    item.setTitle(&ns(&command_title(command, pane)));
    if command == Command::ToggleHidden {
        let shown = app().is_some_and(|a| a.settings.borrow().show_hidden);
        item.setState(if shown { NSControlStateValueOn } else { NSControlStateValueOff });
    }
    if command == Command::SyncBrowsing {
        let on = SYNC_BROWSING.with(|s| s.borrow().is_some());
        item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    }
    if let Some(group) = command.grouping() {
        let on = app().is_some_and(|a| a.panes[pane].group_by() == group);
        item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    }
    if command == Command::ToggleFilters {
        let on = app().is_some_and(|a| a.settings.borrow().filters_on);
        item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    }
    if let Some(mode) = command.view_mode() {
        let on = app().is_some_and(|a| a.panes[pane].view_mode() == mode);
        item.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
    }
}

/// Whether a file system is a server (not the Mac or an archive).
fn on_server(fs: &FsRef) -> bool {
    !fs.is_local() && fs.outside().is_none()
}

/// A command's title for a pane: "Delete..." instead of "Move to Trash" on
/// a server, "Upload" or "Download" when copying between the Mac and a
/// server, and so on.
pub fn command_title(command: Command, pane: usize) -> String {
    let Some(app) = app() else { return command.title().into() };
    // On the Mac, or in an archive (not a server).
    let here = !on_server(&app.panes[pane].fs());
    let there = !on_server(&app.panes[1 - pane].fs());
    let title = match command {
        Command::Trash if !app.panes[pane].is_local() => "Delete\u{2026}",
        Command::CopyToOther if app.panes[pane].fs().read_only() => "Extract to Other Pane",
        Command::CopyToOther if here && !there => "Upload to Other Pane",
        Command::CopyToOther if !here && there => "Download to Other Pane",
        Command::PasteFiles if !here => "Upload Copied Items",
        Command::OpenInTerminal if !here => "Open SSH Session in Terminal",
        Command::CopyPath if !here => "Copy Server Link",
        Command::CloseTab if app.panes[pane].tab_count() <= 1 => "Close Window",
        Command::TogglePreview if !app.preview.view().isHidden() => "Hide Preview Pane",
        Command::ShowDropStack if drop_stack::is_shown() => "Hide Drop Stack",
        Command::Disconnect if app.panes[pane].fs().outside().is_some() => "Close Archive",
        Command::ToggleLeftPane | Command::ToggleRightPane if app.settings.borrow().stacked_panes => {
            let hidden = app.panes[usize::from(command == Command::ToggleRightPane)].view().isHidden();
            match (command == Command::ToggleLeftPane, hidden) {
                (true, true) => "Show Top Pane",
                (true, false) => "Hide Top Pane",
                (false, true) => "Show Bottom Pane",
                (false, false) => "Hide Bottom Pane",
            }
        }
        Command::ToggleLeftPane if app.panes[0].view().isHidden() => "Show Left Pane",
        Command::ToggleSidebar if app.sidebar.view().isHidden() => "Show Sidebar",
        Command::ExpandPreview if PREVIEW_EXPANDED.with(|e| e.get().is_some()) => "Shrink Preview Pane",
        Command::StackPanes if app.settings.borrow().stacked_panes => "Panes Side by Side",
        Command::ToggleRightPane if app.panes[1].view().isHidden() => "Show Right Pane",
        _ => command.title(),
    };
    title.to_string()
}

/// The toolbar's buttons, in their default order (None is a flexible space).
const TOOLBAR: &[Command] = &[
    Command::Connect,
    Command::Back,
    Command::Forward,
    Command::CopyToOther,
    Command::MoveToOther,
    Command::Trash,
    Command::NewFolder,
    Command::QuickLook,
    Command::ToggleHidden,
    Command::OpenInTerminal,
    Command::SwapPanes,
    Command::Refresh,
    Command::ToggleLeftPane,
    Command::ToggleRightPane,
    Command::TogglePreview,
    Command::ToggleSidebar,
    Command::ShowDropStack,
];

/// Toolbar items added in later versions, by `toolbar_version`, with
/// where they go (None: at the end).
/// A toolbar item and where it goes (None: at the end).
type NewItem = (Command, Option<isize>);

const NEW_TOOLBAR_ITEMS: &[(u32, &[NewItem])] = &[
    (1, &[(Command::ToggleLeftPane, None), (Command::ToggleRightPane, None), (Command::TogglePreview, None)]),
    (2, &[(Command::ToggleSidebar, Some(0))]),
];

fn toolbar_id(command: Command) -> String {
    format!("forkz.{command:?}")
}

fn toolbar_identifiers(default: bool) -> Retained<NSArray<NSString>> {
    let flexible = unsafe { NSToolbarFlexibleSpaceItemIdentifier }.retain();
    let space = unsafe { NSToolbarSpaceItemIdentifier }.retain();
    let id = |c: Command| ns(&toolbar_id(c));
    let ids: Vec<Retained<NSString>> = if default {
        vec![
            id(Command::ToggleSidebar),
            id(Command::Connect),
            space.clone(),
            id(Command::Back),
            id(Command::Forward),
            space.clone(),
            id(Command::CopyToOther),
            id(Command::MoveToOther),
            id(Command::Trash),
            id(Command::NewFolder),
            flexible.clone(),
            id(Command::QuickLook),
            id(Command::ToggleHidden),
            id(Command::OpenInTerminal),
            id(Command::SwapPanes),
            space.clone(),
            id(Command::ToggleLeftPane),
            id(Command::ToggleRightPane),
            id(Command::TogglePreview),
        ]
    } else {
        let mut ids: Vec<_> = TOOLBAR.iter().map(|c| id(*c)).collect();
        // Your tools (Settings > Tools).
        let tools = current_settings().map(|s| s.tools).unwrap_or_default();
        ids.extend(tools.iter().map(|t| ns(&format!("{TOOL_ITEM_PREFIX}{}", t.name))));
        ids.push(flexible);
        ids.push(space);
        ids
    };
    NSArray::from_retained_slice(&ids)
}

/// Starts the app; returns when it quits. `open` is a folder or an
/// sftp:// address to show first.
pub fn run(open: Option<String>) -> anyhow::Result<()> {
    let mtm = mtm();
    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    let delegate: Retained<AppDelegate> = unsafe { msg_send![AppDelegate::alloc(mtm), init] };
    ns_app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    file_viewer::install(&delegate);
    // No macOS window tabs (forkz has tabs in each pane): this also
    // drops Show Tab Bar and Show All Tabs from the View menu.
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
    let settings = Settings::load();
    let names: Vec<String> = settings.workspaces.iter().map(|w| w.name.clone()).collect();
    ns_app.setMainMenu(Some(&main_menu(mtm, &delegate, &settings.shortcuts, &names)));
    let panes = [
        PaneController::new(
            mtm,
            0,
            PaneState::new(vfs::local(), settings.left.location.clone(), settings.left.sort, settings.show_hidden),
        ),
        PaneController::new(
            mtm,
            1,
            PaneState::new(vfs::local(), settings.right.location.clone(), settings.right.sort, settings.show_hidden),
        ),
    ];
    let sidebar = Sidebar::new(mtm);
    let activity = Activity::new(mtm);
    preview::clean_cache();
    let preview = preview::PreviewPane::new(mtm);
    preview.view().setHidden(!settings.preview.visible);
    preview.select_tab_named(&settings.preview.tab);
    let window = build_window(mtm, &delegate, &panes, &sidebar, &activity, &preview);

    unsafe {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        for name in [NSWorkspaceDidMountNotification, NSWorkspaceDidUnmountNotification] {
            center.addObserver_selector_name_object(&delegate, sel!(volumesChanged:), Some(name), None);
        }
    }

    let app = Rc::new(App {
        window,
        panes,
        active: Cell::new(0),
        sidebar,
        activity,
        preview,
        _delegate: delegate,
        settings: RefCell::new(settings),
        watcher: RefCell::new(None),
        watched: RefCell::new(Vec::new()),
        reload_pending: [Cell::new(false), Cell::new(false)],
        sites: RefCell::new(SiteStore::load()),
        connections: RefCell::new(Vec::new()),
    });
    app.activity.set_limit(app.settings.borrow().transfers.max_transfers);
    restore_tabs(&app);
    apply_filters(&app);
    {
        let (left, right) = {
            let s = app.settings.borrow();
            ((s.left.group_by, s.left.view), (s.right.group_by, s.right.view))
        };
        for (pane, (group, view)) in app.panes.iter().zip([left, right]) {
            pane.set_group_by(group);
            pane.set_view_mode(view);
        }
    }
    apply_appearance(&app);
    apply_speed_limits(&app);
    APP.with(|a| a.set(app.clone())).map_err(|_| anyhow::anyhow!("the app is already running"))?;
    // The columns from settings (the panes were made before the app was
    // reachable, with the default columns).
    for pane in &app.panes {
        pane.apply_columns();
    }

    start_watching();
    reload_sidebar();
    crate::updates::start(mtm);
    app.window.makeKeyAndOrderFront(None);
    first_launch_layout(&app.window);
    if app.settings.borrow().stacked_panes {
        set_panes_stacked(&app, true);
    }
    add_new_toolbar_items(&app);
    // The split view restores its own idea of what is hidden: settings win.
    let (preview_visible, sidebar_hidden) = {
        let s = app.settings.borrow();
        (s.preview.visible, s.sidebar_hidden)
    };
    app.preview.view().setHidden(!preview_visible);
    app.sidebar.view().setHidden(sidebar_hidden);
    if let Some(split) = app.window.contentView().and_then(|v| v.downcast::<NSSplitView>().ok()) {
        split.adjustSubviews();
    }
    for pane in &app.panes {
        pane.view().setHidden(false);
    }
    let hidden = {
        let s = app.settings.borrow();
        [s.left.hidden, s.right.hidden && !s.left.hidden]
    };
    for (index, hide) in hidden.into_iter().enumerate() {
        if hide {
            set_pane_hidden(&app, index, true);
        }
    }
    // Now that the panes have their size, fit the columns to them.
    for pane in &app.panes {
        pane.fit_columns();
    }
    app.panes[0].focus();
    set_active(0);
    if let Some(target) = open {
        open_target(&target);
    }
    // Shortcuts actions that started the app.
    DispatchQueue::main().exec_async(intents::follow_waiting);
    watch_mouse_buttons();
    ns_app.activate();
    ns_app.run();
    Ok(())
}

fn build_window(
    mtm: MainThreadMarker,
    delegate: &AppDelegate,
    panes: &[Retained<PaneController>; 2],
    sidebar: &Sidebar,
    activity: &Activity,
    preview: &preview::PreviewPane,
) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let frame = NSRect::new(NSPoint::new(120.0, 120.0), NSSize::new(1240.0, 760.0));
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&ns("forkz"));
    window.setMinSize(NSSize::new(720.0, 420.0));
    window.setToolbarStyle(NSWindowToolbarStyle::Unified);

    let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &ns("ForkzMainToolbar"));
    toolbar.setDelegate(Some(ProtocolObject::from_ref(delegate)));
    toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
    toolbar.setAllowsUserCustomization(true);
    toolbar.setAutosavesConfiguration(true);
    window.setToolbar(Some(&toolbar));

    let panes_split = NSSplitView::new(mtm);
    panes_split.setVertical(true);
    panes_split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    panes_split.addSubview(&panes[0].view());
    panes_split.addSubview(&panes[1].view());
    panes_split.setAutosaveName(Some(&ns("ForkzPanes")));
    panes_split.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Vertical);

    let right = NSStackView::new(mtm);
    right.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    right.setSpacing(0.0);
    right.addArrangedSubview(&panes_split);
    right.addArrangedSubview(&activity.view());
    for view in [&*panes_split as &NSView, &activity.view()] {
        view.leadingAnchor().constraintEqualToAnchor(&right.leadingAnchor()).setActive(true);
        view.trailingAnchor().constraintEqualToAnchor(&right.trailingAnchor()).setActive(true);
    }

    let main_split = NSSplitView::new(mtm);
    main_split.setVertical(true);
    main_split.setDividerStyle(NSSplitViewDividerStyle::Thin);
    let sidebar_view = sidebar.view();
    main_split.addSubview(&sidebar_view);
    main_split.addSubview(&right);
    main_split.addSubview(&preview.view());
    sidebar_view.widthAnchor().constraintGreaterThanOrEqualToConstant(130.0).setActive(true);
    sidebar_view.widthAnchor().constraintLessThanOrEqualToConstant(320.0).setActive(true);
    // At most half the window (turned off while the preview fills it).
    let widest = preview
        .view()
        .widthAnchor()
        .constraintLessThanOrEqualToAnchor_multiplier(&main_split.widthAnchor(), PREVIEW_MAX_SHARE);
    widest.setIdentifier(Some(&ns(PREVIEW_MAX_WIDTH)));
    widest.setActive(true);
    // Resizing the window resizes the panes, not the sidebar or preview.
    main_split.setHoldingPriority_forSubviewAtIndex(260.0, 0);
    main_split.setHoldingPriority_forSubviewAtIndex(250.0, 1);
    main_split.setHoldingPriority_forSubviewAtIndex(259.0, 2);
    main_split.setAutosaveName(Some(&ns("ForkzMain")));

    window.setContentView(Some(&main_split));
    window.setFrameAutosaveName(&ns("ForkzMainWindow"));
    window
}

/// On first launch: a sidebar of 180 points and two equal panes. (Later
/// launches restore the saved divider positions.)
fn first_launch_layout(window: &NSWindow) {
    let saved = objc2_foundation::NSUserDefaults::standardUserDefaults()
        .objectForKey(&ns("NSSplitView Subview Frames ForkzPanes"))
        .is_some();
    if saved {
        return;
    }
    let Some(main_split) = window.contentView().and_then(|v| v.downcast::<NSSplitView>().ok()) else { return };
    main_split.layoutSubtreeIfNeeded();
    main_split.setPosition_ofDividerAtIndex(180.0, 0);
    let subviews = main_split.subviews();
    let Some(right) = subviews.iter().nth(1) else { return };
    let Some(panes_split) = right.subviews().iter().find_map(|v| v.downcast::<NSSplitView>().ok()) else { return };
    panes_split.layoutSubtreeIfNeeded();
    let width = panes_split.frame().size.width;
    panes_split.setPosition_ofDividerAtIndex((width / 2.0).round(), 0);
}

/// Builds the menu bar again (after shortcuts change).
pub fn rebuild_main_menu() {
    let Some(app) = app() else { return };
    let mtm = mtm();
    let (overrides, names) = {
        let settings = app.settings.borrow();
        (settings.shortcuts.clone(), settings.workspaces.iter().map(|w| w.name.clone()).collect::<Vec<_>>())
    };
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main_menu(mtm, &app._delegate, &overrides, &names)));
}

fn main_menu(
    mtm: MainThreadMarker,
    delegate: &AppDelegate,
    overrides: &std::collections::BTreeMap<String, String>,
    workspaces_saved: &[String],
) -> Retained<NSMenu> {
    let bar = NSMenu::new(mtm);
    let submenu = |title: &str| {
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns(title));
        let item = NSMenuItem::new(mtm);
        item.setSubmenu(Some(&menu));
        bar.addItem(&item);
        menu
    };
    let standard = |menu: &NSMenu, title: &str, action: objc2::runtime::Sel, key: &str| {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), Some(action), &ns(key))
        };
        menu.addItem(&item);
        item
    };
    let command = |menu: &NSMenu, command: Command| {
        let (key, modifiers) = command.effective_shortcut(overrides);
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &ns(command.title()),
                Some(sel!(command:)),
                &ns(&key),
            )
        };
        item.setKeyEquivalentModifierMask(modifiers);
        item.setTag(command.tag());
        unsafe { item.setTarget(Some(delegate)) };
        menu.addItem(&item);
    };
    let separator = |menu: &NSMenu| menu.addItem(&NSMenuItem::separatorItem(mtm));

    let app_menu = submenu("forkz");
    standard(&app_menu, "About forkz", sel!(orderFrontStandardAboutPanel:), "");
    separator(&app_menu);
    command(&app_menu, Command::Settings);
    for (title, action) in [
        ("Check for Updates\u{2026}", sel!(checkForUpdates:)),
        ("Check for Updates Automatically", sel!(toggleAutomaticUpdates:)),
    ] {
        let item = standard(&app_menu, title, action, "");
        unsafe { item.setTarget(Some(delegate)) };
    }
    separator(&app_menu);
    standard(&app_menu, "Hide forkz", sel!(hide:), "h");
    let others = standard(&app_menu, "Hide Others", sel!(hideOtherApplications:), "h");
    others.setKeyEquivalentModifierMask(
        objc2_app_kit::NSEventModifierFlags::Command | objc2_app_kit::NSEventModifierFlags::Option,
    );
    standard(&app_menu, "Show All", sel!(unhideAllApplications:), "");
    separator(&app_menu);
    standard(&app_menu, "Quit forkz", sel!(terminate:), "q");

    let file = submenu("File");
    command(&file, Command::NewTab);
    command(&file, Command::OpenInNewTab);
    separator(&file);
    command(&file, Command::Connect);
    command(&file, Command::Disconnect);
    for c in [
        Command::ImportFileZilla,
        Command::ImportCyberduck,
        Command::ImportWinScp,
        Command::ImportSitesFile,
        Command::ExportSites,
    ] {
        command(&file, c);
    }
    separator(&file);
    for c in [Command::Open, Command::OpenWithApp, Command::QuickLook, Command::ViewFile] {
        command(&file, c);
    }
    separator(&file);
    for c in [
        Command::NewFolder,
        Command::NewFolderWithSelection,
        Command::NewFile,
        Command::Rename,
        Command::MultiRename,
        Command::Duplicate,
        Command::Compress,
    ] {
        command(&file, c);
    }
    separator(&file);
    command(&file, Command::Trash);
    command(&file, Command::DeleteImmediately);
    command(&file, Command::Permissions);
    command(&file, Command::EditTags);
    separator(&file);
    for c in [Command::CompareFiles, Command::CalculateSizes, Command::ExportTree] {
        command(&file, c);
    }
    let checksums = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Copy Checksum"));
    for c in [Command::CopyMd5, Command::CopySha1, Command::CopySha256, Command::CopySha384, Command::CopySha512] {
        command(&checksums, c);
    }
    let checksums_item = NSMenuItem::new(mtm);
    checksums_item.setTitle(&ns("Copy Checksum"));
    checksums_item.setSubmenu(Some(&checksums));
    file.addItem(&checksums_item);
    separator(&file);
    for c in [Command::RevealInFinder, Command::OpenInTerminal, Command::Share, Command::AddToSidebar] {
        command(&file, c);
    }
    separator(&file);
    command(&file, Command::CloseTab);
    command(&file, Command::ReopenClosedTab);

    let edit = submenu("Edit");
    standard(&edit, "Cut", sel!(cut:), "x");
    standard(&edit, "Copy", sel!(copy:), "c");
    standard(&edit, "Paste", sel!(paste:), "v");
    command(&edit, Command::MoveFilesHere);
    command(&edit, Command::CopyPath);
    standard(&edit, "Select All", sel!(selectAll:), "a");
    command(&edit, Command::SelectMatching);
    command(&edit, Command::SelectSameType);
    separator(&edit);
    command(&edit, Command::Filter);
    command(&edit, Command::Search);

    let view = submenu("View");
    for c in [Command::ViewAsList, Command::ViewAsIcons, Command::ViewAsColumns] {
        command(&view, c);
    }
    separator(&view);
    for c in [Command::ToggleHidden, Command::ToggleFilters, Command::FlatView, Command::Refresh] {
        command(&view, c);
    }
    separator(&view);
    for c in [
        Command::ToggleSidebar,
        Command::StackPanes,
        Command::ToggleLeftPane,
        Command::ToggleRightPane,
        Command::TogglePreview,
        Command::ExpandPreview,
    ] {
        command(&view, c);
    }
    separator(&view);
    for c in [Command::ZoomIn, Command::ZoomOut, Command::ActualSize] {
        command(&view, c);
    }
    separator(&view);
    let group = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Group By"));
    for c in [Command::GroupNone, Command::GroupKind, Command::GroupModified, Command::GroupSize] {
        command(&group, c);
    }
    let group_item = NSMenuItem::new(mtm);
    group_item.setTitle(&ns("Group By"));
    group_item.setSubmenu(Some(&group));
    view.addItem(&group_item);
    let columns = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Columns"));
    for (index, (_, title)) in crate::settings::COLUMNS.iter().enumerate() {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &ns(title),
                Some(sel!(toggleColumn:)),
                &ns(""),
            )
        };
        item.setTag(index as isize);
        unsafe { item.setTarget(Some(delegate)) };
        columns.addItem(&item);
    }
    let columns_item = NSMenuItem::new(mtm);
    columns_item.setTitle(&ns("Columns"));
    columns_item.setSubmenu(Some(&columns));
    view.addItem(&columns_item);
    separator(&view);
    standard(&view, "Customize Toolbar\u{2026}", sel!(runToolbarCustomizationPalette:), "");
    let full_screen = standard(&view, "Enter Full Screen", sel!(toggleFullScreen:), "f");
    full_screen.setKeyEquivalentModifierMask(
        objc2_app_kit::NSEventModifierFlags::Command | objc2_app_kit::NSEventModifierFlags::Control,
    );

    let go = submenu("Go");
    for c in [Command::Back, Command::Forward, Command::Up] {
        command(&go, c);
    }
    separator(&go);
    for c in [Command::Home, Command::Desktop, Command::Documents, Command::Downloads, Command::Applications] {
        command(&go, c);
    }
    separator(&go);
    let recent = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Recent Folders"));
    recent.setDelegate(Some(ProtocolObject::from_ref(delegate)));
    let recent_item = NSMenuItem::new(mtm);
    recent_item.setTitle(&ns("Recent Folders"));
    recent_item.setSubmenu(Some(&recent));
    go.addItem(&recent_item);
    command(&go, Command::GoToFolder);

    let commands = submenu("Commands");
    command(&commands, Command::CommandPalette);
    separator(&commands);
    for c in [Command::CopyToOther, Command::MoveToOther] {
        command(&commands, c);
    }
    separator(&commands);
    for c in [Command::SwapPanes, Command::SameFolderInOther] {
        command(&commands, c);
    }
    separator(&commands);
    for c in [
        Command::ComparePanes,
        Command::FindDuplicates,
        Command::DiskUsage,
        Command::SyncFolders,
        Command::SyncBrowsing,
    ] {
        command(&commands, c);
    }
    separator(&commands);
    let tools = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Tools"));
    tools.setDelegate(Some(ProtocolObject::from_ref(delegate)));
    let tools_item = NSMenuItem::new(mtm);
    tools_item.setTitle(&ns("Tools"));
    tools_item.setSubmenu(Some(&tools));
    commands.addItem(&tools_item);
    separator(&commands);
    let limits = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Simultaneous Transfers"));
    for n in [1, 2, 3, 4, 5, 6, 8, 10] {
        let title = if n == 1 { "1 (one after another)".to_string() } else { n.to_string() };
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &ns(&title),
                Some(sel!(transferLimit:)),
                &ns(""),
            )
        };
        item.setTag(TRANSFER_LIMIT_TAG + n);
        unsafe { item.setTarget(Some(delegate)) };
        limits.addItem(&item);
    }
    let limits_item = NSMenuItem::new(mtm);
    limits_item.setTitle(&ns("Simultaneous Transfers"));
    limits_item.setSubmenu(Some(&limits));
    commands.addItem(&limits_item);

    let window = submenu("Window");
    standard(&window, "Minimize", sel!(performMiniaturize:), "m");
    standard(&window, "Zoom", sel!(performZoom:), "");
    separator(&window);
    command(&window, Command::NextTab);
    command(&window, Command::PreviousTab);
    command(&window, Command::MoveTabToOther);
    separator(&window);
    command(&window, Command::ShowDropStack);
    separator(&window);
    let workspace_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Workspaces"));
    command(&workspace_menu, Command::SaveWorkspace);
    if !workspaces_saved.is_empty() {
        workspace_menu.addItem(&NSMenuItem::separatorItem(mtm));
        let delete_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Delete Workspace"));
        for (index, name) in workspaces_saved.iter().enumerate() {
            let key = if index < 9 { (index + 1).to_string() } else { String::new() };
            for (menu, action, key) in
                [(&workspace_menu, sel!(openWorkspace:), key.as_str()), (&delete_menu, sel!(deleteWorkspace:), "")]
            {
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(mtm),
                        &ns(name),
                        Some(action),
                        &ns(key),
                    )
                };
                item.setKeyEquivalentModifierMask(
                    objc2_app_kit::NSEventModifierFlags::Command | objc2_app_kit::NSEventModifierFlags::Control,
                );
                item.setTag(WORKSPACE_TAG + index as isize);
                unsafe { item.setTarget(Some(delegate)) };
                menu.addItem(&item);
            }
        }
        workspace_menu.addItem(&NSMenuItem::separatorItem(mtm));
        let delete_item = NSMenuItem::new(mtm);
        delete_item.setTitle(&ns("Delete Workspace"));
        delete_item.setSubmenu(Some(&delete_menu));
        workspace_menu.addItem(&delete_item);
    }
    let workspace_item = NSMenuItem::new(mtm);
    workspace_item.setTitle(&ns("Workspaces"));
    workspace_item.setSubmenu(Some(&workspace_menu));
    window.addItem(&workspace_item);
    NSApplication::sharedApplication(mtm).setWindowsMenu(Some(&window));
    bar
}

fn is_enabled(command: Command) -> bool {
    let Some(app) = app() else { return false };
    let active = app.active.get();
    let has_selection = !app.panes[active].selected_paths().is_empty();
    is_enabled_in(command, active, has_selection)
}

/// Whether a command can run on a pane; `has_selection` tells if there are
/// items to act on (the selection, or a right-clicked row).
pub fn is_enabled_in(command: Command, index: usize, has_selection: bool) -> bool {
    let Some(app) = app() else { return false };
    let pane = &app.panes[index];
    let local = pane.is_local();
    match command {
        // Opening ".." goes up.
        Command::Open => has_selection || pane.parent_row_chosen(),
        Command::Back => pane.can_go_back(),
        Command::Forward => pane.can_go_forward(),
        Command::Up => pane.can_go_up(),
        // Archives cannot be changed: items in them can only be copied out.
        Command::Rename | Command::MultiRename | Command::Duplicate | Command::Trash | Command::NewFolder
            if pane.fs().read_only() =>
        {
            false
        }
        Command::MoveToOther if pane.fs().read_only() => false,
        Command::CopyToOther | Command::MoveToOther if app.panes[1 - index].fs().read_only() => false,
        Command::PasteFiles | Command::MoveFilesHere if pane.fs().read_only() => false,
        Command::Rename
        | Command::MultiRename
        | Command::Duplicate
        | Command::Trash
        | Command::CopyToOther
        | Command::MoveToOther => has_selection,
        // These need files on the Mac.
        Command::CopyFiles | Command::QuickLook => has_selection && local,
        Command::ViewFile => pane.target_entries().iter().any(|e| !e.is_folder_like()),
        Command::RevealInFinder => local,
        Command::AddToSidebar => local || site_of(&pane.fs()).is_some(),
        Command::RemoveFromSidebar => {
            let settings = app.settings.borrow();
            if local {
                settings.favorites.contains(&pane.location())
            } else {
                let site = site_of(&pane.fs());
                settings.remote_favorites.iter().any(|f| Some(&f.site_id) == site.as_ref() && f.path == pane.location())
            }
        }
        Command::CopySha256 | Command::CopyMd5 | Command::CopySha1 | Command::CopySha384 | Command::CopySha512 => {
            has_selection
        }
        Command::SelectSameType => has_selection,
        Command::MoveTabToOther => pane.tab_count() > 1,
        Command::ReopenClosedTab => CLOSED_TABS.with(|c| !c.borrow().is_empty()),
        Command::ExportSites => !app.sites.borrow().sites.is_empty(),
        Command::NextTab | Command::PreviousTab => pane.tab_count() > 1,
        Command::CompareFiles => {
            let here = pane.selected_paths().len();
            here == 2 || (here == 1 && app.panes[1 - index].selected_paths().len() == 1)
        }
        // Servers, and mounted shares (which count as local).
        Command::Disconnect => !local || pane.fs().key().starts_with("mount:"),
        Command::OpenWithApp => has_selection,
        Command::NewFile => !pane.fs().read_only() && !pane.showing_results(),
        Command::ComparePanes => !app.panes[0].showing_results() && !app.panes[1].showing_results(),
        // On the Mac (duplicates go to the Trash).
        Command::FindDuplicates => local && !pane.showing_results(),
        Command::NewFolderWithSelection => has_selection && !pane.fs().read_only() && !pane.showing_results(),
        Command::DeleteImmediately => has_selection && !pane.fs().read_only(),
        Command::Share => has_selection && local,
        Command::ZoomIn => {
            app.settings.borrow().appearance.text_size < *crate::settings::TEXT_SIZES.last().unwrap_or(&24.0)
        }
        Command::ZoomOut => app.settings.borrow().appearance.text_size > crate::settings::TEXT_SIZES[0],
        Command::ActualSize => (app.settings.borrow().appearance.text_size - 13.0).abs() > 0.01,
        Command::Compress => has_selection && !pane.fs().read_only(),
        Command::EditTags => has_selection && local,
        // While the preview fills the window, the panes stay as they are.
        Command::ToggleLeftPane | Command::ToggleRightPane | Command::ToggleSidebar
            if PREVIEW_EXPANDED.with(|e| e.get().is_some()) =>
        {
            false
        }
        // One pane always stays.
        Command::ToggleLeftPane => app.panes[0].view().isHidden() || !app.panes[1].view().isHidden(),
        Command::ToggleRightPane => app.panes[1].view().isHidden() || !app.panes[0].view().isHidden(),
        Command::SwitchPane => !app.panes[1 - index].view().isHidden(),
        // A shell needs SSH; FTP, WebDAV, and S3 have no permissions.
        Command::OpenInTerminal => local || pane.fs().key().starts_with("sftp://"),
        Command::Permissions => pane.fs().supports_permissions() && !pane.fs().read_only(),
        Command::PasteFiles | Command::MoveFilesHere => {
            !pane_view::paths_on_pasteboard(&NSPasteboard::generalPasteboard()).is_empty()
        }
        _ => true,
    }
}

/// Runs a command on a pane (the active one if None).
pub fn run_command(command: Command, pane: Option<usize>) {
    let Some(app) = app() else { return };
    let index = pane.unwrap_or(app.active.get());
    let pane = app.panes[index].clone();
    let other = app.panes[1 - index].clone();
    let targets = || pane.target_paths();
    match command {
        Command::Open => pane.open_selection(),
        Command::NewFolder => new_folder(&pane),
        Command::Rename => rename(&pane),
        Command::MultiRename => multi_rename(&pane),
        Command::TogglePreview => toggle_preview(),
        Command::OpenWithApp => open_with_app(&pane),
        Command::Compress => compress(&pane),
        Command::EditTags => edit_tags(&pane),
        Command::CommandPalette => command_palette(),
        Command::SelectMatching => select_matching(&pane),
        Command::Search => search::run(&pane),
        Command::SaveWorkspace => workspaces::save(),
        Command::ToggleSidebar => toggle_sidebar(),
        Command::NewFile => new_file(&pane),
        Command::NewFolderWithSelection => new_folder_with_selection(&pane),
        Command::DeleteImmediately => delete_immediately(&pane),
        Command::Share => share(&pane),
        Command::SyncFolders => sync_folders(None),
        Command::ComparePanes => compare_panes(&app),
        Command::FlatView => search::flat_view(&pane),
        Command::FindDuplicates => duplicates_window::open(pane.location()),
        Command::SyncBrowsing => toggle_sync_browsing(),
        Command::ExpandPreview => toggle_preview_expanded(),
        Command::ToggleFilters => update_settings(|s| s.filters_on = !s.filters_on),
        Command::StackPanes => {
            let stacked = !app.settings.borrow().stacked_panes;
            set_panes_stacked(&app, stacked);
            update_settings(|s| s.stacked_panes = stacked);
        }
        Command::ViewAsList | Command::ViewAsIcons | Command::ViewAsColumns => {
            if let Some(mode) = command.view_mode() {
                pane.set_view_mode(mode);
                update_settings(|s| {
                    let side = if index == 0 { &mut s.left } else { &mut s.right };
                    side.view = mode;
                });
            }
        }
        Command::GroupNone | Command::GroupKind | Command::GroupModified | Command::GroupSize => {
            if let Some(group) = command.grouping() {
                pane.set_group_by(group);
                update_settings(|s| {
                    let side = if index == 0 { &mut s.left } else { &mut s.right };
                    side.group_by = group;
                });
            }
        }
        Command::ZoomIn => zoom(1),
        Command::ZoomOut => zoom(-1),
        Command::ActualSize => zoom(0),
        Command::ToggleLeftPane => toggle_pane(0),
        Command::ToggleRightPane => toggle_pane(1),
        Command::Duplicate => {
            start_job(transfer_job(JobKind::Copy, pane.fs(), targets(), pane.fs(), pane.location()));
        }
        Command::Trash => delete(&pane, targets()),
        Command::CopyToOther => {
            start_job(transfer_job(JobKind::Copy, pane.fs(), targets(), other.fs(), other.location()));
        }
        Command::MoveToOther => {
            start_job(transfer_job(JobKind::Move, pane.fs(), targets(), other.fs(), other.location()));
        }
        Command::CopyFiles => {
            let paths = targets();
            if !paths.is_empty() && pane.is_local() {
                let pasteboard = NSPasteboard::generalPasteboard();
                pasteboard.clearContents();
                let urls: Vec<_> = paths.iter().map(|p| ProtocolObject::from_retained(file_url(p))).collect();
                pasteboard.writeObjects(&NSArray::from_retained_slice(&urls));
            }
        }
        Command::PasteFiles | Command::MoveFilesHere => {
            let sources = pane_view::paths_on_pasteboard(&NSPasteboard::generalPasteboard());
            let kind = if command == Command::PasteFiles { JobKind::Copy } else { JobKind::Move };
            start_job(transfer_job(kind, vfs::local(), sources, pane.fs(), pane.location()));
        }
        Command::CopyPath => {
            let fs = pane.fs();
            // Nothing selected: the folder itself.
            let mut paths = targets();
            if paths.is_empty() {
                paths.push(pane.location());
            }
            let text = paths.iter().map(|p| fs.display_path(p)).collect::<Vec<_>>().join("\n");
            let pasteboard = NSPasteboard::generalPasteboard();
            pasteboard.clearContents();
            pasteboard.setString_forType(&ns(&text), unsafe { NSPasteboardTypeString });
        }
        Command::RevealInFinder if pane.is_local() => {
            let mut paths = targets();
            if paths.is_empty() {
                paths.push(pane.location());
            }
            let urls: Vec<_> = paths.iter().map(|p| file_url(p)).collect();
            NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&urls));
        }
        Command::RevealInFinder => {}
        Command::OpenInTerminal => open_in_terminal(&pane),
        Command::QuickLook => pane_view::toggle_quick_look(&pane),
        Command::ViewFile => {
            // The first file chosen (folders have nothing to view).
            if let Some(entry) = pane.target_entries().into_iter().find(|e| !e.is_folder_like()) {
                viewer::open(pane.fs(), entry.path, entry.size);
            }
        }
        Command::AddToSidebar if pane.is_local() => {
            let folders: Vec<PathBuf> = targets().into_iter().filter(|p| p.is_dir()).collect();
            if folders.is_empty() {
                add_favorite(pane.location());
            } else {
                folders.into_iter().for_each(add_favorite);
            }
        }
        Command::AddToSidebar => add_remote_favorites(&pane),
        Command::RemoveFromSidebar if pane.is_local() => remove_favorite(&pane.location()),
        Command::RemoveFromSidebar => {
            if let Some(site_id) = site_of(&pane.fs()) {
                remove_remote_favorite(&site_id, &pane.location());
            }
        }
        Command::ToggleHidden => {
            let show = {
                let mut settings = app.settings.borrow_mut();
                settings.show_hidden = !settings.show_hidden;
                settings.save();
                settings.show_hidden
            };
            for pane in &app.panes {
                pane.set_show_hidden(show);
            }
        }
        Command::Refresh => pane.reload(),
        Command::Back => pane.go_back(),
        Command::Forward => pane.go_forward(),
        Command::Up => pane.go_up(),
        Command::Home | Command::Desktop | Command::Documents | Command::Downloads | Command::Applications => {
            let place = match command {
                Command::Home => dirs::home_dir(),
                Command::Desktop => dirs::desktop_dir(),
                Command::Documents => dirs::document_dir(),
                Command::Downloads => dirs::download_dir(),
                _ => Some(PathBuf::from("/Applications")),
            };
            if let Some(place) = place {
                pane.navigate_to(vfs::local(), place);
            }
        }
        Command::GoToFolder => go_to_folder(&pane),
        Command::SwitchPane if !other.view().isHidden() => other.focus(),
        Command::SwitchPane => {}
        Command::SwapPanes => {
            let (here_fs, here) = (pane.fs(), pane.location());
            pane.navigate_to(other.fs(), other.location());
            other.navigate_to(here_fs, here);
        }
        Command::SameFolderInOther => other.navigate_to(pane.fs(), pane.location()),
        Command::Connect => {
            if let Some(form) = site_form::show(None, None) {
                connect_from_form(form, index);
            }
        }
        Command::Disconnect if pane.fs().outside().is_some() => close_archive(&pane),
        Command::Disconnect => disconnect(&pane.fs().key()),
        Command::ImportFileZilla => import_filezilla(),
        Command::Permissions => change_permissions(&pane),
        Command::CompareFiles => compare_files(&pane, &other),
        Command::CalculateSizes => calculate_sizes(&pane),
        Command::CopySha256 => copy_checksums(&pane, crate::fs::checksum::Algorithm::Sha256),
        Command::CopyMd5 => copy_checksums(&pane, crate::fs::checksum::Algorithm::Md5),
        Command::CopySha1 => copy_checksums(&pane, crate::fs::checksum::Algorithm::Sha1),
        Command::CopySha384 => copy_checksums(&pane, crate::fs::checksum::Algorithm::Sha384),
        Command::CopySha512 => copy_checksums(&pane, crate::fs::checksum::Algorithm::Sha512),
        Command::ImportCyberduck => site_transfer::import_cyberduck(),
        Command::ImportWinScp => site_transfer::import_winscp(),
        Command::ImportSitesFile => site_transfer::import_file(),
        Command::ExportSites => site_transfer::export_sites(),
        Command::SelectSameType => select_same_type(&pane),
        Command::MoveTabToOther => move_tab_to_other(&pane, &other),
        Command::ReopenClosedTab => reopen_closed_tab(),
        Command::ExportTree => export_tree(&pane),
        Command::ShowDropStack => drop_stack::toggle(),
        Command::DiskUsage => {
            let fs = pane.fs();
            let fs = fs.browse_fs().unwrap_or(fs);
            // The folder the pane shows (a pane always has something
            // selected, so the selection would rarely be what is meant).
            usage_window::open(fs, pane.location());
        }
        Command::Filter => pane.focus_filter(),
        Command::Settings => settings_window::show(),
        Command::NewTab => pane.new_tab(pane.fs(), pane.location()),
        Command::CloseTab => {
            // Command-W in another window (a viewer, Settings) closes that.
            let key = NSApplication::sharedApplication(mtm()).keyWindow();
            match key {
                Some(key) if !std::ptr::eq(&*key, &*app.window) => key.performClose(None),
                _ => {
                    if !pane.close_tab(pane.active_tab()) {
                        app.window.performClose(None);
                    }
                }
            }
        }
        Command::NextTab => pane.cycle_tab(1),
        Command::PreviousTab => pane.cycle_tab(-1),
        Command::OpenInNewTab => {
            let folders: Vec<PathBuf> =
                pane.entries_for(&targets()).into_iter().filter(|e| e.is_folder_like()).map(|e| e.path).collect();
            if folders.is_empty() {
                pane.new_tab(pane.fs(), pane.location());
            } else {
                for folder in folders {
                    pane.new_tab(pane.fs(), folder);
                }
            }
        }
    }
}

/// Reopens the tabs saved for each pane (the first tab is already shown).
fn restore_tabs(app: &App) {
    let (left, right, show_hidden) = {
        let s = app.settings.borrow();
        (s.left.clone(), s.right.clone(), s.show_hidden)
    };
    for (pane, saved) in app.panes.iter().zip([left, right]) {
        if saved.tabs.len() < 2 {
            continue;
        }
        // The pane opened on `location`; it becomes the first tab.
        if pane.location() != saved.tabs[0] {
            pane.navigate_to(vfs::local(), saved.tabs[0].clone());
        }
        for path in saved.tabs.iter().skip(1).filter(|p| p.is_dir()) {
            pane.add_background_tab(PaneState::new(vfs::local(), path.clone(), saved.sort, show_hidden));
        }
        pane.select_tab(saved.active_tab.min(pane.tab_count() - 1));
    }
}

/// A copy of the settings (for the Settings window).
pub fn current_settings() -> Option<Settings> {
    app().map(|a| a.settings.borrow().clone())
}

/// Changes settings, saves them, and applies what changed.
pub fn update_settings(change: impl FnOnce(&mut Settings)) {
    let Some(app) = app() else { return };
    let (show_hidden, limit, appearance_changed, shortcuts_changed, filters_changed) = {
        let mut settings = app.settings.borrow_mut();
        let (appearance, shortcuts, workspaces) =
            (settings.appearance.clone(), settings.shortcuts.clone(), settings.workspaces.clone());
        let filters = (settings.filters.clone(), settings.filters_on);
        change(&mut settings);
        settings.save();
        (
            settings.show_hidden,
            settings.transfers.max_transfers,
            settings.appearance != appearance,
            settings.shortcuts != shortcuts || settings.workspaces != workspaces,
            (settings.filters.clone(), settings.filters_on) != filters,
        )
    };
    if filters_changed {
        apply_filters(&app);
    }
    for pane in &app.panes {
        if pane.show_hidden() != show_hidden {
            pane.set_show_hidden(show_hidden);
        }
    }
    app.activity.set_limit(limit);
    apply_speed_limits(&app);
    if appearance_changed {
        apply_appearance(&app);
    }
    if shortcuts_changed {
        rebuild_main_menu();
    }
}

thread_local! {
    /// What the filename filters leave out of lists (with a version that
    /// changes with them) and of copies.
    static FILTERS: RefCell<(u64, Option<crate::fs::filters::Exclusions>, Option<crate::fs::filters::Exclusions>)> =
        const { RefCell::new((0, None, None)) };
}

/// The filename filters for lists, and their version.
pub fn listing_exclusions() -> (Option<crate::fs::filters::Exclusions>, u64) {
    FILTERS.with(|f| {
        let f = f.borrow();
        (f.1.clone(), f.0)
    })
}

/// The filename filters for copies.
pub fn transfer_exclusions() -> Option<crate::fs::filters::Exclusions> {
    FILTERS.with(|f| f.borrow().2.clone())
}

/// The filename filters from settings, applied to the lists.
fn apply_filters(app: &App) {
    use crate::fs::filters::Exclusions;
    let (filters, on) = {
        let s = app.settings.borrow();
        (s.filters.clone(), s.filters_on)
    };
    let listing = if on { Exclusions::new(&filters, |f| f.listings) } else { None };
    let transfer = if on { Exclusions::new(&filters, |f| f.transfers) } else { None };
    FILTERS.with(|f| {
        let mut f = f.borrow_mut();
        *f = (f.0 + 1, listing, transfer);
    });
    for pane in &app.panes {
        pane.filters_changed();
    }
}

/// The upload and download speed limits from settings.
fn apply_speed_limits(app: &App) {
    let transfers = &app.settings.borrow().transfers;
    ops::set_speed_limits(transfers.upload_limit_kbps * 1024, transfers.download_limit_kbps * 1024);
}

/// Applies the appearance again (after a theme file changed).
pub fn refresh_appearance() {
    if let Some(app) = app() {
        apply_appearance(&app);
    }
}

/// Theme, text size, row spacing, and alternating rows (from settings).
fn apply_appearance(app: &App) {
    use crate::settings::ROW_SPACINGS;
    let appearance = app.settings.borrow().appearance.clone();
    let text = appearance.text_size.clamp(10.0, 20.0);
    let base = ROW_SPACINGS.iter().find(|r| r.0 == appearance.row_spacing).map_or(22.0, |r| r.2);
    // Bigger text needs taller rows.
    let row = (base + (text - 13.0) * 1.5).round().max(16.0);
    pane_view::set_look(text, row);
    // A color theme, or macOS's colors (system, light, dark).
    let theme = crate::theme::find(&appearance.theme);
    themed::set(theme.as_ref(), appearance.alternating_rows);
    app.sidebar.set_zoom(text / 13.0);
    app.sidebar.apply_theme();
    app.preview.set_zoom(text / 13.0);
    for pane in &app.panes {
        pane.apply_appearance(appearance.alternating_rows);
    }
    let background = themed::colors().map_or_else(objc2_app_kit::NSColor::windowBackgroundColor, |c| c.background);
    app.window.setBackgroundColor(Some(&background));
    let name = match (appearance.theme.as_str(), &theme) {
        (_, Some(theme)) if theme.dark => Some(unsafe { objc2_app_kit::NSAppearanceNameDarkAqua }),
        (_, Some(_)) => Some(unsafe { objc2_app_kit::NSAppearanceNameAqua }),
        ("light", None) => Some(unsafe { objc2_app_kit::NSAppearanceNameAqua }),
        ("dark", None) => Some(unsafe { objc2_app_kit::NSAppearanceNameDarkAqua }),
        _ => None,
    };
    let chosen = name.and_then(objc2_app_kit::NSAppearance::appearanceNamed);
    NSApplication::sharedApplication(mtm()).setAppearance(chosen.as_deref());
}

/// Does what a forkz:// link says (see `crate::links`); problems are shown
/// in an alert.
pub fn open_link(link: &str) {
    use crate::links::{Link, Pane};
    tracing::info!("link {link}");
    let Some(app) = app() else { return };
    let pane_index = |pane: Pane| match pane {
        Pane::Active => app.active.get(),
        Pane::Other => 1 - app.active.get(),
        Pane::Left => 0,
        Pane::Right => 1,
    };
    let by_name = |names: Vec<String>, name: &str, what: &str| -> Result<usize, String> {
        names
            .iter()
            .position(|n| n == name)
            .or_else(|| names.iter().position(|n| n.eq_ignore_ascii_case(name)))
            .ok_or_else(|| format!("There is no {what} named \u{201c}{name}\u{201d}."))
    };
    let settings = app.settings.borrow().clone();
    let result: Result<(), String> = match crate::links::parse(link) {
        Err(e) => Err(e),
        Ok(Link::Open { path, pane, new_tab }) => {
            let index = pane_index(pane);
            let (folder, select) = if path.is_dir() {
                (path, Vec::new())
            } else if path.exists() {
                (path.parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf), vec![path])
            } else {
                return dialogs::show_error(
                    "This forkz link cannot be used.".into(),
                    format!("{} does not exist.", path.display()),
                );
            };
            let target = app.panes[index].clone();
            if new_tab {
                target.new_tab(vfs::local(), folder.clone());
            }
            target.navigate_selecting(vfs::local(), folder, &select);
            set_active(index);
            target.focus();
            Ok(())
        }
        Ok(Link::Connect { site, path, pane }) => {
            let sites = app.sites.borrow().sites.clone();
            let found = sites.iter().find(|s| s.id == site).or_else(|| {
                by_name(sites.iter().map(|s| s.name.clone()).collect(), &site, "site").ok().map(|i| &sites[i])
            });
            match found {
                Some(found) => {
                    let mut found = found.clone();
                    if let Some(path) = path {
                        found.remote_dir = Some(path);
                        found.local_dir = None;
                    }
                    let index = pane_index(pane);
                    set_active(index);
                    connect_site(found, index, None, false);
                    Ok(())
                }
                None => Err(format!("There is no saved site named \u{201c}{site}\u{201d}.")),
            }
        }
        Ok(Link::Search { path, text, kind, contents, pane }) => {
            let index = pane_index(pane);
            set_active(index);
            search::search_link(index, path, text, kind.as_deref(), contents)
        }
        Ok(Link::Synclet(name)) => {
            by_name(settings.synclets.iter().map(|s| s.name.clone()).collect(), &name, "synclet").map(open_synclet)
        }
        Ok(Link::SmartFolder { name, pane }) => {
            by_name(settings.smart_folders.iter().map(|s| s.name.clone()).collect(), &name, "smart folder").map(|i| {
                set_active(pane_index(pane));
                search::open_smart_folder(i);
            })
        }
        Ok(Link::Workspace(name)) => {
            by_name(settings.workspaces.iter().map(|w| w.name.clone()).collect(), &name, "workspace")
                .map(workspaces::open)
        }
        Ok(Link::Command(name)) => match Command::ALL.iter().find(|c| c.id().eq_ignore_ascii_case(&name)) {
            Some(command) if is_enabled(*command) => {
                run_command(*command, None);
                Ok(())
            }
            Some(command) => Err(format!("\u{201c}{}\u{201d} cannot be used right now.", command.title())),
            None => Err(format!("forkz has no command named \u{201c}{name}\u{201d}.")),
        },
    };
    drop(settings);
    if let Err(problem) = result {
        dialogs::show_error("This forkz link cannot be used.".into(), problem);
        return;
    }
    NSApplication::sharedApplication(mtm()).activate();
    app.window.makeKeyAndOrderFront(None);
}

/// Remembers a folder a pane shows (Go > Recent Folders): on the Mac, or
/// on a saved site. Search results and archives are left out.
pub fn note_recent_folder(fs: &FsRef, path: &Path) {
    if fs.title_for(path).is_some() || fs.outside().is_some() {
        return;
    }
    let site = if fs.is_local() {
        None
    } else {
        match site_of(fs) {
            Some(site) => Some(site),
            None => return,
        }
    };
    let folder = crate::settings::RecentFolder { site, path: path.to_path_buf() };
    let Some(app) = app() else { return };
    if app.settings.borrow().recent_folders.first() == Some(&folder) {
        return;
    }
    update_settings(|s| {
        s.recent_folders.retain(|f| *f != folder);
        s.recent_folders.insert(0, folder);
        s.recent_folders.truncate(crate::settings::RECENT_FOLDERS);
    });
}

/// A recent folder's title: its name, and the site's for a server.
fn recent_title(folder: &crate::settings::RecentFolder) -> String {
    let name = if folder.path.parent().is_none() { "/".to_string() } else { title_of(&folder.path) };
    let site = folder.site.as_deref().and_then(|id| app()?.sites.borrow().get(id).map(|s| s.name.clone()));
    match site {
        Some(site) => format!("{name} ({site})"),
        None => name,
    }
}

/// Fills Commands > Tools: each tool, then Edit Tools.
fn fill_tools_menu(menu: &NSMenu, delegate: &AppDelegate) {
    menu.removeAllItems();
    let mtm = mtm();
    let tools = current_settings().map(|s| s.tools).unwrap_or_default();
    for (index, tool) in tools.iter().enumerate() {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &ns(&tool.name),
                Some(sel!(runTool:)),
                &ns(""),
            )
        };
        item.setTag(TOOL_TAG + index as isize);
        item.setToolTip(Some(&ns(&tool.command)));
        if let Some(icon) = util::symbol(&tool.symbol, &tool.name) {
            item.setImage(Some(&icon));
        }
        unsafe { item.setTarget(Some(delegate)) };
        menu.addItem(&item);
    }
    if !tools.is_empty() {
        menu.addItem(&NSMenuItem::separatorItem(mtm));
    }
    let edit = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &ns("Edit Tools\u{2026}"),
            Some(sel!(editTools:)),
            &ns(""),
        )
    };
    unsafe { edit.setTarget(Some(delegate)) };
    menu.addItem(&edit);
}

/// Runs a tool on the active pane's selection (on this Mac).
fn run_tool(index: usize) {
    let Some(app) = app() else { return };
    let Some(tool) = app.settings.borrow().tools.get(index).cloned() else { return };
    let active = app.active.get();
    let (pane, other) = (&app.panes[active], &app.panes[1 - active]);
    if !pane.is_local() {
        dialogs::show_error(
            format!("\u{201c}{}\u{201d} runs on folders on this Mac.", tool.name),
            "Tools are shell commands: they cannot work on items on servers or in archives.".into(),
        );
        return;
    }
    let local =
        |p: &PaneController| if p.is_local() { (p.target_paths(), p.location()) } else { (Vec::new(), PathBuf::new()) };
    let (other_selection, other_folder) = local(other);
    let context = custom_tools::Context {
        selection: pane.target_paths(),
        folder: pane.location(),
        other_selection,
        other_folder,
    };
    custom_tools::run(&tool, context);
}

/// The tools changed (Settings > Tools): the toolbar's items show them.
pub fn tools_changed() {
    let Some(app) = app() else { return };
    let Some(toolbar) = app.window.toolbar() else { return };
    let names: Vec<String> = app.settings.borrow().tools.iter().map(|t| t.name.clone()).collect();
    // Items of tools that were renamed or removed go.
    let items = toolbar.items();
    for index in (0..items.count()).rev() {
        let id = items.objectAtIndex(index).itemIdentifier().to_string();
        if let Some(name) = id.strip_prefix(TOOL_ITEM_PREFIX)
            && !names.iter().any(|n| n == name)
        {
            toolbar.removeItemAtIndex(index as isize);
        }
    }
}

/// Fills Go > Recent Folders.
fn fill_recent_menu(menu: &NSMenu, delegate: &AppDelegate) {
    menu.removeAllItems();
    let mtm = mtm();
    let recent = current_settings().map(|s| s.recent_folders).unwrap_or_default();
    for (index, folder) in recent.iter().enumerate() {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &ns(&recent_title(folder)),
                Some(sel!(openRecentFolder:)),
                &ns(""),
            )
        };
        item.setTag(RECENT_TAG + index as isize);
        item.setToolTip(Some(&ns(&folder.path.display().to_string())));
        let icon = if folder.site.is_some() {
            util::symbol("server.rack", "Server").unwrap_or_else(util::folder_icon)
        } else {
            util::icon_for_path(&folder.path)
        };
        icon.setSize(NSSize::new(16.0, 16.0));
        item.setImage(Some(&icon));
        unsafe { item.setTarget(Some(delegate)) };
        menu.addItem(&item);
    }
    if recent.is_empty() {
        let none = NSMenuItem::new(mtm);
        none.setTitle(&ns("No Recent Folders"));
        none.setEnabled(false);
        menu.addItem(&none);
        return;
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let clear = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &ns("Clear Menu"),
            Some(sel!(clearRecentFolders:)),
            &ns(""),
        )
    };
    unsafe { clear.setTarget(Some(delegate)) };
    menu.addItem(&clear);
}

/// Opens a recent folder in the active pane (connecting to its site).
fn open_recent(index: usize) {
    let Some(folder) = current_settings().and_then(|s| s.recent_folders.get(index).cloned()) else { return };
    match folder.site {
        Some(site) => open_remote_favorite(&site, &folder.path),
        None => open_in_active_pane(folder.path),
    }
}

/// The saved site a connection was opened from.
fn site_of(fs: &FsRef) -> Option<String> {
    let app = app()?;
    let connections = app.connections.borrow();
    connections.iter().find(|(_, c)| vfs::same(c, fs)).map(|(id, _)| id.clone())
}

/// Entering a folder selects ".." (a setting).
pub fn start_on_parent() -> bool {
    app().is_some_and(|a| a.settings.borrow().general.start_on_parent)
}

/// Compare Files: two files (or folders) in the pane, or one in each pane,
/// in the built-in compare window or the chosen compare tool (server files
/// are downloaded for those first).
fn compare_files(pane: &PaneController, other: &PaneController) {
    let here = pane.target_paths();
    let there = other.selected_paths();
    let pair: Vec<(FsRef, PathBuf)> = match (here.as_slice(), there.as_slice()) {
        ([a, b], _) => vec![(pane.fs(), a.clone()), (pane.fs(), b.clone())],
        ([a], [b]) => vec![(pane.fs(), a.clone()), (other.fs(), b.clone())],
        _ => {
            dialogs::show_error(
                "Select two files to compare.".into(),
                "Select two files in one pane, or one file in each pane.".into(),
            );
            return;
        }
    };
    // Two files or two folders: tools cannot compare a file with a folder.
    let kinds: Vec<Option<bool>> = pair.iter().map(|(fs, p)| fs.stat(p).ok().map(|e| e.is_folder_like())).collect();
    if kinds[0] != kinds[1] {
        dialogs::show_error(
            "A file cannot be compared with a folder.".into(),
            "Select two files, or two folders.".into(),
        );
        return;
    }
    let tool = app().map(|a| a.settings.borrow().general.compare_tool.clone()).unwrap_or_default();
    if tool == "forkz" {
        let [(lfs, lpath), (rfs, rpath)]: [(FsRef, PathBuf); 2] = pair.try_into().expect("two");
        let (left, right) = (diff_window::Side { fs: lfs, path: lpath }, diff_window::Side { fs: rfs, path: rpath });
        if kinds[0] == Some(true) {
            diff_window::compare_folders(left, right);
        } else {
            diff_window::compare_files(left, right);
        }
        return;
    }
    std::thread::spawn(move || {
        let local: std::io::Result<Vec<PathBuf>> = pair.iter().map(|(fs, path)| local_copy(fs, path)).collect();
        DispatchQueue::main().exec_async(move || {
            let result = local.and_then(|paths| tools::compare(&tool, &paths[0], &paths[1]));
            if let Err(e) = result {
                dialogs::show_error("The files could not be compared.".into(), e.to_string());
            }
        });
    });
}

/// A local path for a file: itself on the Mac, a downloaded copy for a
/// server file (in a temporary folder).
fn local_copy(fs: &FsRef, path: &Path) -> std::io::Result<PathBuf> {
    if fs.is_local() {
        return Ok(path.to_path_buf());
    }
    if fs.stat(path)?.kind == crate::fs::EntryKind::Folder {
        return Err(std::io::Error::other("Folders on servers cannot be compared yet."));
    }
    let folder = std::env::temp_dir().join("forkz-compare").join(crate::sites::new_id());
    std::fs::create_dir_all(&folder)?;
    let target = folder.join(title_of(path));
    let mut reader = fs.open_read(path)?;
    let mut writer = std::fs::File::create(&target)?;
    std::io::copy(&mut reader, &mut writer)?;
    Ok(target)
}

thread_local! {
    /// Tabs closed lately (pane, file system, folder), the latest last.
    static CLOSED_TABS: RefCell<Vec<(usize, FsRef, PathBuf)>> = const { RefCell::new(Vec::new()) };
}

/// The mouse's Back and Forward buttons (buttons 4 and 5) go back and
/// forward in the pane under the pointer.
fn watch_mouse_buttons() {
    use objc2_app_kit::{NSEvent, NSEventMask};
    let block = block2::RcBlock::new(|event: std::ptr::NonNull<NSEvent>| -> *mut NSEvent {
        let raw = event.as_ptr();
        let event = unsafe { event.as_ref() };
        let step = match event.buttonNumber() {
            3 => -1,
            4 => 1,
            _ => return raw,
        };
        let Some(app) = app() else { return raw };
        let in_main = event.window(mtm()).is_some_and(|w| std::ptr::eq(&*w, &*app.window));
        if !in_main {
            return raw;
        }
        let point = event.locationInWindow();
        let under = app.panes.iter().find(|pane| {
            let view = pane.view();
            let local = view.convertPoint_fromView(point, None);
            !view.isHiddenOrHasHiddenAncestor() && objc2_foundation::NSPointInRect(local, view.bounds())
        });
        let pane = under.unwrap_or(&app.panes[app.active.get()]).clone();
        if step < 0 {
            pane.go_back();
        } else {
            pane.go_forward();
        }
        std::ptr::null_mut()
    });
    let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::OtherMouseDown, &block) };
    // Watched for as long as forkz runs.
    std::mem::forget(monitor);
}

/// Remembers a closed tab, for Reopen Closed Tab.
pub fn remember_closed_tab(pane: usize, fs: FsRef, path: PathBuf) {
    CLOSED_TABS.with(|c| {
        let mut closed = c.borrow_mut();
        closed.push((pane, fs, path));
        let extra = closed.len().saturating_sub(20);
        closed.drain(..extra);
    });
}

/// File > Reopen Closed Tab: the last closed tab, in its pane again.
fn reopen_closed_tab() {
    let Some((index, fs, path)) = CLOSED_TABS.with(|c| c.borrow_mut().pop()) else { return };
    let Some(app) = app() else { return };
    // A server since disconnected: its tab opens on the Mac's home.
    let connected =
        fs.is_local() || fs.outside().is_some() || app.connections.borrow().iter().any(|(_, c)| vfs::same(c, &fs));
    let (fs, path) = if connected { (fs, path) } else { (vfs::local(), dirs::home_dir().unwrap_or_default()) };
    let pane = app.panes[index.min(1)].clone();
    pane.new_tab(fs, path);
    set_active(pane.index());
    pane.focus();
}

/// Window > Move Tab to Other Pane.
fn move_tab_to_other(pane: &PaneController, other: &PaneController) {
    let (fs, path) = (pane.fs(), pane.location());
    if pane.close_tab(pane.active_tab()) {
        // Not a closed tab to reopen.
        CLOSED_TABS.with(|c| c.borrow_mut().pop());
        other.new_tab(fs, path);
        set_active(other.index());
        other.focus();
    }
}

/// Edit > Select Same Type: the items like the selected ones (the same
/// extensions; folders for folders).
fn select_same_type(pane: &PaneController) {
    let chosen = pane.target_entries();
    let folders = chosen.iter().any(|e| e.is_folder_like());
    let extensions: std::collections::HashSet<String> =
        chosen.iter().filter(|e| !e.is_folder_like()).map(|e| e.extension()).collect();
    let same: Vec<PathBuf> = pane
        .items()
        .into_iter()
        .filter(|e| if e.is_folder_like() { folders } else { extensions.contains(&e.extension()) })
        .map(|e| e.path)
        .collect();
    pane.select_paths(&same);
}

/// File > Export Folder Tree: a text file listing the pane's folder and
/// everything in it, indented.
fn export_tree(pane: &PaneController) {
    let fs = pane.fs();
    let fs = fs.browse_fs().unwrap_or(fs);
    let root = pane.location();
    let name = if root.parent().is_none() && !fs.is_local() { fs.label() } else { title_of(&root) };
    let panel = objc2_app_kit::NSSavePanel::savePanel(mtm());
    panel.setNameFieldStringValue(&ns(&format!("{name} tree.txt")));
    panel.setMessage(Some(&ns(&format!("A text listing of \u{201c}{name}\u{201d} and everything in it."))));
    if panel.runModal() != objc2_app_kit::NSModalResponseOK {
        return;
    }
    let Some(target) = panel.URL().and_then(|u| util::url_path(&u)) else { return };
    let hidden = pane.show_hidden();
    let Some(activity) = activity_view() else { return };
    let (note, cancel) = activity.begin_task(&format!("Listing \u{201c}{name}\u{201d}\u{2026}"));
    std::thread::spawn(move || {
        let result = crate::fs::tree::write(&fs, &root, hidden, &cancel).and_then(|text| std::fs::write(&target, text));
        DispatchQueue::main().exec_async(move || {
            if let Some(activity) = activity_view() {
                activity.remove(note);
            }
            match result {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => dialogs::show_error("The folder tree could not be saved.".into(), e.to_string()),
            }
        });
    });
}

/// Compare Panes: selects in each pane what differs from the other pane:
/// items the other does not have, newer files, and files of the same date
/// but another size. Folders in both are not looked into.
fn compare_panes(app: &App) {
    let (left, right) = (&app.panes[0], &app.panes[1]);
    let (on_left, on_right) = crate::fs::diff::compare_listings(&left.items(), &right.items());
    left.select_paths(&on_left);
    right.select_paths(&on_right);
    if on_left.is_empty() && on_right.is_empty() {
        dialogs::show_info(
            "Both panes have the same items.".into(),
            "Items are compared by name, date, and size. Folders in both panes are not looked into: use Compare Files on them to compare everything inside.".into(),
        );
    }
}

/// Calculate Folder Sizes: for the selected folders, or every folder in
/// the list, in the background.
fn calculate_sizes(pane: &PaneController) {
    let Some(app) = app() else { return };
    let selected: Vec<PathBuf> = pane
        .entries_for(&pane.target_paths())
        .into_iter()
        .filter(|e| e.kind == crate::fs::EntryKind::Folder)
        .map(|e| e.path)
        .collect();
    let folders = if selected.is_empty() { pane.folder_paths() } else { selected };
    if folders.is_empty() {
        return;
    }
    let (fs, location, index) = (pane.fs(), pane.location(), pane.index());
    let note = app.activity.begin_note("Calculating folder sizes\u{2026}");
    std::thread::spawn(move || {
        // A server gets a connection of its own, so browsing stays quick.
        let walker = if fs.is_local() { fs.clone() } else { fs.connect_another().unwrap_or_else(|_| fs.clone()) };
        for folder in folders {
            let (bytes, _) = ops::measure(&walker, &folder);
            let (fs, location) = (fs.clone(), location.clone());
            DispatchQueue::main().exec_async(move || {
                let Some(app) = self::app() else { return };
                let pane = &app.panes[index];
                if vfs::same(&pane.fs(), &fs) && pane.location() == location {
                    pane.set_folder_size(&folder, bytes);
                }
            });
        }
        DispatchQueue::main().exec_async(move || {
            if let Some(app) = self::app() {
                app.activity.remove(note);
            }
        });
    });
}

/// Copy SHA-256 or MD5 Checksum: of the selected files, to the clipboard.
fn copy_checksums(pane: &PaneController, algorithm: crate::fs::checksum::Algorithm) {
    let Some(app) = app() else { return };
    let files: Vec<PathBuf> =
        pane.entries_for(&pane.target_paths()).into_iter().filter(|e| !e.is_folder_like()).map(|e| e.path).collect();
    if files.is_empty() {
        return;
    }
    let fs = pane.fs();
    let note = app.activity.begin_note(&format!("Calculating {}\u{2026}", algorithm.name()));
    std::thread::spawn(move || {
        let reader = if fs.is_local() { fs.clone() } else { fs.connect_another().unwrap_or_else(|_| fs.clone()) };
        let results: Vec<(PathBuf, std::io::Result<String>)> =
            files.into_iter().map(|f| (f.clone(), crate::fs::checksum::checksum(&reader, &f, algorithm))).collect();
        DispatchQueue::main().exec_async(move || {
            let Some(app) = self::app() else { return };
            app.activity.remove(note);
            let mut copied = Vec::new();
            let mut shown = Vec::new();
            for (path, result) in &results {
                let name = title_of(path);
                match result {
                    Ok(hash) => {
                        // Like shasum: "hash  name" (just the hash for one file).
                        copied.push(if results.len() == 1 { hash.clone() } else { format!("{hash}  {name}") });
                        // Groups of 8 wrap cleanly (the alert would hyphenate).
                        let grouped: Vec<String> =
                            hash.as_bytes().chunks(8).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
                        shown.push(format!("{name}\n{}", grouped.join(" ")));
                    }
                    Err(e) => shown.push(format!("{name}\ncould not be read: {e}")),
                }
            }
            if !copied.is_empty() {
                let pasteboard = NSPasteboard::generalPasteboard();
                pasteboard.clearContents();
                pasteboard.setString_forType(&ns(&copied.join("\n")), unsafe { NSPasteboardTypeString });
            }
            let message = if copied.is_empty() {
                format!("The {} could not be calculated.", algorithm.name())
            } else {
                format!("{} copied to the clipboard.", algorithm.name())
            };
            dialogs::show_info(message, shown.join("\n\n"));
        });
    });
}

/// Add to Sidebar on a server: the selected folders, or the current one,
/// as bookmarks of the site.
fn add_remote_favorites(pane: &PaneController) {
    let Some(app) = app() else { return };
    let Some(site_id) = site_of(&pane.fs()) else { return };
    let folders: Vec<PathBuf> =
        pane.entries_for(&pane.target_paths()).into_iter().filter(|e| e.is_folder_like()).map(|e| e.path).collect();
    let folders = if folders.is_empty() { vec![pane.location()] } else { folders };
    {
        let mut settings = app.settings.borrow_mut();
        for path in folders {
            let favorite = crate::settings::RemoteFavorite { site_id: site_id.clone(), path };
            if !settings.remote_favorites.contains(&favorite) {
                settings.remote_favorites.push(favorite);
            }
        }
        settings.save();
    }
    reload_sidebar();
}

fn remove_remote_favorite(site_id: &str, path: &Path) {
    let Some(app) = app() else { return };
    {
        let mut settings = app.settings.borrow_mut();
        settings.remote_favorites.retain(|f| !(f.site_id == site_id && f.path == path));
        settings.save();
    }
    reload_sidebar();
}

/// A server bookmark in the sidebar: connects (or reuses the connection)
/// and opens the folder.
fn open_remote_favorite(site_id: &str, path: &Path) {
    let Some(app) = app() else { return };
    let Some(mut site) = app.sites.borrow().get(site_id).cloned() else {
        dialogs::show_error("The site of this bookmark was deleted.".into(), String::new());
        return;
    };
    site.remote_dir = Some(path.display().to_string());
    // A bookmark is about this folder: the other pane stays where it is.
    site.local_dir = None;
    connect_site(site, app.active.get(), None, false);
}

/// Asks for new permissions and applies them in the background.
fn change_permissions(pane: &PaneController) {
    let paths = pane.target_paths();
    let entries = pane.entries_for(&paths);
    let Some(first) = entries.first() else { return };
    let title = match entries.as_slice() {
        [one] => format!("Permissions of \u{201c}{}\u{201d}", one.name),
        many => format!("Permissions of {} items", many.len()),
    };
    let mode = first.permissions.unwrap_or(0o644);
    let has_folders = entries.iter().any(|e| e.kind == crate::fs::EntryKind::Folder);
    let Some(choice) = permissions::ask(&title, mode, has_folders) else { return };
    let fs = pane.fs();
    let folder = pane.location();
    std::thread::spawn(move || {
        let failed = permissions::apply(&fs, &paths, &choice);
        DispatchQueue::main().exec_async(move || {
            let Some(app) = app() else { return };
            for pane in &app.panes {
                if vfs::same(&pane.fs(), &fs) && pane.location() == folder {
                    pane.reload();
                }
            }
            activity::report_problems(&Outcome { failed, ..Outcome::default() });
        });
    });
}

/// A copy or move job, with the transfer settings applied.
pub fn transfer_job(
    kind: JobKind,
    source_fs: FsRef,
    sources: Vec<PathBuf>,
    dest_fs: FsRef,
    destination: PathBuf,
) -> Job {
    let mut job = Job::between(kind, source_fs, sources, dest_fs, destination);
    if let Some(app) = app() {
        let settings = app.settings.borrow();
        job.keep_dates = settings.transfers.preserve_timestamps;
        job.on_conflict = match (job.source_fs.is_local(), job.dest_fs.is_local()) {
            (false, true) => settings.transfers.download_conflict,
            (true, false) => settings.transfers.upload_conflict,
            _ => None,
        };
    }
    job
}

/// Trash on the Mac; permanent deletion (after asking) on servers.
fn delete(pane: &PaneController, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    let fs = pane.fs();
    if fs.is_local() {
        // Apps' details first: their bundles are gone once trashed.
        let offer = app().is_some_and(|a| a.settings.borrow().general.app_leftovers);
        let apps: Vec<(PathBuf, crate::fs::apps::AppInfo)> = if offer {
            paths.iter().filter_map(|p| Some((p.clone(), crate::fs::apps::app_info(p)?))).collect()
        } else {
            Vec::new()
        };
        start_job(Job::local(JobKind::Trash, paths.clone(), pane.location()));
        if !apps.is_empty() {
            // Once the apps are in the Trash (not if that failed or was
            // stopped).
            std::thread::spawn(move || {
                for _ in 0..120 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    if apps.iter().all(|(path, _)| path.symlink_metadata().is_err()) {
                        let apps: Vec<_> = apps.into_iter().map(|(_, app)| app).collect();
                        DispatchQueue::main().exec_async(move || offer_app_leftovers(&apps, &paths));
                        return;
                    }
                }
            });
        }
        return;
    }
    let what = match paths.as_slice() {
        [one] => format!("\u{201c}{}\u{201d}", title_of(one)),
        many => format!("{} items", many.len()),
    };
    let message = format!("Delete {what} from {}?", fs.label());
    let why = if fs.outside().is_some() {
        "Items in an archive cannot go to the Trash: this cannot be undone."
    } else {
        "Servers have no Trash: this cannot be undone."
    };
    if dialogs::confirm(&message, why, "Delete", true) {
        start_job(Job::between(JobKind::Delete, fs.clone(), paths, fs, pane.location()));
    }
}

/// After apps went to the Trash: offers to move their leftover files
/// (settings, caches, and support files in ~/Library) there too.
fn offer_app_leftovers(apps: &[crate::fs::apps::AppInfo], trashed: &[PathBuf]) {
    let Some(home) = dirs::home_dir() else { return };
    let found: Vec<PathBuf> = apps
        .iter()
        .flat_map(|app| crate::fs::apps::leftovers(&home, app))
        .filter(|p| !trashed.iter().any(|t| p.starts_with(t)))
        .collect();
    if found.is_empty() {
        return;
    }
    let names = match apps {
        [one] => format!("\u{201c}{}\u{201d}", one.name),
        _ => "these apps".to_string(),
    };
    let message = format!("Also move the leftover files of {names} to the Trash?");
    let informative =
        "Settings, caches, and support files the app kept in your Library folder. Uncheck any you want to keep.";
    if let Some(chosen) = dialogs::choose_paths(&message, informative, "Move to Trash", "Keep", &found, &home) {
        start_job(Job::local(JobKind::Trash, chosen, home.join("Library")));
    }
}

/// Downloads remote files to a temporary folder and opens them.
pub fn open_remote(fs: FsRef, files: Vec<PathBuf>) {
    if files.is_empty() {
        return;
    }
    // Each file in a folder of its own: files with the same name from
    // different folders stay apart, and each is watched for saves.
    for file in files {
        let folder = std::env::temp_dir().join("forkz-open").join(crate::sites::new_id());
        if let Err(e) = std::fs::create_dir_all(&folder) {
            dialogs::show_error("The file could not be downloaded.".into(), e.to_string());
            return;
        }
        let mut job = Job::between(JobKind::Copy, fs.clone(), vec![file], vfs::local(), folder);
        job.on_conflict = Some(Resolution::Replace);
        job.open_when_done = true;
        start_job(job);
    }
}

fn open_in_terminal(pane: &PaneController) {
    let fs = pane.fs();
    let folder = match pane.target_paths().as_slice() {
        [one] if pane.is_local() && one.is_dir() => one.clone(),
        _ => pane.location(),
    };
    let terminal = app().map(|a| a.settings.borrow().general.terminal.clone()).unwrap_or_default();
    let result = if fs.is_local() { tools::open_terminal(&terminal, &folder) } else { ssh_in_terminal(&fs, &folder) };
    if let Err(e) = result {
        dialogs::show_error("Terminal could not be opened.".into(), e.to_string());
    }
}

/// Opens an SSH session in Terminal, in the pane's remote folder.
fn ssh_in_terminal(fs: &FsRef, folder: &Path) -> std::io::Result<()> {
    let Some(app) = app() else { return Ok(()) };
    let connections = app.connections.borrow();
    let site_id = connections.iter().find(|(_, c)| vfs::same(c, fs)).map(|(id, _)| id.clone());
    let site = site_id.and_then(|id| app.sites.borrow().get(&id).cloned());
    let Some(site) = site else { return Err(std::io::Error::other("The connection\u{2019}s site is unknown.")) };
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    let mut command = format!("ssh -p {} ", site.port);
    if let (vfs::sftp::AuthMethod::Key, Some(key)) = (site.auth, &site.key_file) {
        command.push_str(&format!("-i {} ", quote(&key.to_string_lossy())));
    }
    let target = if site.user.is_empty() { site.host.clone() } else { format!("{}@{}", site.user, site.host) };
    let remote = format!("cd {} && exec $SHELL -l", quote(&folder.to_string_lossy()));
    command.push_str(&format!("-t {} {}", quote(&target), quote(&remote)));
    tools::run_ssh(&app.settings.borrow().general.terminal, &command)
}

fn new_folder(pane: &PaneController) {
    match ops::new_folder(&pane.fs(), &pane.location()) {
        Ok(path) => {
            let place = if pane.is_local() { path.display().to_string() } else { pane.fs().display_path(&path) };
            oplog::record(oplog::Kind::Change, "Created a folder", place);
            pane.reload_selecting(std::slice::from_ref(&path));
            // Name it right away, like Finder.
            rename_path(pane, &path);
        }
        Err(e) => dialogs::show_error("The folder could not be created.".into(), e.to_string()),
    }
}

fn rename(pane: &PaneController) {
    let targets = pane.target_paths();
    match targets.as_slice() {
        [path] => rename_path(pane, path),
        [] => {}
        _ => multi_rename(pane),
    }
}

/// Renames the selected items with rules (see `multi_rename`).
fn multi_rename(pane: &PaneController) {
    multi_rename_with(pane, None);
}

/// Rename Multiple Items starting from a saved preset.
fn rename_with_preset(pane: &PaneController, index: usize) {
    let steps = current_settings().and_then(|s| s.rename_presets.get(index).map(|p| p.steps.clone()));
    if let Some(steps) = steps {
        multi_rename_with(pane, Some(steps));
    }
}

fn multi_rename_with(pane: &PaneController, initial: Option<Vec<crate::rename::Step>>) {
    let paths = pane.target_paths();
    if paths.is_empty() {
        return;
    }
    rename_entries(pane.fs(), pane.location(), pane.entries_for(&paths), pane.all_names(), initial);
}

/// A rename preset applied to items dropped on it in the sidebar (or the
/// active pane's selection when it is clicked): Rename Multiple Items opens
/// with its rules, to check and confirm.
pub fn apply_rename_preset(index: usize, items: Option<(FsRef, Vec<PathBuf>)>) {
    let Some(app) = app() else { return };
    let steps = current_settings().and_then(|s| s.rename_presets.get(index).map(|p| p.steps.clone()));
    let Some(steps) = steps else { return };
    let Some((fs, paths)) = items else {
        let pane = app.panes[app.active.get()].clone();
        if pane.target_paths().is_empty() {
            dialogs::show_error("Select the items to rename first.".into(), "Or drag them onto the preset.".into());
        } else {
            multi_rename_with(&pane, Some(steps));
        }
        return;
    };
    let Some(dir) = paths.first().and_then(|p| p.parent()).map(Path::to_path_buf) else { return };
    if paths.iter().any(|p| p.parent() != Some(dir.as_path())) {
        dialogs::show_error(
            "Drop items from one folder at a time.".into(),
            "A preset renames items within their folder.".into(),
        );
        return;
    }
    let entries: Vec<crate::fs::Entry> = paths.iter().filter_map(|p| fs.stat(p).ok()).collect();
    let existing = fs.list(&dir).map(|list| list.into_iter().map(|e| e.name).collect()).unwrap_or_default();
    rename_entries(fs, dir, entries, existing, Some(steps));
}

/// Rename Multiple Items for entries of one folder; panes showing it are
/// reloaded with the renamed items selected.
fn rename_entries(
    fs: FsRef,
    dir: PathBuf,
    entries: Vec<crate::fs::Entry>,
    existing: Vec<String>,
    initial: Option<Vec<crate::rename::Step>>,
) {
    if entries.is_empty() {
        return;
    }
    let local = fs.is_local();
    let items: Vec<crate::rename::Item> = entries
        .into_iter()
        .map(|e| crate::rename::Item {
            folder: e.kind == crate::fs::EntryKind::Folder && !e.package,
            created: if local { std::fs::metadata(&e.path).and_then(|m| m.created()).ok() } else { None },
            name: e.name,
            modified: e.modified,
        })
        .collect();
    let Some(planned) = multi_rename::show(items, existing, initial) else { return };
    let changed = planned.iter().filter(|p| p.changes()).count();
    std::thread::spawn(move || {
        let (done, failed) = crate::rename::apply(&fs, &dir, &planned);
        DispatchQueue::main().exec_async(move || {
            let Some(app) = app() else { return };
            for pane in &app.panes {
                if vfs::same(&pane.fs(), &fs) && pane.location() == dir {
                    pane.reload_selecting(&done);
                }
            }
            let place = if fs.is_local() { dir.display().to_string() } else { fs.display_path(&dir) };
            let detail = failed.iter().map(|(name, e)| format!("{name}: {e}")).collect::<Vec<_>>().join("\n");
            let kind = if failed.is_empty() { oplog::Kind::Change } else { oplog::Kind::Problem };
            oplog::record(kind, format!("Renamed {} of {changed} items in {place}", done.len()), detail);
            if !failed.is_empty() {
                let detail = failed.iter().map(|(name, e)| format!("{name}: {e}")).collect::<Vec<_>>().join("\n");
                dialogs::show_error(format!("{} of {changed} items could not be renamed.", failed.len()), detail);
            }
        });
    });
}

fn rename_path(pane: &PaneController, path: &Path) {
    let name = title_of(path);
    let fs = pane.fs();
    let is_folder = fs.stat(path).is_ok_and(|e| e.is_folder_like());
    // Select the name without its extension.
    let stem_len = match name.rfind('.') {
        Some(dot) if dot > 0 && !is_folder => name[..dot].chars().count(),
        _ => name.chars().count(),
    };
    let Some(new_name) = dialogs::prompt(
        &format!("Rename \u{201c}{name}\u{201d}"),
        "Enter a new name:",
        "Rename",
        &name,
        Some(stem_len),
    ) else {
        return;
    };
    match ops::rename(&fs, path, new_name.trim()) {
        Ok(renamed) => {
            let folder = if fs.is_local() {
                renamed.parent().map(|p| p.display().to_string())
            } else {
                renamed.parent().map(|p| fs.display_path(p))
            };
            oplog::record(
                oplog::Kind::Change,
                format!("Renamed \u{201c}{name}\u{201d} to \u{201c}{}\u{201d}", title_of(&renamed)),
                folder.unwrap_or_default(),
            );
            pane.reload_selecting(&[renamed]);
            other_pane_reload(pane.index());
        }
        Err(e) => {
            oplog::record(oplog::Kind::Problem, format!("Could not rename \u{201c}{name}\u{201d}"), e.to_string());
            dialogs::show_error(format!("\u{201c}{name}\u{201d} could not be renamed."), e.to_string());
        }
    }
}

fn go_to_folder(pane: &PaneController) {
    let fs = pane.fs();
    let current = pane.location().display().to_string();
    let hint = if fs.is_local() { "Enter a path (~ for your home folder):" } else { "Enter a path on the server:" };
    let Some(text) = dialogs::prompt("Go to Folder", hint, "Go", &current, None) else {
        return;
    };
    let text = text.trim();
    // A server address (sftp://, smb://, and so on) connects.
    if let Some((scheme, _)) = text.split_once("://")
        && crate::sites::Protocol::from_scheme(scheme).is_some()
    {
        open_target(text);
        return;
    }
    let path = match text.strip_prefix('~') {
        Some(rest) => fs.home().join(rest.trim_start_matches('/')),
        None if Path::new(text).is_absolute() => PathBuf::from(text),
        None => pane.location().join(text),
    };
    match fs.stat(&path) {
        Ok(entry) if entry.is_folder_like() || entry.package => pane.navigate(path),
        Ok(_) => {
            if let Some(parent) = path.parent() {
                pane.navigate_selecting(fs.clone(), parent.to_path_buf(), std::slice::from_ref(&path));
            }
        }
        Err(_) => dialogs::show_error("The folder can\u{2019}t be found.".into(), fs.display_path(&path)),
    }
}

fn other_pane_reload(index: usize) {
    if let Some(app) = app() {
        let (pane, other) = (&app.panes[index], &app.panes[1 - index]);
        if other.location() == pane.location() && vfs::same(&other.fs(), &pane.fs()) {
            other.reload();
        }
    }
}

pub fn start_job(job: Job) {
    if job.sources.is_empty() {
        return;
    }
    if let Some(app) = app() {
        app.activity.start(job);
    }
}

fn with_activity(f: impl FnOnce(&Activity)) {
    if let Some(app) = app() {
        f(&app.activity);
    }
}

/// What a finished job did, for the log.
fn log_job(job: &Job, outcome: &Outcome) {
    let what = match job.sources.as_slice() {
        [one] => format!("\u{201c}{}\u{201d}", title_of(one)),
        many => format!("{} items", many.len()),
    };
    let place =
        |fs: &FsRef, path: &Path| if fs.is_local() { path.display().to_string() } else { fs.display_path(path) };
    let text = match (job.kind, !on_server(&job.source_fs), !on_server(&job.dest_fs)) {
        (JobKind::Copy, true, false) => format!("Uploaded {what} to {}", place(&job.dest_fs, &job.destination)),
        (JobKind::Copy, false, true) => format!("Downloaded {what} to {}", job.destination.display()),
        (JobKind::Copy, ..) => format!("Copied {what} to {}", place(&job.dest_fs, &job.destination)),
        (JobKind::Move, ..) => format!("Moved {what} to {}", place(&job.dest_fs, &job.destination)),
        (JobKind::Trash, ..) => format!("Moved {what} to the Trash"),
        (JobKind::Delete, ..) => {
            let from =
                job.sources.first().and_then(|s| s.parent()).map(|p| place(&job.source_fs, p)).unwrap_or_default();
            format!("Deleted {what} from {from}")
        }
    };
    let text = if outcome.cancelled { format!("{text} (stopped)") } else { text };
    let mut detail: Vec<String> = outcome.failed.iter().map(|(path, e)| format!("{}: {e}", title_of(path))).collect();
    if outcome.skipped > 0 {
        detail.push(format!("{} skipped", outcome.skipped));
    }
    let failed = !outcome.failed.is_empty();
    let detail = detail.join("\n");
    match job.kind {
        JobKind::Copy | JobKind::Move => oplog::record_transfer(text, detail, failed),
        _ if failed => oplog::record(oplog::Kind::Problem, text, detail),
        _ => oplog::record(oplog::Kind::Change, text, detail),
    }
}

fn job_finished(id: isize, job: Job, outcome: Outcome, took: std::time::Duration) {
    let Some(app) = app() else { return };
    app.activity.remove(id);
    app.activity.job_done(id);
    // Sound cues (downloads to open a file are not transfers to hear).
    let cue = match job.kind {
        _ if outcome.cancelled => None,
        JobKind::Trash if !outcome.created.is_empty() => Some(sounds::Cue::Trash),
        JobKind::Delete if outcome.failed.is_empty() => Some(sounds::Cue::Delete),
        JobKind::Copy | JobKind::Move if took >= sounds::LONG_TRANSFER && !job.open_when_done => {
            Some(if outcome.failed.is_empty() { sounds::Cue::Done } else { sounds::Cue::Failed })
        }
        _ => None,
    };
    if let Some(cue) = cue {
        sounds::play(cue);
    }
    promises::finished(&job, &outcome);
    log_job(&job, &outcome);
    let shows =
        |pane: &PaneController, fs: &FsRef, folder: &Path| vfs::same(&pane.fs(), fs) && pane.location() == folder;
    for pane in &app.panes {
        let is_destination =
            matches!(job.kind, JobKind::Copy | JobKind::Move) && shows(pane, &job.dest_fs, &job.destination);
        let is_source = job.sources.first().and_then(|s| s.parent()).is_some_and(|p| shows(pane, &job.source_fs, p));
        if is_destination && !outcome.created.is_empty() {
            pane.reload_selecting(&outcome.created);
        } else if is_destination || is_source || pane.is_local() {
            pane.reload();
        }
    }
    if job.open_when_done && !outcome.cancelled {
        let workspace = NSWorkspace::sharedWorkspace();
        for path in &outcome.created {
            // A server file: saving the copy uploads it back.
            if !job.source_fs.is_local()
                && !job.source_fs.read_only()
                && let Some(remote) = job.sources.iter().find(|s| s.file_name() == path.file_name())
            {
                remote_edit::track(job.source_fs.clone(), remote.clone(), path.clone());
            }
            workspace.openURL(&file_url(path));
        }
    }
    activity::report_problems(&outcome);
}

thread_local! {
    /// Sync Browsing: the folders both panes started from.
    static SYNC_BROWSING: RefCell<Option<(PathBuf, PathBuf)>> = const { RefCell::new(None) };
    /// The other pane is following (so it does not lead back).
    static FOLLOWING: Cell<bool> = const { Cell::new(false) };
}

/// Sync Browsing: opening a folder in one pane opens the same folder (by
/// its path inside where they started) in the other.
fn toggle_sync_browsing() {
    let Some(app) = app() else { return };
    let on = SYNC_BROWSING.with(|s| s.borrow().is_some());
    let bases = (!on).then(|| (app.panes[0].location(), app.panes[1].location()));
    SYNC_BROWSING.with(|s| *s.borrow_mut() = bases);
    let text = if on { "Sync Browsing off" } else { "Sync Browsing on" };
    oplog::record(oplog::Kind::Change, text, "");
}

fn follow_sync_browsing(index: usize) {
    if FOLLOWING.with(Cell::get) {
        return;
    }
    let Some(app) = app() else { return };
    let Some((left, right)) = SYNC_BROWSING.with(|s| s.borrow().clone()) else { return };
    let (here_base, other_base) = if index == 0 { (left, right) } else { (right, left) };
    let here = app.panes[index].location();
    let other = &app.panes[1 - index];
    let target = if let Ok(inside) = here.strip_prefix(&here_base) {
        other_base.join(inside)
    } else if let Ok(above) = here_base.strip_prefix(&here) {
        // Went up past where it started: the other goes up as far, and
        // both start from there.
        let levels = above.components().count();
        let mut target = other_base.clone();
        for _ in 0..levels {
            target = target.parent().map(Path::to_path_buf).unwrap_or(target);
        }
        let bases = if index == 0 { (here.clone(), target.clone()) } else { (target.clone(), here.clone()) };
        SYNC_BROWSING.with(|s| *s.borrow_mut() = Some(bases));
        target
    } else {
        return;
    };
    if other.location() == target || (other.is_local() && !target.is_dir()) {
        return;
    }
    FOLLOWING.with(|f| f.set(true));
    other.navigate(target);
    FOLLOWING.with(|f| f.set(false));
}

/// A side of a sync from a pane (search results: the folder searched).
fn sync_side(pane: &PaneController) -> sync_window::Side {
    let fs = pane.fs();
    let fs = fs.browse_fs().unwrap_or(fs);
    let path = pane.location();
    // A mounted share is kept as its site (it may not be mounted later).
    let saved = if fs.read_only() {
        None
    } else if let Some(site) = site_of(&fs) {
        Some(crate::settings::WorkspaceTab { site: Some(site), path: path.clone() })
    } else if fs.is_local() {
        Some(crate::settings::WorkspaceTab { site: None, path: path.clone() })
    } else {
        None
    };
    sync_window::Side { fs, path, saved }
}

/// Sync Folders: the two panes' folders, or a synclet's (with its options,
/// compared right away).
fn sync_folders(sides: Option<(sync_window::Side, sync_window::Side, crate::fs::sync::Options)>) {
    let Some(app) = app() else { return };
    let compare_now = sides.is_some();
    let (left, right, options) = sides.unwrap_or_else(|| {
        // Deleting is never carried over from an earlier sync of other
        // folders (synclets keep their own choice).
        let options = crate::fs::sync::Options { delete_orphans: false, ..app.settings.borrow().sync.clone() };
        (sync_side(&app.panes[0]), sync_side(&app.panes[1]), options)
    });
    if left.fs.read_only() && right.fs.read_only() {
        dialogs::show_error(
            "Archives cannot be synced.".into(),
            "At least one side must be a folder that can change.".into(),
        );
        return;
    }
    let Some((options, items)) = sync_window::show(left.clone(), right.clone(), &options, compare_now) else { return };
    if !compare_now {
        update_settings(|s| s.sync = options.clone());
    }
    let keep_dates = app.settings.borrow().transfers.preserve_timestamps;
    let chosen: Vec<&crate::fs::sync::Item> = items.iter().collect();
    let jobs = crate::fs::sync::jobs(&chosen, &left.fs, &left.path, &right.fs, &right.path, keep_dates);
    let count = items.len();
    let (note, cancel) =
        app.activity.begin_task(&format!("Syncing {count} {}\u{2026}", if count == 1 { "item" } else { "items" }));
    let place = |side: &sync_window::Side| {
        if side.fs.is_local() { side.path.display().to_string() } else { side.fs.display_path(&side.path) }
    };
    let (left_text, right_text) = (place(&left), place(&right));
    std::thread::spawn(move || {
        let mut failed: Vec<String> = Vec::new();
        for job in &jobs {
            if cancel.is_cancelled() {
                break;
            }
            let mut observer = SyncObserver { detail: TaskDetail { note, last: None } };
            let outcome = ops::run(job, &mut observer, cancel.clone());
            failed.extend(outcome.failed.iter().map(|(path, e)| format!("{}: {e}", title_of(path))));
        }
        let stopped = cancel.is_cancelled();
        DispatchQueue::main().exec_async(move || {
            let Some(app) = self::app() else { return };
            app.activity.remove(note);
            for pane in &app.panes {
                pane.reload();
            }
            let text = format!(
                "Synced {left_text} and {right_text}: {count} {}{}",
                if count == 1 { "item" } else { "items" },
                if stopped { " (stopped)" } else { "" }
            );
            let kind = if failed.is_empty() { oplog::Kind::Transfer } else { oplog::Kind::Problem };
            oplog::record(kind, text, failed.join("\n"));
            if !failed.is_empty() {
                let more =
                    if failed.len() > 8 { format!("\n\u{2026}and {} more", failed.len() - 8) } else { String::new() };
                let list: Vec<String> = failed.iter().take(8).cloned().collect();
                dialogs::show_error(format!("{} items could not be synced.", failed.len()), list.join("\n") + &more);
            }
        });
    });
}

/// Progress for a sync, which carries on past errors (listed at the end).
struct SyncObserver {
    detail: TaskDetail,
}

impl ops::Observer for SyncObserver {
    fn progress(&mut self, progress: &ops::Progress) {
        let text = if progress.total_bytes > 0 {
            format!(
                "{} of {}: {}",
                crate::fs::format_size(progress.done_bytes),
                crate::fs::format_size(progress.total_bytes),
                progress.current
            )
        } else {
            progress.current.clone()
        };
        self.detail.show(text, false);
    }
    fn conflict(&mut self, _conflict: &ops::Conflict) -> ops::Answer {
        ops::Answer { resolution: ops::Resolution::Replace, apply_to_all: true }
    }
    fn error(&mut self, _path: &Path, _error: &std::io::Error) -> ops::ErrorAnswer {
        ops::ErrorAnswer::Skip
    }
}

/// Opens a synclet: connects to its sites if needed, then shows the Sync
/// window with its folders and options, already compared.
pub fn open_synclet(index: usize) {
    let Some(app) = app() else { return };
    let Some(synclet) = app.settings.borrow().synclets.get(index).cloned() else { return };
    let sites = app.sites.borrow().clone();
    let existing = |tab: &crate::settings::WorkspaceTab| -> Option<FsRef> {
        match &tab.site {
            None => Some(vfs::local()),
            Some(id) => app.connections.borrow().iter().find(|(sid, _)| sid == id).map(|(_, fs)| fs.clone()),
        }
    };
    let ready = (existing(&synclet.left), existing(&synclet.right));
    if let (Some(l), Some(r)) = &ready {
        let left =
            sync_window::Side { fs: l.clone(), path: synclet.left.path.clone(), saved: Some(synclet.left.clone()) };
        let right =
            sync_window::Side { fs: r.clone(), path: synclet.right.path.clone(), saved: Some(synclet.right.clone()) };
        sync_folders(Some((left, right, synclet.options.clone())));
        return;
    }
    // Connect what is not connected yet (off the main thread).
    let mut missing = Vec::new();
    for (tab, fs) in [(&synclet.left, &ready.0), (&synclet.right, &ready.1)] {
        if fs.is_none()
            && let Some(id) = &tab.site
        {
            match sites.get(id) {
                Some(site) => missing.push(site.clone()),
                None => {
                    dialogs::show_error(
                        format!("The synclet \u{201c}{}\u{201d} uses a site that was deleted.", synclet.name),
                        String::new(),
                    );
                    return;
                }
            }
        }
    }
    missing.dedup_by(|a, b| a.id == b.id);
    let (timeout, extra_keys) = {
        let settings = app.settings.borrow();
        (Duration::from_secs(settings.sftp.timeout_seconds.max(5)), settings.sftp.key_files.clone())
    };
    let note = app.activity.begin_note(&format!("Connecting for \u{201c}{}\u{201d}\u{2026}", synclet.name));
    std::thread::spawn(move || {
        let mut connected: Vec<(String, std::io::Result<FsRef>)> = Vec::new();
        for site in &missing {
            let password = crate::sites::keychain::password(&site.id);
            let prompter = UiPrompter { offer_save: true, save: std::sync::Mutex::new(false) };
            let result = open_connection(site, password, timeout, extra_keys.clone(), &prompter).map(|(fs, _)| fs);
            connected.push((site.id.clone(), result));
        }
        DispatchQueue::main().exec_async(move || {
            let Some(app) = self::app() else { return };
            app.activity.remove(note);
            for (id, result) in connected {
                match result {
                    Ok(fs) => app.connections.borrow_mut().push((id, fs)),
                    Err(e) => {
                        if e.kind() != std::io::ErrorKind::Interrupted {
                            dialogs::show_error("Could not connect for the synclet.".into(), e.to_string());
                        }
                        return;
                    }
                }
            }
            reload_sidebar();
            open_synclet(index);
        });
    });
}

pub fn delete_synclet(index: usize) {
    let Some(name) = current_settings().and_then(|s| s.synclets.get(index).map(|x| x.name.clone())) else { return };
    if dialogs::confirm(
        &format!("Delete the synclet \u{201c}{name}\u{201d}?"),
        "The folders are not changed.",
        "Delete",
        true,
    ) {
        update_settings(|s| s.synclets.retain(|x| x.name != name));
        reload_sidebar();
    }
}

/// Makes an empty file (named first) in the pane's folder.
fn new_file(pane: &PaneController) {
    let initial = "untitled.txt";
    let Some(name) = dialogs::prompt("New File", "Name of the new file:", "Create", initial, Some("untitled".len()))
    else {
        return;
    };
    let name = name.trim().to_string();
    if name.contains('/') {
        dialogs::show_error("Names cannot contain \u{201c}/\u{201d}.".into(), String::new());
        return;
    }
    let fs = pane.fs();
    let path = pane.location().join(&name);
    if fs.stat(&path).is_ok() {
        dialogs::show_error(format!("\u{201c}{name}\u{201d} already exists."), "Choose another name.".into());
        return;
    }
    let result = fs.open_write_sized(&path, Some(0)).and_then(|mut w| std::io::Write::flush(&mut w));
    match result {
        Ok(()) => {
            let place = if fs.is_local() { path.display().to_string() } else { fs.display_path(&path) };
            oplog::record(oplog::Kind::Change, format!("Created \u{201c}{name}\u{201d}"), place);
            pane.reload_selecting(&[path]);
        }
        Err(e) => dialogs::show_error(format!("\u{201c}{name}\u{201d} could not be created."), e.to_string()),
    }
}

/// Moves the selection into a new folder (named first).
fn new_folder_with_selection(pane: &PaneController) {
    let paths = pane.target_paths();
    if paths.is_empty() {
        return;
    }
    let initial = "New Folder With Items";
    let what = if paths.len() == 1 { "1 item".to_string() } else { format!("{} items", paths.len()) };
    let Some(name) = dialogs::prompt(
        "New Folder with Selection",
        &format!("A folder for the {what}:"),
        "Create",
        initial,
        Some(initial.chars().count()),
    ) else {
        return;
    };
    let name = name.trim().to_string();
    let fs = pane.fs();
    let folder = pane.location().join(&name);
    if name.contains('/') || fs.stat(&folder).is_ok() {
        dialogs::show_error(
            format!("\u{201c}{name}\u{201d} cannot be used."),
            "It exists already, or has a \u{201c}/\u{201d}.".into(),
        );
        return;
    }
    if let Err(e) = fs.create_dir(&folder) {
        dialogs::show_error(format!("\u{201c}{name}\u{201d} could not be created."), e.to_string());
        return;
    }
    start_job(Job::between(JobKind::Move, fs.clone(), paths, fs, folder.clone()));
    pane.reload_selecting(&[folder]);
}

/// Deletes the selection right away (not to the Trash), after asking.
fn delete_immediately(pane: &PaneController) {
    let paths = pane.target_paths();
    if paths.is_empty() {
        return;
    }
    let fs = pane.fs();
    let what = match paths.as_slice() {
        [one] => format!("\u{201c}{}\u{201d}", title_of(one)),
        many => format!("{} items", many.len()),
    };
    if dialogs::confirm(
        &format!("Delete {what} immediately?"),
        "It does not go to the Trash: this cannot be undone.",
        "Delete",
        true,
    ) {
        start_job(Job::between(JobKind::Delete, fs.clone(), paths, fs, pane.location()));
    }
}

/// The macOS Share menu for the selection (on the Mac).
fn share(pane: &PaneController) {
    let paths = pane.target_paths();
    if paths.is_empty() {
        return;
    }
    let urls: Vec<Retained<AnyObject>> =
        paths.iter().map(|p| Retained::into_super(Retained::into_super(file_url(p)))).collect();
    let picker = unsafe {
        objc2_app_kit::NSSharingServicePicker::initWithItems(
            <objc2_app_kit::NSSharingServicePicker as objc2::AnyThread>::alloc(),
            &NSArray::from_retained_slice(&urls),
        )
    };
    let table = pane.table();
    let row = table.selectedRow();
    let rect = if row >= 0 { table.rectOfRow(row) } else { table.visibleRect() };
    picker.showRelativeToRect_ofView_preferredEdge(rect, table, objc2_foundation::NSRectEdge::MaxY);
}

fn activity_view() -> Option<Retained<Activity>> {
    app().map(|a| a.activity.clone())
}

fn pane(index: usize) -> Option<Retained<PaneController>> {
    app().and_then(|a| a.panes.get(index).cloned())
}

fn active_pane() -> Option<Retained<PaneController>> {
    app().and_then(|a| a.panes.get(a.active.get()).cloned())
}

/// Opens the selection with each item's default app (archives too, instead
/// of browsing them). Server and archive items are downloaded first.
fn open_with_app(pane: &PaneController) {
    let paths = pane.target_paths();
    if paths.is_empty() {
        return;
    }
    if pane.is_local() {
        let workspace = NSWorkspace::sharedWorkspace();
        for path in &paths {
            workspace.openURL(&file_url(path));
        }
    } else {
        let files: Vec<PathBuf> =
            pane.entries_for(&paths).into_iter().filter(|e| !e.is_folder_like()).map(|e| e.path).collect();
        open_remote(pane.fs(), files);
    }
}

/// Opens a file in a chosen app.
fn open_with(file: &Path, app: &Path) {
    let configuration = objc2_app_kit::NSWorkspaceOpenConfiguration::configuration();
    let urls = NSArray::from_retained_slice(&[file_url(file)]);
    NSWorkspace::sharedWorkspace().openURLs_withApplicationAtURL_configuration_completionHandler(
        &urls,
        &file_url(app),
        &configuration,
        None,
    );
}

/// Compresses the selection into an archive beside it (the name chosen
/// picks the format). On a server, the items are downloaded to a temporary
/// folder, compressed, and the archive is uploaded beside them.
fn compress(pane: &PaneController) {
    let paths = pane.target_paths();
    let Some(first) = paths.first() else { return };
    let fs = pane.fs();
    // Search results: the items' real file system.
    let fs = fs.browse_fs().unwrap_or(fs);
    let folder = pane.location();
    let suggested = if paths.len() == 1 { format!("{}.zip", title_of(first)) } else { "Archive.zip".to_string() };
    let stem = suggested.len() - ".zip".len();
    let Some(name) = dialogs::prompt(
        "Compress",
        "Name of the archive: .zip, .tar.gz, .tar.bz2, .tar.xz, .tar, or .7z.",
        "Compress",
        &suggested,
        Some(suggested[..stem].chars().count()),
    ) else {
        return;
    };
    let name = name.trim().to_string();
    if vfs::archive::Format::for_name(&name).is_none() {
        dialogs::show_error(
            "forkz cannot make that kind of archive.".into(),
            "End the name with .zip, .tar.gz, .tar.bz2, .tar.xz, .tar, or .7z.".into(),
        );
        return;
    }
    let destination = folder.join(&name);
    let taken = if fs.is_local() { destination.exists() } else { fs.stat(&destination).is_ok() };
    if taken {
        dialogs::show_error(format!("\u{201c}{name}\u{201d} already exists."), "Choose another name.".into());
        return;
    }
    let Some(app) = app() else { return };
    let (note, cancel) = app.activity.begin_task(&format!("Compressing into \u{201c}{name}\u{201d}\u{2026}"));
    let index = pane.index();
    std::thread::spawn(move || {
        let result = if fs.is_local() {
            compress_local(&paths, &destination, note, &cancel)
        } else {
            compress_remote(&fs, &paths, &folder, &name, note, &cancel)
        };
        DispatchQueue::main().exec_async(move || {
            let Some(app) = self::app() else { return };
            app.activity.remove(note);
            let what = if paths.len() == 1 {
                format!("\u{201c}{}\u{201d}", title_of(&paths[0]))
            } else {
                format!("{} items", paths.len())
            };
            let place = if fs.is_local() { folder.display().to_string() } else { fs.display_path(&folder) };
            match result {
                Ok(()) => {
                    oplog::record(oplog::Kind::Change, format!("Compressed {what} into \u{201c}{name}\u{201d}"), place);
                    app.panes[index].reload_selecting(&[destination]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                    oplog::record(oplog::Kind::Change, format!("Stopped compressing {what}"), place);
                }
                Err(e) => {
                    oplog::record(
                        oplog::Kind::Problem,
                        format!("Could not compress into \u{201c}{name}\u{201d}"),
                        e.to_string(),
                    );
                    dialogs::show_error(format!("\u{201c}{name}\u{201d} could not be made."), e.to_string());
                }
            }
        });
    });
}

/// Shows a line under a task's bar (from any thread), at most ten times a
/// second.
struct TaskDetail {
    note: isize,
    last: Option<std::time::Instant>,
}

impl TaskDetail {
    fn show(&mut self, text: String, force: bool) {
        let now = std::time::Instant::now();
        if !force && self.last.is_some_and(|t| now - t < Duration::from_millis(100)) {
            return;
        }
        self.last = Some(now);
        let note = self.note;
        DispatchQueue::main().exec_async(move || with_activity(|a| a.set_detail(note, &text)));
    }
}

/// The bytes in these files and folders (on the Mac).
fn size_of_tree(paths: &[PathBuf]) -> u64 {
    let mut total = 0;
    let mut stack: Vec<PathBuf> = paths.to_vec();
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            if let Ok(read) = std::fs::read_dir(&path) {
                stack.extend(read.flatten().map(|e| e.path()));
            }
        } else if meta.is_file() {
            total += meta.len();
        }
    }
    total
}

fn compress_local(
    paths: &[PathBuf],
    destination: &Path,
    note: isize,
    cancel: &ops::CancelToken,
) -> std::io::Result<()> {
    let total = size_of_tree(paths);
    let mut detail = TaskDetail { note, last: None };
    vfs::archive::compress(paths, destination, &mut |written| {
        detail.show(format!("{} of {}", crate::fs::format_size(written), crate::fs::format_size(total)), false);
        !cancel.is_cancelled()
    })
}

/// A job's progress as a task's line ("Downloading: 3 MB of 9 MB").
struct TaskObserver {
    detail: TaskDetail,
    verb: &'static str,
}

impl ops::Observer for TaskObserver {
    fn progress(&mut self, progress: &ops::Progress) {
        let text = format!(
            "{}: {} of {}",
            self.verb,
            crate::fs::format_size(progress.done_bytes),
            crate::fs::format_size(progress.total_bytes)
        );
        self.detail.show(text, false);
    }
    fn conflict(&mut self, _conflict: &ops::Conflict) -> ops::Answer {
        ops::Answer { resolution: ops::Resolution::Replace, apply_to_all: true }
    }
    fn error(&mut self, _path: &Path, _error: &std::io::Error) -> ops::ErrorAnswer {
        ops::ErrorAnswer::Stop
    }
}

fn compress_remote(
    fs: &FsRef,
    paths: &[PathBuf],
    folder: &Path,
    name: &str,
    note: isize,
    cancel: &ops::CancelToken,
) -> std::io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let work = preview::cache_dir().join(format!("compress-{}", COUNT.fetch_add(1, Ordering::Relaxed)));
    let items = work.join("items");
    let out = work.join("out");
    std::fs::create_dir_all(&items)?;
    std::fs::create_dir_all(&out)?;
    let result = (|| {
        let stopped = || std::io::Error::new(std::io::ErrorKind::Interrupted, "Stopped");
        let failed = |outcome: &ops::Outcome| {
            outcome.failed.first().map(|(path, e)| std::io::Error::other(format!("{}: {e}", title_of(path))))
        };
        // Download.
        let mut download = Job::between(JobKind::Copy, fs.clone(), paths.to_vec(), vfs::local(), items.clone());
        download.keep_dates = true;
        let mut observer = TaskObserver { detail: TaskDetail { note, last: None }, verb: "Downloading" };
        let outcome = ops::run(&download, &mut observer, cancel.clone());
        if outcome.cancelled || cancel.is_cancelled() {
            return Err(stopped());
        }
        if let Some(error) = failed(&outcome) {
            return Err(error);
        }
        // Compress.
        let local: Vec<PathBuf> = paths.iter().map(|p| items.join(title_of(p))).collect();
        let archive = out.join(name);
        compress_local(&local, &archive, note, cancel)?;
        // Upload beside the items.
        let upload = Job::between(JobKind::Copy, vfs::local(), vec![archive], fs.clone(), folder.to_path_buf());
        let mut observer = TaskObserver { detail: TaskDetail { note, last: None }, verb: "Uploading" };
        let outcome = ops::run(&upload, &mut observer, cancel.clone());
        if outcome.cancelled || cancel.is_cancelled() {
            return Err(stopped());
        }
        match failed(&outcome) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Sets the Finder tags of the selection.
fn edit_tags(pane: &PaneController) {
    let paths = pane.target_paths();
    let Some(first) = paths.first() else { return };
    let current = util::tags_of(first).join(", ");
    let what = if paths.len() == 1 {
        format!("\u{201c}{}\u{201d}", title_of(first))
    } else {
        format!("{} items", paths.len())
    };
    let Some(text) = dialogs::prompt_allow_empty(
        &format!("Tags for {what}"),
        "Separate tags with commas. Finder's colors (Red, Orange, Yellow, Green, Blue, Purple, Gray) work as tags. Leave it empty to remove all tags.",
        "Set Tags",
        &current,
    ) else {
        return;
    };
    let tags: Vec<String> = text.split(',').map(str::trim).filter(|t| !t.is_empty()).map(str::to_string).collect();
    let failed: Vec<String> =
        paths.iter().filter_map(|p| util::set_tags(p, &tags).err().map(|e| format!("{}: {e}", title_of(p)))).collect();
    oplog::record(
        oplog::Kind::Change,
        format!("Tagged {what}: {}", if tags.is_empty() { "no tags".into() } else { tags.join(", ") }),
        "",
    );
    pane.reload();
    if !failed.is_empty() {
        dialogs::show_error("Some tags could not be set.".into(), failed.join("\n"));
    }
}

/// Selects the items matching a pattern.
fn select_matching(pane: &PaneController) {
    thread_local! {
        static LAST: RefCell<String> = const { RefCell::new(String::new()) };
    }
    let last = LAST.with(|l| l.borrow().clone());
    let Some((text, choice)) = dialogs::prompt_with_choice(
        "Select Matching",
        "Names matching: * is any text, ? one character, [a-z] one of these. Separate several with ; (plain text matches names containing it).",
        "Select",
        &last,
        &["Select only these", "Add them to the selection", "Remove them from the selection"],
        0,
    ) else {
        return;
    };
    LAST.with(|l| *l.borrow_mut() = text.clone());
    if pane.select_matching(&crate::fs::glob::Patterns::new(&text), choice == 1, choice == 2) == 0 {
        objc2_app_kit::NSBeep();
    }
}

/// Shows the command palette and runs what is chosen.
fn command_palette() {
    let Some(app) = app() else { return };
    let active = app.active.get();
    #[derive(Clone)]
    enum Action {
        Run(Command),
        Workspace(usize),
        Place(PathBuf),
        Site(String),
        RemoteFolder(String, PathBuf),
        Synclet(usize),
        SmartFolder(usize),
        Recent(usize),
        Tool(usize),
        Preset(usize),
        OpenWith(PathBuf, PathBuf),
    }
    let item = |title: String, detail: &str, value: Action| palette::Item { title, detail: detail.to_string(), value };
    let overrides = app.settings.borrow().shortcuts.clone();
    let mut items: Vec<palette::Item<Action>> = Command::ALL
        .iter()
        .copied()
        .filter(|c| {
            *c != Command::CommandPalette && is_enabled_in(*c, active, !app.panes[active].selected_paths().is_empty())
        })
        .map(|c| {
            let (key, modifiers) = c.effective_shortcut(&overrides);
            palette::Item {
                title: command_title(c, active).trim_end_matches('\u{2026}').to_string(),
                detail: commands::display(&key, modifiers),
                value: Action::Run(c),
            }
        })
        .collect();
    let settings = app.settings.borrow().clone();
    items.extend(settings.workspaces.iter().enumerate().map(|(i, w)| palette::Item {
        title: format!("Open Workspace \u{201c}{}\u{201d}", w.name),
        detail: if i < 9 { format!("\u{2303}\u{2318}{}", i + 1) } else { "Workspace".into() },
        value: Action::Workspace(i),
    }));
    // Places, favorites, and devices.
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let mut places: Vec<(String, PathBuf, &str)> = vec![(title_of(&home), home.clone(), "Folder")];
    for (dir, name) in
        [(dirs::desktop_dir(), "Desktop"), (dirs::document_dir(), "Documents"), (dirs::download_dir(), "Downloads")]
    {
        if let Some(dir) = dir {
            places.push((name.to_string(), dir, "Folder"));
        }
    }
    places.push(("Applications".into(), PathBuf::from("/Applications"), "Folder"));
    places.extend(settings.favorites.iter().map(|f| (title_of(f), f.clone(), "Favorite")));
    places.push(("Macintosh HD".into(), PathBuf::from("/"), "Device"));
    if let Ok(read) = std::fs::read_dir("/Volumes") {
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir()
                && std::fs::read_link(&path).is_err()
                && !entry.file_name().to_string_lossy().starts_with('.')
            {
                places.push((entry.file_name().to_string_lossy().into_owned(), path, "Device"));
            }
        }
    }
    items.extend(places.into_iter().map(|(name, path, kind)| item(format!("Go to {name}"), kind, Action::Place(path))));
    let sites = app.sites.borrow().sites.clone();
    items.extend(sites.iter().map(|s| item(format!("Connect to {}", s.name), "Site", Action::Site(s.id.clone()))));
    items.extend(settings.remote_favorites.iter().filter_map(|f| {
        let site = sites.iter().find(|s| s.id == f.site_id)?;
        let title = format!("Go to {} on {}", title_of(&f.path), site.name);
        Some(item(title, "Server folder", Action::RemoteFolder(f.site_id.clone(), f.path.clone())))
    }));
    items.extend(
        settings
            .synclets
            .iter()
            .enumerate()
            .map(|(i, s)| item(format!("Sync \u{201c}{}\u{201d}", s.name), "Synclet", Action::Synclet(i))),
    );
    items.extend(
        settings
            .smart_folders
            .iter()
            .enumerate()
            .map(|(i, f)| item(format!("Open {}", f.name), "Smart folder", Action::SmartFolder(i))),
    );
    items.extend(
        settings.tools.iter().enumerate().map(|(i, t)| item(format!("Run {}", t.name), "Tool", Action::Tool(i))),
    );
    let recent: Vec<_> = settings.recent_folders.iter().map(recent_title).collect();
    items.extend(
        recent
            .into_iter()
            .enumerate()
            .map(|(i, title)| item(format!("Go to {title}"), "Recent folder", Action::Recent(i))),
    );
    // Presets and apps need a selection.
    let pane = app.panes[active].clone();
    let selection = pane.target_paths();
    if !selection.is_empty() && !pane.fs().read_only() {
        items.extend(
            settings.rename_presets.iter().enumerate().map(|(i, p)| {
                item(format!("Rename with \u{201c}{}\u{201d}", p.name), "Rename preset", Action::Preset(i))
            }),
        );
    }
    if let [file] = selection.as_slice()
        && pane.is_local()
        && !file.is_dir()
    {
        let apps = NSWorkspace::sharedWorkspace().URLsForApplicationsToOpenURL(&file_url(file));
        let mut seen = std::collections::HashSet::new();
        for app_url in apps.iter() {
            let Some(app_path) = util::url_path(&app_url) else { continue };
            let name = app_path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            // Two copies of an app: the first (the one macOS prefers).
            if !seen.insert(name.clone()) || seen.len() > 15 {
                continue;
            }
            items.push(item(format!("Open with {name}"), "App", Action::OpenWith(file.clone(), app_path)));
        }
    }
    let window = app.window.clone();
    drop(app);
    match palette::show(items, Some(&window)) {
        Some(Action::Run(command)) => run_command(command, Some(active)),
        Some(Action::Workspace(index)) => workspaces::open(index),
        Some(Action::Place(path)) => open_in_active_pane(path),
        Some(Action::Site(id)) => site_action(sidebar::SiteAction::Connect, Some(id)),
        Some(Action::RemoteFolder(site, path)) => open_remote_favorite(&site, &path),
        Some(Action::Synclet(index)) => open_synclet(index),
        Some(Action::SmartFolder(index)) => search::open_smart_folder(index),
        Some(Action::Recent(index)) => open_recent(index),
        Some(Action::Tool(index)) => run_tool(index),
        Some(Action::Preset(index)) => rename_with_preset(&pane, index),
        Some(Action::OpenWith(file, app)) => open_with(&file, &app),
        None => {}
    }
}

/// The pane commands go to by default.
pub fn active_index() -> usize {
    app().map_or(0, |a| a.active.get())
}

pub fn preview_pane() -> Option<Retained<preview::PreviewPane>> {
    app().map(|a| a.preview.clone())
}

/// The operation log changed: the preview pane may show it.
pub fn log_changed() {
    if let Some(app) = app() {
        app.preview.log_changed();
    }
}

/// A pane's selection or folder changed: the preview pane follows the
/// active pane.
pub fn selection_changed(index: usize) {
    let Some(app) = app() else { return };
    if index != app.active.get() || app.preview.view().isHidden() {
        return;
    }
    let pane = &app.panes[index];
    let entries = pane.entries_for(&pane.selected_paths());
    app.preview.set_selection(pane.fs(), pane.location(), entries, pane.item_count());
}

/// Hides or shows the sidebar.
fn toggle_sidebar() {
    let Some(app) = app() else { return };
    let view = app.sidebar.view();
    let show = view.isHidden();
    if !show {
        let width = view.frame().size.width;
        update_settings(|s| s.sidebar_width = width);
    }
    view.setHidden(!show);
    if let Some(split) = app.window.contentView().and_then(|v| v.downcast::<NSSplitView>().ok()) {
        split.adjustSubviews();
        if show {
            let width = app.settings.borrow().sidebar_width.clamp(130.0, 320.0);
            split.layoutSubtreeIfNeeded();
            split.setPosition_ofDividerAtIndex(width, 0);
        }
    }
    for pane in &app.panes {
        pane.fit_columns();
    }
    update_settings(|s| s.sidebar_hidden = !show);
}

/// Makes the file lists and sidebar bigger (1), smaller (-1), or normal (0).
fn zoom(step: i32) {
    use crate::settings::TEXT_SIZES;
    let Some(app) = app() else { return };
    let current = app.settings.borrow().appearance.text_size;
    let size = match step {
        0 => 13.0,
        s if s > 0 => TEXT_SIZES.iter().copied().find(|t| *t > current + 0.01).unwrap_or(current),
        _ => TEXT_SIZES.iter().rev().copied().find(|t| *t < current - 0.01).unwrap_or(current),
    };
    update_settings(|s| s.appearance.text_size = size);
    for pane in &app.panes {
        pane.fit_columns();
    }
}

/// Everything with a Finder tag (that Spotlight knows), in the active pane.
pub fn show_tag(name: &'static str) {
    let Some(app) = app() else { return };
    let (note, cancel) = app.activity.begin_task(&format!("Finding items tagged \u{201c}{name}\u{201d}\u{2026}"));
    let index = app.active.get();
    std::thread::spawn(move || {
        let result = crate::fs::search::spotlight_tag(name, &cancel);
        DispatchQueue::main().exec_async(move || {
            let Some(app) = self::app() else { return };
            app.activity.remove(note);
            let paths = match result {
                Ok(paths) => paths,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return,
                Err(e) => {
                    dialogs::show_error(
                        format!("Items tagged \u{201c}{name}\u{201d} could not be found."),
                        e.to_string(),
                    );
                    return;
                }
            };
            let local = vfs::local();
            let entries: Vec<crate::fs::Entry> = paths.iter().filter_map(|p| local.stat(p).ok()).collect();
            if entries.is_empty() {
                dialogs::show_info(
                    format!("Nothing is tagged \u{201c}{name}\u{201d}."),
                    "Items are found through Spotlight, in the folders it indexes.".into(),
                );
                return;
            }
            let title = format!("Tagged \u{201c}{name}\u{201d} ({})", entries.len());
            let results = vfs::results::ResultsFs::new(local, PathBuf::from("/"), title, entries);
            app.panes[index].navigate_to(results, PathBuf::from("/"));
            app.panes[index].focus();
        });
    });
}

/// Hides a pane (the other fills the window) or shows it again.
/// The panes one above the other (or side by side again), split evenly.
fn set_panes_stacked(app: &App, stacked: bool) {
    let Some(split) = unsafe { app.panes[0].view().superview() }.and_then(|v| v.downcast::<NSSplitView>().ok()) else {
        return;
    };
    split.setVertical(!stacked);
    split.adjustSubviews();
    split.layoutSubtreeIfNeeded();
    let size = split.frame().size;
    let length = if stacked { size.height } else { size.width };
    split.setPosition_ofDividerAtIndex((length / 2.0).round(), 0);
}

fn toggle_pane(index: usize) {
    let Some(app) = app() else { return };
    let hide = !app.panes[index].view().isHidden();
    if hide && app.panes[1 - index].view().isHidden() {
        return;
    }
    set_pane_hidden(&app, index, hide);
    update_settings(|s| if index == 0 { s.left.hidden = hide } else { s.right.hidden = hide });
}

fn set_pane_hidden(app: &App, index: usize, hide: bool) {
    let view = app.panes[index].view();
    view.setHidden(hide);
    if let Some(split) = unsafe { view.superview() }.and_then(|v| v.downcast::<NSSplitView>().ok()) {
        split.adjustSubviews();
        if !hide {
            split.layoutSubtreeIfNeeded();
            split.setPosition_ofDividerAtIndex((split.frame().size.width / 2.0).round(), 0);
        }
    }
    if hide && app.active.get() == index {
        set_active(1 - index);
        app.panes[1 - index].focus();
    } else if !hide {
        set_active(index);
        app.panes[index].focus();
    }
    for pane in &app.panes {
        pane.fit_columns();
    }
}

/// Toolbars saved by earlier versions get the buttons added since.
fn add_new_toolbar_items(app: &App) {
    let seen = app.settings.borrow().toolbar_version;
    let Some(toolbar) = app.window.toolbar() else { return };
    let mut latest = seen;
    for (version, commands) in NEW_TOOLBAR_ITEMS.iter().filter(|(v, _)| *v > seen) {
        let present: Vec<String> = toolbar.items().iter().map(|i| i.itemIdentifier().to_string()).collect();
        for (command, at) in *commands {
            if !present.contains(&toolbar_id(*command)) {
                let at = at.unwrap_or(toolbar.items().count() as isize);
                toolbar.insertItemWithItemIdentifier_atIndex(&ns(&toolbar_id(*command)), at);
            }
        }
        latest = latest.max(*version);
    }
    if latest != seen {
        update_settings(|s| s.toolbar_version = latest);
    }
}

/// The identifier of the preview pane's maximum width constraint.
const PREVIEW_MAX_WIDTH: &str = "forkz.preview.max-width";

/// The preview pane's widest: this share of the window.
const PREVIEW_MAX_SHARE: f64 = 0.5;

/// Turns the preview pane's maximum width on or off (the constraint is
/// kept by the split view, as it relates the preview to it).
fn limit_preview_width(split: &NSSplitView, on: bool) {
    for constraint in split.constraints().iter() {
        if constraint.identifier().is_some_and(|i| i.to_string() == PREVIEW_MAX_WIDTH) {
            constraint.setActive(on);
        }
    }
}

thread_local! {
    /// The preview fills the window: whether the sidebar was showing and
    /// the preview's width before, to go back.
    static PREVIEW_EXPANDED: Cell<Option<(bool, f64)>> = const { Cell::new(None) };
}

/// Expands the preview pane to fill the window (hiding the sidebar and
/// the panes), or shrinks it back.
pub fn toggle_preview_expanded() {
    let Some(app) = app() else { return };
    let Some(split) = app.window.contentView().and_then(|v| v.downcast::<NSSplitView>().ok()) else { return };
    let subviews = split.subviews();
    let (Some(sidebar), Some(panes)) = (subviews.iter().next(), subviews.iter().nth(1)) else { return };
    let preview = app.preview.view();
    match PREVIEW_EXPANDED.with(|e| e.take()) {
        None => {
            if preview.isHidden() {
                toggle_preview();
            }
            PREVIEW_EXPANDED.with(|e| e.set(Some((!sidebar.isHidden(), preview.frame().size.width))));
            limit_preview_width(&split, false);
            sidebar.setHidden(true);
            panes.setHidden(true);
            split.adjustSubviews();
            app.preview.set_expanded(true);
            app.window.makeFirstResponder(Some(&preview));
        }
        Some((sidebar_shown, width)) => {
            limit_preview_width(&split, true);
            panes.setHidden(false);
            sidebar.setHidden(!sidebar_shown);
            split.adjustSubviews();
            split.layoutSubtreeIfNeeded();
            let total = split.frame().size.width;
            split.setPosition_ofDividerAtIndex(total - width - split.dividerThickness(), 1);
            if sidebar_shown {
                let sidebar_width = app.settings.borrow().sidebar_width.clamp(130.0, 320.0);
                split.setPosition_ofDividerAtIndex(sidebar_width, 0);
            }
            app.preview.set_expanded(false);
            for pane in &app.panes {
                pane.fit_columns();
            }
            app.panes[app.active.get()].focus();
        }
    }
}

pub fn toggle_preview() {
    // Closing an expanded preview first gives the panes back.
    if PREVIEW_EXPANDED.with(|e| e.get().is_some()) {
        toggle_preview_expanded();
    }
    let Some(app) = app() else { return };
    let view = app.preview.view();
    let show = view.isHidden();
    view.setHidden(!show);
    if let Some(split) = app.window.contentView().and_then(|v| v.downcast::<NSSplitView>().ok()) {
        split.adjustSubviews();
        if show {
            split.layoutSubtreeIfNeeded();
            let total = split.frame().size.width;
            let width = app.settings.borrow().preview.width.clamp(220.0, (total * PREVIEW_MAX_SHARE).max(220.0));
            split.setPosition_ofDividerAtIndex(total - width - split.dividerThickness(), 1);
            split.layoutSubtreeIfNeeded();
        }
    }
    // The panes changed width.
    for pane in &app.panes {
        pane.fit_columns();
    }
    if show {
        selection_changed(app.active.get());
    } else {
        let width = view.frame().size.width;
        update_settings(|s| s.preview.width = width);
    }
    update_settings(|s| s.preview.visible = show);
}

/// The pane that commands go to by default.
pub fn set_active(index: usize) {
    let Some(app) = app() else { return };
    app.active.set(index);
    app.panes[index].set_active(true);
    app.panes[1 - index].set_active(false);
    selection_changed(index);
    update_title(&app);
}

fn update_title(app: &App) {
    let pane = &app.panes[app.active.get()];
    let location = pane.location();
    let fs = pane.fs();
    if let Some(title) = fs.title_for(&location) {
        app.window.setTitle(&ns(&title));
        app.window.setRepresentedURL(None);
    } else if fs.is_local() {
        app.window.setTitle(&ns(&title_of(&location)));
        app.window.setRepresentedURL(Some(&file_url(&location)));
    } else if fs.key() == "network:" {
        app.window.setTitle(&ns("Network"));
        app.window.setRepresentedURL(None);
    } else {
        let folder = if location.parent().is_none() { "/".to_string() } else { title_of(&location) };
        app.window.setTitle(&ns(&format!("{folder} ({})", fs.label())));
        app.window.setRepresentedURL(None);
    }
    app.sidebar.highlight(current_place(app));
}

fn current_place(app: &App) -> sidebar::Current {
    let pane = &app.panes[app.active.get()];
    let fs = pane.fs();
    // Search and tag results are not the folder they are listed in.
    if fs.title_for(&pane.location()).is_some() {
        return sidebar::Current::Other;
    }
    if fs.is_local() {
        return sidebar::Current::Local(pane.location());
    }
    if fs.key() == "network:" {
        return sidebar::Current::Network;
    }
    let connections = app.connections.borrow();
    match connections.iter().find(|(_, c)| vfs::same(c, &fs)) {
        Some((id, _)) => sidebar::Current::Site(id.clone(), pane.location()),
        None => sidebar::Current::Other,
    }
}

/// A pane went to another folder.
pub fn location_changed(index: usize) {
    follow_sync_browsing(index);
    let Some(app) = app() else { return };
    let pane = &app.panes[index];
    {
        // Folders on the Mac are remembered (server tabs are not).
        let tabs: Vec<(FsRef, PathBuf)> = pane.tab_locations();
        let active = pane.active_tab();
        let local: Vec<PathBuf> = tabs.iter().filter(|(fs, _)| fs.is_local()).map(|(_, p)| p.clone()).collect();
        let active_local = tabs[..active.min(tabs.len())].iter().filter(|(fs, _)| fs.is_local()).count();
        let mut settings = app.settings.borrow_mut();
        let saved = if index == 0 { &mut settings.left } else { &mut settings.right };
        if pane.is_local() {
            saved.location = pane.location();
        }
        saved.tabs = if local.len() > 1 { local } else { Vec::new() };
        saved.active_tab = if pane.is_local() { active_local } else { 0 };
        settings.save();
    }
    if index == app.active.get() {
        update_title(&app);
    }
    watch_locations(&app);
}

/// A background listing for a pane arrived.
fn listing_arrived(index: usize, generation: u64, result: std::io::Result<Vec<crate::fs::Entry>>) {
    if let Some(app) = app() {
        app.panes[index].listing_arrived(generation, result);
    }
}

fn pane_sort_changed(index: usize, sort: Sort) {
    let Some(app) = app() else { return };
    let mut settings = app.settings.borrow_mut();
    if index == 0 {
        settings.left.sort = sort;
    } else {
        settings.right.sort = sort;
    }
    settings.save();
}

/// Shows a folder in the active pane, with items selected.
fn navigate_active_pane(fs: FsRef, folder: PathBuf, select: &[PathBuf]) {
    let Some(app) = app() else { return };
    let pane = app.panes[app.active.get()].clone();
    pane.navigate_selecting(fs, folder, select);
    app.window.makeKeyAndOrderFront(None);
    pane.focus();
}

/// Shows a file selected in its folder, in the active pane.
fn show_in_active_pane(folder: PathBuf, file: &Path) {
    let Some(app) = app() else { return };
    let pane = app.panes[app.active.get()].clone();
    pane.navigate_selecting(vfs::local(), folder, &[file.to_path_buf()]);
    app.window.makeKeyAndOrderFront(None);
    pane.focus();
}

fn open_in_active_pane(path: PathBuf) {
    let Some(app) = app() else { return };
    let pane = app.panes[app.active.get()].clone();
    pane.navigate_to(vfs::local(), path);
    pane.focus();
}

fn add_favorite(path: PathBuf) {
    let Some(app) = app() else { return };
    {
        let mut settings = app.settings.borrow_mut();
        if settings.favorites.contains(&path) {
            return;
        }
        settings.favorites.push(path);
        settings.save();
    }
    reload_sidebar();
}

fn remove_favorite(path: &Path) {
    let Some(app) = app() else { return };
    {
        let mut settings = app.settings.borrow_mut();
        settings.favorites.retain(|p| p != path);
        settings.save();
    }
    reload_sidebar();
}

/// The sidebar shows the active pane's place again.
pub fn refresh_sidebar_highlight() {
    if let Some(app) = app() {
        app.sidebar.highlight(current_place(&app));
    }
}

pub fn reload_sidebar() {
    let Some(app) = app() else { return };
    let (favorites, remote) = {
        let settings = app.settings.borrow();
        (settings.favorites.clone(), settings.remote_favorites.clone())
    };
    let sites = app.sites.borrow().sites.clone();
    let connected: Vec<String> = app.connections.borrow().iter().map(|(id, _)| id.clone()).collect();
    let synclets: Vec<String> = app.settings.borrow().synclets.iter().map(|s| s.name.clone()).collect();
    let presets: Vec<String> = app.settings.borrow().rename_presets.iter().map(|p| p.name.clone()).collect();
    let smart: Vec<String> = app.settings.borrow().smart_folders.iter().map(|f| f.name.clone()).collect();
    let order = app.settings.borrow().sidebar_order.clone();
    let saved = sidebar::Saved { synclets: &synclets, presets: &presets, smart_folders: &smart, order: &order };
    app.sidebar.reload(&favorites, &remote, &sites, &connected, saved, current_place(&app));
}

// Sites and connections.

/// A sidebar site command.
fn site_action(action: sidebar::SiteAction, site_id: Option<String>) {
    use sidebar::SiteAction;
    tracing::debug!("site action {action:?} on {site_id:?}");
    let Some(app) = app() else { return };
    let site = site_id.as_deref().and_then(|id| app.sites.borrow().get(id).cloned());
    let active = app.active.get();
    match (action, site) {
        (SiteAction::Connect, Some(site)) => connect_site(site, active, None, false),
        (SiteAction::ConnectInOther, Some(site)) => connect_site(site, 1 - active, None, false),
        (SiteAction::Edit, Some(site)) => {
            let saved = crate::sites::keychain::password(&site.id);
            if let Some(form) = site_form::show(Some(&site), saved.clone()) {
                if form.save {
                    match &form.password {
                        Some(password) => crate::sites::keychain::save_password(&form.site.id, password),
                        None => crate::sites::keychain::delete_password(&form.site.id),
                    }
                }
                app.sites.borrow_mut().upsert(form.site);
                app.sites.borrow().save();
                reload_sidebar();
            }
        }
        (SiteAction::Delete, Some(site)) => {
            let message = format!("Delete the site \u{201c}{}\u{201d}?", site.name);
            if dialogs::confirm(&message, "Its saved password is removed from the Keychain too.", "Delete", true) {
                crate::sites::keychain::delete_password(&site.id);
                app.sites.borrow_mut().remove(&site.id);
                app.sites.borrow().save();
                reload_sidebar();
            }
        }
        (SiteAction::Disconnect, Some(site)) => {
            let key = app.connections.borrow().iter().find(|(id, _)| *id == site.id).map(|(_, fs)| fs.key());
            if let Some(key) = key {
                disconnect(&key);
            }
        }
        (SiteAction::New, _) => run_command(Command::Connect, None),
        (SiteAction::Import, _) => import_filezilla(),
        _ => {}
    }
}

/// After the Connect form: save the site if asked, then connect.
fn connect_from_form(form: site_form::FormResult, pane: usize) {
    let Some(app) = app() else { return };
    if form.save {
        app.sites.borrow_mut().upsert(form.site.clone());
        app.sites.borrow().save();
        reload_sidebar();
    }
    connect_site(form.site, pane, form.password, form.save);
}

/// Asks the user from the connecting thread (host keys, passwords).
struct UiPrompter {
    offer_save: bool,
    /// The user chose to save the password they typed.
    save: std::sync::Mutex<bool>,
}

impl vfs::sftp::Prompter for UiPrompter {
    fn host_key(
        &self,
        host: &str,
        port: u16,
        key_type: &str,
        fingerprint: &str,
        status: vfs::known_hosts::Status,
    ) -> bool {
        tracing::debug!("asking about the host key of {host}:{port} ({status:?})");
        let (reply, answer) = std::sync::mpsc::channel();
        let (host, key_type, fingerprint) = (host.to_string(), key_type.to_string(), fingerprint.to_string());
        DispatchQueue::main()
            .exec_async(move || dialogs::ask_host_key(host, port, key_type, fingerprint, status, reply));
        answer.recv().unwrap_or(false)
    }

    fn secret(&self, message: &str, informative: &str) -> Option<String> {
        let (reply, answer) = std::sync::mpsc::channel();
        let (message, informative, offer) = (message.to_string(), informative.to_string(), self.offer_save);
        DispatchQueue::main().exec_async(move || dialogs::ask_secret(message, informative, offer, reply));
        let (secret, save) = answer.recv().ok().flatten()?;
        *self.save.lock().unwrap_or_else(|e| e.into_inner()) = save;
        Some(secret)
    }
}

/// Connects to a site in a pane (reusing an open connection), then shows
/// its remote folder, and its local folder in the other pane.
fn connect_site(site: crate::sites::Site, pane_index: usize, password: Option<String>, save_password: bool) {
    tracing::debug!("connecting to {} in pane {pane_index}", site.address());
    let Some(app) = app() else { return };
    let existing = app.connections.borrow().iter().find(|(id, _)| *id == site.id).map(|(_, fs)| fs.clone());
    if let Some(fs) = existing {
        show_site(&site, fs, pane_index);
        workspaces::connection_done();
        return;
    }
    let (timeout, extra_keys) = {
        let settings = app.settings.borrow();
        (Duration::from_secs(settings.sftp.timeout_seconds.max(5)), settings.sftp.key_files.clone())
    };
    let saved_site = app.sites.borrow().get(&site.id).is_some();
    let activity_id = app.activity.begin_note(&format!("Connecting to {}\u{2026}", site.address()));
    std::thread::spawn(move || {
        // The Keychain is read here, off the main thread.
        // Only saved sites have a password in the Keychain.
        let password =
            if saved_site { password.or_else(|| crate::sites::keychain::password(&site.id)) } else { password };
        let typed = password.is_some();
        let prompter = UiPrompter { offer_save: saved_site, save: std::sync::Mutex::new(false) };
        let result = open_connection(&site, password, timeout, extra_keys, &prompter);
        let save_typed = *prompter.save.lock().unwrap_or_else(|e| e.into_inner());
        DispatchQueue::main().exec_async(move || {
            let Some(app) = self::app() else { return };
            app.activity.remove(activity_id);
            match result {
                Ok((fs, secret)) => {
                    // Keep what worked if asked (the form's checkbox, or the
                    // password prompt's).
                    if let Some(secret) = secret
                        && ((save_password && typed) || save_typed)
                        && app.sites.borrow().get(&site.id).is_some()
                    {
                        crate::sites::keychain::save_password(&site.id, &secret);
                    }
                    oplog::record(oplog::Kind::Connection, format!("Connected to {}", site.address()), "");
                    app.connections.borrow_mut().push((site.id.clone(), fs.clone()));
                    reload_sidebar();
                    show_site(&site, fs, pane_index);
                    workspaces::connection_done();
                }
                Err(e) => {
                    // The site stays clickable: show what the pane shows.
                    update_title(&app);
                    workspaces::connection_done();
                    if e.kind() != std::io::ErrorKind::Interrupted {
                        let message = format!("Could not connect to {}", site.address());
                        oplog::record(oplog::Kind::Problem, message.clone(), e.to_string());
                        dialogs::show_error(format!("{message}."), e.to_string());
                    }
                }
            }
        });
    });
}

/// What to mount for a share site: the server and the share (the first
/// part of the remote folder).
fn share_params(site: &crate::sites::Site, password: Option<String>) -> vfs::mount::ShareParams {
    let scheme = site.protocol.scheme();
    let share = site.remote_dir.clone().unwrap_or_default();
    // NFS exports are paths; SMB and AFP mount the share, not a folder in it.
    let share =
        if scheme == "nfs" { share } else { share.trim_matches('/').split('/').next().unwrap_or("").to_string() };
    vfs::mount::ShareParams {
        scheme,
        host: site.host.clone(),
        port: (site.port != site.protocol.default_port()).then_some(site.port),
        user: site.user.clone(),
        password,
        share,
    }
}

/// Connects to a site with its protocol (off the main thread). Returns the
/// file system and the password or key that worked.
fn open_connection(
    site: &crate::sites::Site,
    password: Option<String>,
    timeout: Duration,
    extra_keys: Vec<PathBuf>,
    prompter: &UiPrompter,
) -> std::io::Result<(FsRef, Option<String>)> {
    use crate::sites::Protocol;
    let user = if site.user.is_empty() && !matches!(site.protocol, Protocol::S3 | Protocol::S3Http) {
        std::env::var("USER").unwrap_or_default()
    } else {
        site.user.clone()
    };
    match site.protocol {
        Protocol::Sftp => {
            let ssh = vfs::ssh_config::SshConfig::load().resolve(&site.host);
            let params = site.params(&ssh, password, timeout, extra_keys);
            tracing::debug!("connecting with {params:?} (from ~/.ssh/config: {ssh:?})");
            let fs = vfs::sftp::connect(params, prompter)?;
            let secret = fs.secret().map(str::to_string);
            Ok((fs, secret))
        }
        Protocol::Ftp | Protocol::Ftps | Protocol::FtpPlain => {
            let security = match site.protocol {
                Protocol::Ftps => vfs::ftp::Security::Tls,
                Protocol::FtpPlain => vfs::ftp::Security::Plain,
                _ => vfs::ftp::Security::TlsIfAvailable,
            };
            // FTP servers expect "anonymous" when there is no account.
            let user = if site.user.is_empty() { "anonymous".to_string() } else { user };
            let params =
                vfs::ftp::FtpParams { host: site.host.clone(), port: site.port, user, password, security, timeout };
            let fs = vfs::ftp::connect(params, prompter)?;
            let secret = fs.secret().map(str::to_string);
            Ok((fs, secret))
        }
        Protocol::Webdav | Protocol::Webdavs => {
            let root = site.remote_dir.clone().unwrap_or_else(|| "/".to_string());
            let params = vfs::webdav::DavParams {
                host: site.host.clone(),
                port: site.port,
                secure: site.protocol == Protocol::Webdavs,
                user: site.user.clone(),
                password,
                root,
                timeout,
            };
            let fs = vfs::webdav::connect(params, prompter)?;
            let secret = fs.secret().map(str::to_string);
            Ok((fs, secret))
        }
        Protocol::Smb | Protocol::Afp | Protocol::Nfs => {
            let params = share_params(site, password);
            let mountpoint = vfs::mount::mount(&params)?;
            let fs: FsRef = vfs::mount::MountFs::new(mountpoint, &params);
            Ok((fs, params.password))
        }
        Protocol::S3 | Protocol::S3Http => {
            let params = vfs::s3::S3Params {
                host: site.host.clone(),
                port: site.port,
                secure: site.protocol == Protocol::S3,
                access_key: site.user.clone(),
                secret_key: password,
                region: site.s3_region(),
                timeout,
            };
            let fs = vfs::s3::connect(params, prompter)?;
            let secret = fs.secret().map(str::to_string);
            Ok((fs, secret))
        }
    }
}

fn show_site(site: &crate::sites::Site, fs: FsRef, pane_index: usize) {
    let Some(app) = app() else { return };
    // For WebDAV the remote folder is the server's WebDAV root (see
    // open_connection), so the site opens at the top of it.
    let webdav = matches!(site.protocol, crate::sites::Protocol::Webdav | crate::sites::Protocol::Webdavs);
    let folder = if site.protocol.is_share() && site.protocol != crate::sites::Protocol::Nfs {
        // The share is mounted; the rest of the remote folder is inside it.
        let inside: PathBuf = site.remote_dir.as_deref().unwrap_or("").trim_matches('/').split('/').skip(1).collect();
        let folder = fs.home().join(inside);
        if folder.is_dir() { folder } else { fs.home() }
    } else if site.protocol.is_share() {
        fs.home()
    } else {
        site.remote_dir.as_ref().filter(|_| !webdav).map(PathBuf::from).unwrap_or_else(|| fs.home())
    };
    let pane = app.panes[pane_index].clone();
    pane.navigate_to(fs, folder);
    pane.focus();
    if let Some(local) = site.local_dir.as_ref().filter(|d| d.is_dir()) {
        app.panes[1 - pane_index].navigate_to(vfs::local(), local.clone());
    }
    update_title(&app);
}

/// Gives a pane the keyboard focus (its list, or its column view).
fn focus_pane(index: usize) {
    if let Some(pane) = app().and_then(|a| a.panes.get(index).cloned()) {
        pane.focus();
    }
}

/// `git status` for a pane's Git column.
fn git_status_arrived(index: usize, statuses: crate::fs::git::Statuses) {
    if let Some(pane) = app().and_then(|a| a.panes.get(index).cloned()) {
        pane.git_arrived(statuses);
    }
}

/// The icon view's selection colors, after the focus moved.
fn refresh_icon_highlights(index: usize) {
    if let Some(icons) = app().and_then(|a| a.panes.get(index).and_then(|p| p.icon_view())) {
        icons.refresh_highlights();
    }
}

/// The column view shows the pane's folder (after the window appears).
fn sync_pane_columns(index: usize) {
    if let Some(pane) = app().and_then(|a| a.panes.get(index).cloned()) {
        pane.sync_columns();
    }
}

/// A server folder listed for a pane's column view.
fn column_listing_arrived(index: usize, folder: PathBuf, list: Vec<crate::fs::Entry>) {
    if let Some(columns) = app().and_then(|a| a.panes.get(index).and_then(|p| p.column_browser())) {
        columns.listing_arrived(folder, list);
    }
}

/// Shows folders and files sent to forkz (opened with it, or revealed as
/// the folder viewer) in the active pane and new tabs; files are selected.
fn show_paths(paths: &[PathBuf], reveal: bool) {
    let Some(app) = app() else { return };
    let pane = app.panes[app.active.get()].clone();
    for (index, show) in file_viewer::plan(paths, |p| p.is_dir(), reveal).into_iter().enumerate() {
        if index > 0 {
            pane.new_tab(vfs::local(), pane.location());
        }
        match show {
            file_viewer::Show::Folder(folder) => pane.navigate_to(vfs::local(), folder),
            file_viewer::Show::Select(folder, files) => pane.navigate_selecting(vfs::local(), folder, &files),
        }
    }
    if !paths.is_empty() {
        NSApplication::sharedApplication(mtm()).activate();
        app.window.makeKeyAndOrderFront(None);
    }
}

/// Opens a folder or a server address (sftp://, ftp://, ftps://, dav://,
/// davs://, s3://) in the active pane.
/// Connects to a server address (from Network), as Go to Folder does.
pub fn connect_address(address: &str) {
    open_target(address);
}

/// Network in the sidebar: the servers on the local network, in the
/// active pane.
pub fn open_network() {
    let Some(app) = app() else { return };
    let pane = app.panes[app.active.get()].clone();
    pane.navigate_to(vfs::network::new(), PathBuf::from("/"));
    pane.focus();
}

fn open_target(target: &str) {
    if target.starts_with("forkz:") {
        // Once the app runs: at launch, its window is not up yet.
        let link = target.to_string();
        DispatchQueue::main().exec_async(move || open_link(&link));
        return;
    }
    let Some(app) = app() else { return };
    let protocol = target.split_once("://").and_then(|(scheme, _)| crate::sites::Protocol::from_scheme(scheme));
    let (Some(protocol), Some((_, address))) = (protocol, target.split_once("://")) else {
        app.panes[app.active.get()].navigate_to(vfs::local(), PathBuf::from(target));
        return;
    };
    let (authority, path) = match address.find('/') {
        Some(slash) => (&address[..slash], Some(vfs::http::decode(&address[slash..]))),
        None => (address, None),
    };
    let (user, host_port) = match authority.rsplit_once('@') {
        Some((user, rest)) => (vfs::http::decode(user), rest),
        None => (String::new(), authority),
    };
    let default_port = protocol.default_port();
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => (host.to_string(), port.parse().unwrap_or(default_port)),
        _ => (host_port.to_string(), default_port),
    };
    let mut wanted = crate::sites::Site::new(&host, port, &user);
    wanted.protocol = protocol;
    // A saved site for the same account brings its login settings.
    let same_family = |p: crate::sites::Protocol| p.scheme() == protocol.scheme() || p == protocol;
    let saved = app
        .sites
        .borrow()
        .sites
        .iter()
        .find(|s| {
            same_family(s.protocol)
                && s.host.eq_ignore_ascii_case(&host)
                && s.port == port
                && (user.is_empty() || s.user == user)
        })
        .cloned();
    // Otherwise: keys from the agent or ~/.ssh, then a password prompt.
    let mut site = saved.unwrap_or(wanted);
    if path.is_some() {
        site.remote_dir = path;
    }
    connect_site(site, app.active.get(), None, false);
}

/// Shows an archive's contents in a pane, like a folder (`select`: an item
/// inside to open its folder at). Archives on servers or in other archives
/// are fetched to a temporary copy first.
pub fn open_archive(index: usize, fs: FsRef, path: PathBuf, select: Option<PathBuf>) {
    let Some(app) = app() else { return };
    let name = title_of(&path);
    let note = app.activity.begin_note(&format!("Opening \u{201c}{name}\u{201d}\u{2026}"));
    std::thread::spawn(move || {
        let result = (|| {
            let local = if fs.is_local() { path.clone() } else { fetch_archive(&fs, &path)? };
            vfs::archive::ArchiveFs::open(&local, Some((fs.clone(), path.clone())))
        })();
        DispatchQueue::main().exec_async(move || {
            let Some(app) = self::app() else { return };
            app.activity.remove(note);
            match result {
                Ok(archive) => {
                    let pane = &app.panes[index];
                    let folder =
                        select.as_ref().and_then(|s| s.parent()).map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
                    let select: Vec<PathBuf> = select.into_iter().collect();
                    pane.navigate_selecting(archive, folder, &select);
                    pane.focus();
                }
                Err(e) => dialogs::show_error(
                    format!("\u{201c}{name}\u{201d} cannot be opened as an archive."),
                    e.to_string(),
                ),
            }
        });
    });
}

/// A temporary copy of an archive that is not a file on this Mac. The
/// copies go when forkz next starts (see `preview::clean_cache`).
fn fetch_archive(fs: &FsRef, path: &Path) -> std::io::Result<PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let folder = preview::cache_dir().join(format!("archive-{}", COUNT.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&folder)?;
    let copy = folder.join(title_of(path));
    let mut reader = fs.open_read(path)?;
    std::io::copy(&mut reader, &mut std::fs::File::create(&copy)?)?;
    Ok(copy)
}

/// Leaves an archive for the folder it is in, selecting it.
fn close_archive(pane: &PaneController) {
    let fs = pane.fs();
    let Some((outer, file)) = fs.outside() else { return };
    let folder = file.parent().map_or_else(|| outer.home(), Path::to_path_buf);
    pane.navigate_selecting(outer, folder, &[file]);
}

/// Closes a connection; panes showing it go back to the Mac.
fn disconnect(key: &str) {
    let Some(app) = app() else { return };
    if let Some(fs) = app.connections.borrow().iter().find(|(_, fs)| fs.key() == key).map(|(_, fs)| fs.clone()) {
        oplog::record(oplog::Kind::Connection, format!("Disconnected from {}", fs.label()), "");
    }
    app.connections.borrow_mut().retain(|(_, fs)| fs.key() != key);
    for pane in &app.panes {
        pane.leave(key);
    }
    update_title(&app);
    reload_sidebar();
    // A mounted share is unmounted (once nothing shows it).
    if let Some(mountpoint) = key.strip_prefix("mount:").map(PathBuf::from) {
        std::thread::spawn(move || {
            let result = vfs::mount::unmount(&mountpoint);
            DispatchQueue::main().exec_async(move || {
                if let Err(e) = result {
                    dialogs::show_error(
                        format!("\u{201c}{}\u{201d} could not be unmounted.", title_of(&mountpoint)),
                        e.to_string(),
                    );
                }
            });
        });
    }
}

/// Imports sites, passwords, trusted host keys, and settings from FileZilla.
fn import_filezilla() {
    let Some(app) = app() else { return };
    let Some(dir) = crate::sites::filezilla::default_dir().filter(|d| d.is_dir()) else {
        dialogs::show_error(
            "No FileZilla settings were found.".into(),
            "forkz looks in ~/.config/filezilla, where FileZilla keeps its Site Manager.".into(),
        );
        return;
    };
    let import = match crate::sites::filezilla::read(&dir) {
        Ok(import) => import,
        Err(e) => {
            dialogs::show_error("FileZilla\u{2019}s settings could not be read.".into(), e.to_string());
            return;
        }
    };
    let new_sites: Vec<_> = {
        let store = app.sites.borrow();
        import
            .sites
            .iter()
            .filter(|(site, _)| !store.sites.iter().any(|s| s.same_account(site) && s.name == site.name))
            .cloned()
            .collect()
    };
    let duplicates = import.sites.len() - new_sites.len();
    let passwords = new_sites.iter().filter(|(_, p)| p.is_some()).count();
    let settings = &import.settings;
    let mut found = vec![format!(
        "{} SFTP site{} ({} with a saved password)",
        new_sites.len(),
        if new_sites.len() == 1 { "" } else { "s" },
        passwords
    )];
    if duplicates > 0 {
        found.push(format!("{duplicates} already in forkz (left as they are)"));
    }
    if !import.host_keys.is_empty() {
        found.push(format!(
            "{} trusted host key{}",
            import.host_keys.len(),
            if import.host_keys.len() == 1 { "" } else { "s" }
        ));
    }
    let mut setting_lines = Vec::new();
    if let Some(show) = settings.show_hidden {
        setting_lines.push(format!("Show hidden files: {}", if show { "on" } else { "off" }));
    }
    if let Some(keep) = settings.preserve_timestamps {
        setting_lines.push(format!("Keep modification dates on transfers: {}", if keep { "on" } else { "off" }));
    }
    let describe = |r: Option<Resolution>| match r {
        None => "ask",
        Some(Resolution::Replace) => "replace",
        Some(Resolution::ReplaceOlder) => "replace if older",
        Some(Resolution::KeepBoth) => "keep both",
        Some(Resolution::Skip) => "skip",
        Some(_) => "ask",
    };
    if let Some(action) = settings.download_conflict {
        setting_lines.push(format!("When a downloaded file exists: {}", describe(action)));
    }
    if let Some(action) = settings.upload_conflict {
        setting_lines.push(format!("When an uploaded file exists: {}", describe(action)));
    }
    if let Some(timeout) = settings.timeout_seconds {
        setting_lines.push(format!("Connection timeout: {timeout} seconds"));
    }
    if !settings.key_files.is_empty() {
        setting_lines.push(format!("{} extra key file(s)", settings.key_files.len()));
    }
    if let Some(n) = settings.max_transfers {
        setting_lines.push(format!("Simultaneous transfers: {n}"));
    }
    let mut detail = format!("Found in {}:\n\n\u{2022} {}", dir.display(), found.join("\n\u{2022} "));
    if !setting_lines.is_empty() {
        detail.push_str(&format!("\n\nSettings:\n\u{2022} {}", setting_lines.join("\n\u{2022} ")));
    }
    if !import.notes.is_empty() {
        detail.push_str(&format!("\n\nNot imported:\n\u{2022} {}", import.notes.join("\n\u{2022} ")));
    }
    detail.push_str("\n\nPasswords go to your login Keychain. FileZilla\u{2019}s files are not changed.");
    if new_sites.is_empty() && import.host_keys.is_empty() && setting_lines.is_empty() {
        dialogs::show_error("Nothing to import from FileZilla.".into(), detail);
        return;
    }
    if !dialogs::confirm("Import from FileZilla?", &detail, "Import", false) {
        return;
    }

    for (site, password) in &new_sites {
        if let Some(password) = password {
            crate::sites::keychain::save_password(&site.id, password);
        }
        app.sites.borrow_mut().upsert(site.clone());
    }
    app.sites.borrow().save();
    let mut key_errors = 0;
    for (host, port, key) in &import.host_keys {
        if vfs::known_hosts::trust(host, *port, key).is_err() {
            key_errors += 1;
        }
    }
    {
        let mut current = app.settings.borrow_mut();
        if let Some(show) = settings.show_hidden {
            current.show_hidden = show;
        }
        if let Some(keep) = settings.preserve_timestamps {
            current.transfers.preserve_timestamps = keep;
        }
        if let Some(action) = settings.download_conflict {
            current.transfers.download_conflict = action;
        }
        if let Some(action) = settings.upload_conflict {
            current.transfers.upload_conflict = action;
        }
        if let Some(timeout) = settings.timeout_seconds {
            current.sftp.timeout_seconds = timeout;
        }
        if let Some(n) = settings.max_transfers {
            current.transfers.max_transfers = n;
        }
        for key in &settings.key_files {
            if !current.sftp.key_files.contains(key) {
                current.sftp.key_files.push(key.clone());
            }
        }
        current.save();
    }
    let show = app.settings.borrow().show_hidden;
    for pane in &app.panes {
        pane.set_show_hidden(show);
    }
    app.activity.set_limit(app.settings.borrow().transfers.max_transfers);
    reload_sidebar();
    if key_errors > 0 {
        dialogs::show_error(format!("{key_errors} host key(s) could not be saved."), String::new());
    }
}

/// Watches the panes' local folders with FSEvents and reloads a pane when
/// its folder changes. (Remote folders are reloaded after jobs.)
fn start_watching() {
    let Some(app) = app() else { return };
    let watcher = notify::recommended_watcher(|event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        DispatchQueue::main().exec_async(move || folder_changed(&event.paths));
    });
    match watcher {
        Ok(watcher) => *app.watcher.borrow_mut() = Some(watcher),
        Err(e) => tracing::warn!("folder watching is off: {e}"),
    }
    watch_locations(&app);
}

fn watch_locations(app: &App) {
    let mut watcher = app.watcher.borrow_mut();
    let Some(watcher) = watcher.as_mut() else { return };
    let wanted: Vec<PathBuf> = {
        let mut wanted: Vec<PathBuf> = app.panes.iter().filter(|p| p.is_local()).map(|p| p.location()).collect();
        wanted.dedup();
        wanted
    };
    let mut watched = app.watched.borrow_mut();
    for path in watched.iter().filter(|p| !wanted.contains(p)) {
        let _ = watcher.unwatch(path);
    }
    for path in wanted.iter().filter(|p| !watched.contains(p)) {
        if let Err(e) = watcher.watch(path, RecursiveMode::NonRecursive) {
            tracing::debug!("cannot watch {}: {e}", path.display());
        }
    }
    *watched = wanted;
}

fn folder_changed(paths: &[PathBuf]) {
    let Some(app) = app() else { return };
    for (index, pane) in app.panes.iter().enumerate() {
        if !pane.is_local() {
            continue;
        }
        let location = pane.location();
        // Events name real paths (/private/tmp/... for /tmp/...): compare
        // those too.
        let real = std::fs::canonicalize(&location).unwrap_or_else(|_| location.clone());
        let affected = paths.iter().any(|p| {
            p == &location || p.parent() == Some(location.as_path()) || p == &real || p.parent() == Some(real.as_path())
        });
        if !affected || app.reload_pending[index].replace(true) {
            continue;
        }
        let when = DispatchTime::try_from(Duration::from_millis(150)).unwrap_or(DispatchTime::NOW);
        let _ = DispatchQueue::main().after(when, move || {
            if let Some(current) = self::app() {
                current.reload_pending[index].set(false);
                current.panes[index].reload();
            }
        });
    }
}
