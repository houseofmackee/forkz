//! The app: one window with a sidebar, two file panes, and running jobs at
//! the bottom; the menu bar and toolbar; and folder watching.

mod activity;
pub mod commands;
mod dialogs;
mod multi_rename;
mod oplog;
mod pane_view;
mod permissions;
mod preview;
mod promises;
mod remote_edit;
mod settings_window;
mod sidebar;
mod site_form;
mod tools;
mod util;

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
    NSControlStateValueOn, NSMenu, NSMenuItem, NSMenuItemValidation, NSPasteboard, NSPasteboardTypeString, NSSplitView,
    NSSplitViewDividerStyle, NSStackView, NSToolbar, NSToolbarDelegate, NSToolbarDisplayMode,
    NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem, NSToolbarItemValidation, NSToolbarSpaceItemIdentifier,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask, NSWindowToolbarStyle, NSWorkspace,
    NSWorkspaceDidMountNotification, NSWorkspaceDidUnmountNotification,
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

        /// Commands > Simultaneous Transfers.
        #[unsafe(method(transferLimit:))]
        fn transfer_limit(&self, sender: &NSMenuItem) {
            set_transfer_limit((sender.tag() - TRANSFER_LIMIT_TAG) as usize);
        }

        #[unsafe(method(volumesChanged:))]
        fn volumes_changed(&self, _notification: &NSNotification) {
            reload_sidebar();
        }
    }

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn should_terminate_after_last_window_closed(&self, _app: &NSApplication) -> bool {
            true
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            if let Some(app) = app() {
                app.settings.borrow().save();
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
}

/// A command's title for a pane: "Delete..." instead of "Move to Trash" on
/// a server, "Upload" or "Download" when copying between the Mac and a
/// server, and so on.
pub fn command_title(command: Command, pane: usize) -> String {
    let Some(app) = app() else { return command.title().into() };
    let here = app.panes[pane].is_local();
    let there = app.panes[1 - pane].is_local();
    let title = match command {
        Command::Trash if !here => "Delete\u{2026}",
        Command::CopyToOther if here && !there => "Upload to Other Pane",
        Command::CopyToOther if !here && there => "Download to Other Pane",
        Command::PasteFiles if !here => "Upload Copied Items",
        Command::OpenInTerminal if !here => "Open SSH Session in Terminal",
        Command::CopyPath if !here => "Copy Server Link",
        Command::CloseTab if app.panes[pane].tab_count() <= 1 => "Close Window",
        Command::TogglePreview if !app.preview.view().isHidden() => "Hide Preview Pane",
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
        ]
    } else {
        let mut ids: Vec<_> = TOOLBAR.iter().map(|c| id(*c)).collect();
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
    let settings = Settings::load();
    ns_app.setMainMenu(Some(&main_menu(mtm, &delegate, &settings.shortcuts)));
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
    apply_appearance(&app);
    APP.with(|a| a.set(app.clone())).map_err(|_| anyhow::anyhow!("the app is already running"))?;

    start_watching();
    reload_sidebar();
    crate::updates::start(mtm);
    app.window.makeKeyAndOrderFront(None);
    first_launch_layout(&app.window);
    // Now that the panes have their size, fit the columns to them.
    for pane in &app.panes {
        pane.fit_columns();
    }
    app.panes[0].focus();
    set_active(0);
    if let Some(target) = open {
        open_target(&target);
    }
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
    preview.view().widthAnchor().constraintLessThanOrEqualToConstant(520.0).setActive(true);
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
    let overrides = app.settings.borrow().shortcuts.clone();
    NSApplication::sharedApplication(mtm).setMainMenu(Some(&main_menu(mtm, &app._delegate, &overrides)));
}

fn main_menu(
    mtm: MainThreadMarker,
    delegate: &AppDelegate,
    overrides: &std::collections::BTreeMap<String, String>,
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
    command(&file, Command::ImportFileZilla);
    separator(&file);
    for c in [Command::Open, Command::QuickLook] {
        command(&file, c);
    }
    separator(&file);
    for c in [Command::NewFolder, Command::Rename, Command::MultiRename, Command::Duplicate] {
        command(&file, c);
    }
    separator(&file);
    command(&file, Command::Trash);
    command(&file, Command::Permissions);
    separator(&file);
    for c in [Command::CompareFiles, Command::CalculateSizes, Command::CopySha256, Command::CopyMd5] {
        command(&file, c);
    }
    separator(&file);
    for c in [Command::RevealInFinder, Command::OpenInTerminal, Command::AddToSidebar] {
        command(&file, c);
    }
    separator(&file);
    command(&file, Command::CloseTab);

    let edit = submenu("Edit");
    standard(&edit, "Cut", sel!(cut:), "x");
    standard(&edit, "Copy", sel!(copy:), "c");
    standard(&edit, "Paste", sel!(paste:), "v");
    command(&edit, Command::MoveFilesHere);
    command(&edit, Command::CopyPath);
    standard(&edit, "Select All", sel!(selectAll:), "a");
    separator(&edit);
    command(&edit, Command::Filter);

    let view = submenu("View");
    for c in [Command::ToggleHidden, Command::Refresh] {
        command(&view, c);
    }
    separator(&view);
    command(&view, Command::TogglePreview);
    separator(&view);
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
    command(&go, Command::GoToFolder);

    let commands = submenu("Commands");
    for c in [Command::CopyToOther, Command::MoveToOther] {
        command(&commands, c);
    }
    separator(&commands);
    for c in [Command::SwapPanes, Command::SameFolderInOther] {
        command(&commands, c);
    }
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
        Command::Up => pane.location().parent().is_some(),
        Command::Rename
        | Command::MultiRename
        | Command::Duplicate
        | Command::Trash
        | Command::CopyToOther
        | Command::MoveToOther => has_selection,
        // These need files on the Mac.
        Command::CopyFiles | Command::QuickLook => has_selection && local,
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
        Command::CopySha256 | Command::CopyMd5 => has_selection,
        Command::NextTab | Command::PreviousTab => pane.tab_count() > 1,
        Command::CompareFiles => {
            let here = pane.selected_paths().len();
            here == 2 || (here == 1 && app.panes[1 - index].selected_paths().len() == 1)
        }
        Command::Disconnect => !local,
        // A shell needs SSH; FTP, WebDAV, and S3 have no permissions.
        Command::OpenInTerminal => local || pane.fs().key().starts_with("sftp://"),
        Command::Permissions => pane.fs().supports_permissions(),
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
        Command::SwitchPane => other.focus(),
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
        Command::Disconnect => disconnect(&pane.fs().key()),
        Command::ImportFileZilla => import_filezilla(),
        Command::Permissions => change_permissions(&pane),
        Command::CompareFiles => compare_files(&pane, &other),
        Command::CalculateSizes => calculate_sizes(&pane),
        Command::CopySha256 => copy_checksums(&pane, crate::fs::checksum::Algorithm::Sha256),
        Command::CopyMd5 => copy_checksums(&pane, crate::fs::checksum::Algorithm::Md5),
        Command::Filter => pane.focus_filter(),
        Command::Settings => settings_window::show(),
        Command::NewTab => pane.new_tab(pane.fs(), pane.location()),
        Command::CloseTab => {
            if !pane.close_tab(pane.active_tab()) {
                app.window.performClose(None);
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
    let (show_hidden, limit, appearance_changed, shortcuts_changed) = {
        let mut settings = app.settings.borrow_mut();
        let (appearance, shortcuts) = (settings.appearance.clone(), settings.shortcuts.clone());
        change(&mut settings);
        settings.save();
        (
            settings.show_hidden,
            settings.transfers.max_transfers,
            settings.appearance != appearance,
            settings.shortcuts != shortcuts,
        )
    };
    for pane in &app.panes {
        if pane.show_hidden() != show_hidden {
            pane.set_show_hidden(show_hidden);
        }
    }
    app.activity.set_limit(limit);
    if appearance_changed {
        apply_appearance(&app);
    }
    if shortcuts_changed {
        rebuild_main_menu();
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
    for pane in &app.panes {
        pane.apply_appearance(appearance.alternating_rows);
    }
    let name = match appearance.theme.as_str() {
        "light" => Some(unsafe { objc2_app_kit::NSAppearanceNameAqua }),
        "dark" => Some(unsafe { objc2_app_kit::NSAppearanceNameDarkAqua }),
        _ => None,
    };
    let chosen = name.and_then(objc2_app_kit::NSAppearance::appearanceNamed);
    NSApplication::sharedApplication(mtm()).setAppearance(chosen.as_deref());
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

/// Compare Files: two files in the pane, or one in each pane, in the
/// chosen compare tool. Server files are downloaded first.
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
        start_job(Job::local(JobKind::Trash, paths, pane.location()));
        return;
    }
    let what = match paths.as_slice() {
        [one] => format!("\u{201c}{}\u{201d}", title_of(one)),
        many => format!("{} items", many.len()),
    };
    let message = format!("Delete {what} from {}?", fs.label());
    if dialogs::confirm(&message, "Servers have no Trash: this cannot be undone.", "Delete", true) {
        start_job(Job::between(JobKind::Delete, fs.clone(), paths, fs, pane.location()));
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
    let paths = pane.target_paths();
    if paths.is_empty() {
        return;
    }
    let items: Vec<crate::rename::Item> = pane
        .entries_for(&paths)
        .into_iter()
        .map(|e| crate::rename::Item {
            folder: e.kind == crate::fs::EntryKind::Folder && !e.package,
            name: e.name,
            modified: e.modified,
        })
        .collect();
    let Some(planned) = multi_rename::show(items, pane.all_names()) else { return };
    let (fs, dir, index) = (pane.fs(), pane.location(), pane.index());
    let changed = planned.iter().filter(|p| p.changes()).count();
    std::thread::spawn(move || {
        let (done, failed) = crate::rename::apply(&fs, &dir, &planned);
        DispatchQueue::main().exec_async(move || {
            let Some(app) = app() else { return };
            app.panes[index].reload_selecting(&done);
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
    let text = match (job.kind, job.source_fs.is_local(), job.dest_fs.is_local()) {
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

fn job_finished(id: isize, job: Job, outcome: Outcome) {
    let Some(app) = app() else { return };
    app.activity.remove(id);
    app.activity.job_done(id);
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
                && let Some(remote) = job.sources.iter().find(|s| s.file_name() == path.file_name())
            {
                remote_edit::track(job.source_fs.clone(), remote.clone(), path.clone());
            }
            workspace.openURL(&file_url(path));
        }
    }
    activity::report_problems(&outcome);
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

fn toggle_preview() {
    let Some(app) = app() else { return };
    let view = app.preview.view();
    let show = view.isHidden();
    view.setHidden(!show);
    if let Some(split) = app.window.contentView().and_then(|v| v.downcast::<NSSplitView>().ok()) {
        split.adjustSubviews();
        if show {
            split.layoutSubtreeIfNeeded();
            let width = app.settings.borrow().preview.width.clamp(220.0, 520.0);
            let total = split.frame().size.width;
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
    if fs.is_local() {
        app.window.setTitle(&ns(&title_of(&location)));
        app.window.setRepresentedURL(Some(&file_url(&location)));
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
    if fs.is_local() {
        return sidebar::Current::Local(pane.location());
    }
    let connections = app.connections.borrow();
    match connections.iter().find(|(_, c)| vfs::same(c, &fs)) {
        Some((id, _)) => sidebar::Current::Site(id.clone(), pane.location()),
        None => sidebar::Current::Other,
    }
}

/// A pane went to another folder.
pub fn location_changed(index: usize) {
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

fn reload_sidebar() {
    let Some(app) = app() else { return };
    let (favorites, remote) = {
        let settings = app.settings.borrow();
        (settings.favorites.clone(), settings.remote_favorites.clone())
    };
    let sites = app.sites.borrow().sites.clone();
    app.sidebar.reload(&favorites, &remote, &sites, current_place(&app));
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
                    show_site(&site, fs, pane_index);
                }
                Err(e) => {
                    // The site stays clickable: show what the pane shows.
                    update_title(&app);
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
    let folder = site.remote_dir.as_ref().filter(|_| !webdav).map(PathBuf::from).unwrap_or_else(|| fs.home());
    let pane = app.panes[pane_index].clone();
    pane.navigate_to(fs, folder);
    pane.focus();
    if let Some(local) = site.local_dir.as_ref().filter(|d| d.is_dir()) {
        app.panes[1 - pane_index].navigate_to(vfs::local(), local.clone());
    }
    update_title(&app);
}

/// Opens a folder or a server address (sftp://, ftp://, ftps://, dav://,
/// davs://, s3://) in the active pane.
fn open_target(target: &str) {
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
        let affected = paths.iter().any(|p| p == &location || p.parent() == Some(location.as_path()));
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
