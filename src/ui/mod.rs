//! The app: one window with a sidebar, two file panes, and running jobs at
//! the bottom; the menu bar and toolbar; and folder watching.

mod activity;
pub mod commands;
mod dialogs;
mod pane_view;
mod permissions;
mod sidebar;
mod site_form;
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
        Command::CopyPath if !here => "Copy sftp:// Link",
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
    ns_app.setMainMenu(Some(&main_menu(mtm, &delegate)));

    let settings = Settings::load();
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
    let window = build_window(mtm, &delegate, &panes, &sidebar, &activity);

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
        _delegate: delegate,
        settings: RefCell::new(settings),
        watcher: RefCell::new(None),
        watched: RefCell::new(Vec::new()),
        reload_pending: [Cell::new(false), Cell::new(false)],
        sites: RefCell::new(SiteStore::load()),
        connections: RefCell::new(Vec::new()),
    });
    app.activity.set_limit(app.settings.borrow().transfers.max_transfers);
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
    sidebar_view.widthAnchor().constraintGreaterThanOrEqualToConstant(130.0).setActive(true);
    sidebar_view.widthAnchor().constraintLessThanOrEqualToConstant(320.0).setActive(true);
    // Resizing the window resizes the panes, not the sidebar.
    main_split.setHoldingPriority_forSubviewAtIndex(260.0, 0);
    main_split.setHoldingPriority_forSubviewAtIndex(250.0, 1);
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

fn main_menu(mtm: MainThreadMarker, delegate: &AppDelegate) -> Retained<NSMenu> {
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
        let (key, modifiers) = command.shortcut();
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &ns(command.title()),
                Some(sel!(command:)),
                &ns(key),
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
    command(&file, Command::Connect);
    command(&file, Command::Disconnect);
    command(&file, Command::ImportFileZilla);
    separator(&file);
    for c in [Command::Open, Command::QuickLook] {
        command(&file, c);
    }
    separator(&file);
    for c in [Command::NewFolder, Command::Rename, Command::Duplicate] {
        command(&file, c);
    }
    separator(&file);
    command(&file, Command::Trash);
    command(&file, Command::Permissions);
    separator(&file);
    for c in [Command::RevealInFinder, Command::OpenInTerminal, Command::AddToSidebar] {
        command(&file, c);
    }
    separator(&file);
    standard(&file, "Close Window", sel!(performClose:), "w");

    let edit = submenu("Edit");
    standard(&edit, "Cut", sel!(cut:), "x");
    standard(&edit, "Copy", sel!(copy:), "c");
    standard(&edit, "Paste", sel!(paste:), "v");
    command(&edit, Command::MoveFilesHere);
    command(&edit, Command::CopyPath);
    standard(&edit, "Select All", sel!(selectAll:), "a");

    let view = submenu("View");
    for c in [Command::ToggleHidden, Command::Refresh] {
        command(&view, c);
    }
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
        Command::Rename | Command::Duplicate | Command::Trash | Command::CopyToOther | Command::MoveToOther => {
            has_selection
        }
        // These need files on the Mac.
        Command::CopyFiles | Command::QuickLook => has_selection && local,
        Command::RevealInFinder | Command::AddToSidebar => local,
        Command::RemoveFromSidebar => local && app.settings.borrow().favorites.contains(&pane.location()),
        Command::Disconnect => !local,
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
        Command::AddToSidebar => {}
        Command::RemoveFromSidebar => remove_favorite(&pane.location()),
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
    }
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
    let folder = std::env::temp_dir().join("forkz-open").join(fs.label().replace(['/', ':'], "_"));
    if let Err(e) = std::fs::create_dir_all(&folder) {
        dialogs::show_error("The file could not be downloaded.".into(), e.to_string());
        return;
    }
    let mut job = Job::between(JobKind::Copy, fs, files, vfs::local(), folder);
    job.on_conflict = Some(Resolution::Replace);
    job.open_when_done = true;
    start_job(job);
}

fn open_in_terminal(pane: &PaneController) {
    let fs = pane.fs();
    let folder = match pane.target_paths().as_slice() {
        [one] if pane.is_local() && one.is_dir() => one.clone(),
        _ => pane.location(),
    };
    let result = if fs.is_local() {
        std::process::Command::new("/usr/bin/open").args(["-a", "Terminal"]).arg(&folder).spawn().map(|_| ())
    } else {
        ssh_in_terminal(&fs, &folder)
    };
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
    let script = std::env::temp_dir().join(format!("forkz-ssh-{}.command", crate::sites::new_id()));
    std::fs::write(&script, format!("#!/bin/sh\nrm -f \"$0\"\nexec {command}\n"))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
    std::process::Command::new("/usr/bin/open").args(["-a", "Terminal"]).arg(&script).spawn().map(|_| ())
}

fn new_folder(pane: &PaneController) {
    match ops::new_folder(&pane.fs(), &pane.location()) {
        Ok(path) => {
            pane.reload_selecting(std::slice::from_ref(&path));
            // Name it right away, like Finder.
            rename_path(pane, &path);
        }
        Err(e) => dialogs::show_error("The folder could not be created.".into(), e.to_string()),
    }
}

fn rename(pane: &PaneController) {
    let targets = pane.target_paths();
    if let [path] = targets.as_slice() {
        rename_path(pane, path);
    }
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
            pane.reload_selecting(&[renamed]);
            other_pane_reload(pane.index());
        }
        Err(e) => dialogs::show_error(format!("\u{201c}{name}\u{201d} could not be renamed."), e.to_string()),
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

fn job_finished(id: isize, job: Job, outcome: Outcome) {
    let Some(app) = app() else { return };
    app.activity.remove(id);
    app.activity.job_done(id);
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
            workspace.openURL(&file_url(path));
        }
    }
    activity::report_problems(&outcome);
}

/// The pane that commands go to by default.
pub fn set_active(index: usize) {
    let Some(app) = app() else { return };
    app.active.set(index);
    app.panes[index].set_active(true);
    app.panes[1 - index].set_active(false);
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
        Some((id, _)) => sidebar::Current::Site(id.clone()),
        None => sidebar::Current::Other,
    }
}

/// A pane went to another folder.
pub fn location_changed(index: usize) {
    let Some(app) = app() else { return };
    let pane = &app.panes[index];
    if pane.is_local() {
        let mut settings = app.settings.borrow_mut();
        let location = pane.location();
        if index == 0 {
            settings.left.location = location;
        } else {
            settings.right.location = location;
        }
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
    let favorites = app.settings.borrow().favorites.clone();
    let sites = app.sites.borrow().sites.clone();
    app.sidebar.reload(&favorites, &sites, current_place(&app));
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
        let ssh = vfs::ssh_config::SshConfig::load().resolve(&site.host);
        let params = site.params(&ssh, password, timeout, extra_keys);
        tracing::debug!("connecting with {params:?} (from ~/.ssh/config: {ssh:?})");
        let prompter = UiPrompter { offer_save: saved_site, save: std::sync::Mutex::new(false) };
        let result = vfs::sftp::connect(params, &prompter);
        let save_typed = *prompter.save.lock().unwrap_or_else(|e| e.into_inner());
        DispatchQueue::main().exec_async(move || {
            let Some(app) = self::app() else { return };
            app.activity.remove(activity_id);
            match result {
                Ok(fs) => {
                    // Keep what worked if asked (the form's checkbox, or the
                    // password prompt's).
                    if let Some(secret) = fs.secret()
                        && ((save_password && typed) || save_typed)
                        && app.sites.borrow().get(&site.id).is_some()
                    {
                        crate::sites::keychain::save_password(&site.id, secret);
                    }
                    let fs: FsRef = fs;
                    app.connections.borrow_mut().push((site.id.clone(), fs.clone()));
                    show_site(&site, fs, pane_index);
                }
                Err(e) => {
                    // The site stays clickable: show what the pane shows.
                    update_title(&app);
                    if e.kind() != std::io::ErrorKind::Interrupted {
                        dialogs::show_error(format!("Could not connect to {}.", site.address()), e.to_string());
                    }
                }
            }
        });
    });
}

fn show_site(site: &crate::sites::Site, fs: FsRef, pane_index: usize) {
    let Some(app) = app() else { return };
    let folder = site.remote_dir.as_ref().map(PathBuf::from).unwrap_or_else(|| fs.home());
    let pane = app.panes[pane_index].clone();
    pane.navigate_to(fs, folder);
    pane.focus();
    if let Some(local) = site.local_dir.as_ref().filter(|d| d.is_dir()) {
        app.panes[1 - pane_index].navigate_to(vfs::local(), local.clone());
    }
    update_title(&app);
}

/// Opens a folder or an sftp:// address in the active pane.
fn open_target(target: &str) {
    let Some(app) = app() else { return };
    let Some(address) = target.strip_prefix("sftp://") else {
        app.panes[app.active.get()].navigate_to(vfs::local(), PathBuf::from(target));
        return;
    };
    let (authority, path) = match address.find('/') {
        Some(slash) => (&address[..slash], Some(address[slash..].to_string())),
        None => (address, None),
    };
    let (user, host_port) = match authority.rsplit_once('@') {
        Some((user, rest)) => (user.to_string(), rest),
        None => (String::new(), authority),
    };
    let (host, port) = match host_port.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => (host.to_string(), port.parse().unwrap_or(22)),
        _ => (host_port.to_string(), 22),
    };
    let wanted = crate::sites::Site::new(&host, port, &user);
    // A saved site for the same account brings its login settings.
    let saved = app
        .sites
        .borrow()
        .sites
        .iter()
        .find(|s| s.host.eq_ignore_ascii_case(&host) && s.port == port && (user.is_empty() || s.user == user))
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
