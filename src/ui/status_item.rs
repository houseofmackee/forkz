//! forkz in the menu bar (Settings > General): its synclets (when each runs
//! and how its last run went, Run Now, Open), saved sites to connect to,
//! and the transfers (Pause All, Stop All). The icon shows a warning when a
//! scheduled synclet's last run failed, and turns while one runs from here.

use std::cell::RefCell;
use std::collections::HashSet;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject, Sel};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSApplication, NSMenu, NSMenuDelegate, NSMenuItem, NSStatusBar, NSStatusItem};
use objc2_foundation::NSObject;

use super::util::{mtm, ns, symbol};

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzStatusMenu"]
    struct StatusMenu;

    unsafe impl NSObjectProtocol for StatusMenu {}

    unsafe impl NSMenuDelegate for StatusMenu {
        /// Made fresh each time it opens.
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            fill(menu, self);
        }
    }

    impl StatusMenu {
        #[unsafe(method(runSynclet:))]
        fn run_synclet(&self, sender: &NSMenuItem) {
            run_now(sender.tag() as usize);
        }

        #[unsafe(method(openSynclet:))]
        fn open_synclet(&self, sender: &NSMenuItem) {
            show_forkz();
            super::open_synclet(sender.tag() as usize);
        }

        #[unsafe(method(showLog:))]
        fn show_log(&self, sender: &NSMenuItem) {
            let name = super::current_settings().and_then(|s| s.synclets.get(sender.tag() as usize).map(|x| x.name.clone()));
            if let Some(log) = name.and_then(|n| crate::schedule::log_path(&n)) {
                let _ = std::process::Command::new("/usr/bin/open").arg("-a").arg("Console").arg(log).status();
            }
        }

        #[unsafe(method(connectSite:))]
        fn connect_site(&self, sender: &NSMenuItem) {
            let sites = crate::sites::SiteStore::load();
            if let Some(site) = sites.sites.get(sender.tag() as usize) {
                show_forkz();
                let name = percent_encoding::utf8_percent_encode(&site.name, percent_encoding::NON_ALPHANUMERIC);
                super::open_link(&format!("forkz://connect?site={name}"));
            }
        }

        #[unsafe(method(pauseAll:))]
        fn pause_all(&self, sender: &NSMenuItem) {
            if let Some(app) = super::app() {
                app.activity.set_all_paused(sender.tag() == 1);
            }
        }

        #[unsafe(method(stopAll:))]
        fn stop_all(&self, _sender: &NSMenuItem) {
            if let Some(app) = super::app() {
                app.activity.stop_everything();
            }
        }

        #[unsafe(method(openForkz:))]
        fn open_forkz(&self, _sender: &NSMenuItem) {
            show_forkz();
        }

        #[unsafe(method(hideIcon:))]
        fn hide_icon(&self, _sender: &NSMenuItem) {
            super::update_settings(|s| s.general.menu_bar_icon = false);
        }
    }
);

thread_local! {
    static ITEM: RefCell<Option<(Retained<NSStatusItem>, Retained<StatusMenu>)>> = const { RefCell::new(None) };
    /// Synclets running now from Run Now.
    static RUNNING: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Shows or removes the icon (as Settings says), and brings its look up to
/// date.
pub fn update() {
    let wanted = super::current_settings().is_some_and(|s| s.general.menu_bar_icon);
    let has = ITEM.with(|i| i.borrow().is_some());
    if wanted && !has {
        let mtm = mtm();
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(objc2_app_kit::NSVariableStatusItemLength);
        let target: Retained<StatusMenu> = unsafe { msg_send![super(StatusMenu::alloc(mtm).set_ivars(())), init] };
        let menu = NSMenu::new(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*target)));
        item.setMenu(Some(&menu));
        ITEM.with(|i| *i.borrow_mut() = Some((item, target)));
    } else if !wanted && has {
        if let Some((item, _)) = ITEM.with(|i| i.borrow_mut().take()) {
            NSStatusBar::systemStatusBar().removeStatusItem(&item);
        }
        return;
    }
    let state = State::now();
    ITEM.with(|i| {
        let Some((item, _)) = &*i.borrow() else { return };
        let (glyph, what) = if state.running {
            ("arrow.triangle.2.circlepath", "forkz: a synclet is running")
        } else if state.failed {
            ("exclamationmark.triangle", "forkz: a synclet did not sync")
        } else {
            ("rectangle.split.2x1", "forkz")
        };
        if let (Some(button), Some(image)) = (item.button(mtm()), symbol(glyph, what)) {
            image.setTemplate(true);
            button.setImage(Some(&image));
            button.setToolTip(Some(&ns(what)));
        }
    });
}

/// What the icon shows.
struct State {
    running: bool,
    failed: bool,
}

impl State {
    fn now() -> State {
        let runs = crate::schedule::runs();
        let synclets = super::current_settings().map(|s| s.synclets).unwrap_or_default();
        let failed =
            synclets.iter().filter(|s| s.schedule.is_some()).any(|s| runs.get(&s.name).is_some_and(|run| !run.ok));
        State { running: RUNNING.with(|r| !r.borrow().is_empty()), failed }
    }
}

fn item(title: &str, action: Option<Sel>, tag: isize, target: &StatusMenu) -> Retained<NSMenuItem> {
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(target.mtm()), &ns(title), action, &ns(""))
    };
    item.setTag(tag);
    if action.is_some() {
        unsafe { item.setTarget(Some(target)) };
    } else {
        item.setEnabled(false);
    }
    item
}

fn header(menu: &NSMenu, title: &str) {
    let item = NSMenuItem::sectionHeaderWithTitle(&ns(title), menu.mtm());
    menu.addItem(&item);
}

/// The menu: synclets, sites, transfers, and forkz itself.
fn fill(menu: &NSMenu, target: &StatusMenu) {
    menu.removeAllItems();
    menu.setAutoenablesItems(false);
    let settings = super::current_settings().unwrap_or_default();
    let runs = crate::schedule::runs();
    if !settings.synclets.is_empty() {
        header(menu, "Synclets");
        for (index, synclet) in settings.synclets.iter().enumerate() {
            let running = RUNNING.with(|r| r.borrow().contains(&synclet.name));
            let run = runs.get(&synclet.name);
            let failed = run.is_some_and(|r| !r.ok) && synclet.schedule.is_some();
            let glyph = match (running, failed, synclet.schedule) {
                (true, _, _) => "arrow.triangle.2.circlepath",
                (_, true, _) => "exclamationmark.triangle",
                (_, _, Some(_)) => "clock.arrow.2.circlepath",
                _ => "arrow.triangle.2.circlepath",
            };
            // In words too: menus may leave the icons out.
            let title = if running {
                format!("{}: running\u{2026}", synclet.name)
            } else if failed {
                format!("{}: did not sync", synclet.name)
            } else {
                synclet.name.clone()
            };
            let entry = item(&title, None, 0, target);
            entry.setEnabled(true);
            entry.setImage(symbol(glyph, &synclet.name).as_deref());
            let sub = NSMenu::new(menu.mtm());
            sub.setAutoenablesItems(false);
            let when = match synclet.schedule {
                Some(schedule) => format!("Runs {}", schedule.describe()),
                None => "Not scheduled".to_string(),
            };
            sub.addItem(&item(&when, None, 0, target));
            let last = if running {
                "Running now\u{2026}".to_string()
            } else {
                match run {
                    Some(run) => {
                        let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(run.when);
                        format!("Last run {}: {}", super::util::format_date(Some(when)), shortened(&run.summary, 70))
                    }
                    None => "Not run in the background yet".to_string(),
                }
            };
            sub.addItem(&item(&last, None, 0, target));
            sub.addItem(&NSMenuItem::separatorItem(menu.mtm()));
            let run_now = item("Run Now", Some(sel!(runSynclet:)), index as isize, target);
            run_now.setEnabled(!running);
            sub.addItem(&run_now);
            sub.addItem(&item("Open in forkz", Some(sel!(openSynclet:)), index as isize, target));
            if crate::schedule::log_path(&synclet.name).is_some_and(|p| p.exists()) {
                sub.addItem(&item("Show Log", Some(sel!(showLog:)), index as isize, target));
            }
            entry.setSubmenu(Some(&sub));
            menu.addItem(&entry);
        }
        menu.addItem(&NSMenuItem::separatorItem(menu.mtm()));
    }
    let sites = crate::sites::SiteStore::load();
    if !sites.sites.is_empty() {
        header(menu, "Connect to Site");
        for (index, site) in sites.sites.iter().enumerate().take(20) {
            let entry = item(&site.name, Some(sel!(connectSite:)), index as isize, target);
            entry.setImage(symbol("server.rack", &site.name).as_deref());
            menu.addItem(&entry);
        }
        menu.addItem(&NSMenuItem::separatorItem(menu.mtm()));
    }
    if let Some(app) = super::app() {
        let (running, waiting, paused) = app.activity.job_counts();
        if running + waiting > 0 {
            header(menu, "Transfers");
            let mut text = format!("{running} running");
            if waiting > 0 {
                text.push_str(&format!(", {waiting} waiting"));
            }
            menu.addItem(&item(&text, None, 0, target));
            let (title, tag) = if paused { ("Resume All", 0) } else { ("Pause All", 1) };
            menu.addItem(&item(title, Some(sel!(pauseAll:)), tag, target));
            menu.addItem(&item("Stop All", Some(sel!(stopAll:)), 0, target));
            menu.addItem(&NSMenuItem::separatorItem(menu.mtm()));
        }
    }
    menu.addItem(&item("Open forkz", Some(sel!(openForkz:)), 0, target));
    menu.addItem(&item("Hide Menu Bar Icon", Some(sel!(hideIcon:)), 0, target));
}

/// Text cut to about `limit` characters (whole, if it fits), for a menu
/// line that should not grow as wide as the screen.
fn shortened(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    format!("{}\u{2026}", cut.trim_end_matches([',', ':', ';']))
}

/// Brings forkz's window to the front.
fn show_forkz() {
    let mtm = mtm();
    NSApplication::sharedApplication(mtm).activate();
    if let Some(app) = super::app() {
        app.window.makeKeyAndOrderFront(None);
    }
}

/// Run Now: the synclet runs in the background, as a scheduled run would
/// (its output in its log, its run noted), while forkz goes on.
fn run_now(index: usize) {
    let Some(synclet) = super::current_settings().and_then(|s| s.synclets.get(index).cloned()) else { return };
    let (Ok(program), Some(log)) = (std::env::current_exe(), crate::schedule::log_path(&synclet.name)) else { return };
    if let Some(folder) = log.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let Ok(out) = std::fs::OpenOptions::new().create(true).append(true).open(&log) else { return };
    let Ok(err) = out.try_clone() else { return };
    let child = std::process::Command::new(program)
        .arg("synclet")
        .arg(&synclet.name)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            super::dialogs::show_error(format!("\u{201c}{}\u{201d} could not run.", synclet.name), e.to_string());
            return;
        }
    };
    let name = synclet.name.clone();
    RUNNING.with(|r| r.borrow_mut().insert(name.clone()));
    update();
    std::thread::spawn(move || {
        let _ = child.wait();
        DispatchQueue::main().exec_async(move || {
            RUNNING.with(|r| r.borrow_mut().remove(&name));
            super::reload_sidebar();
            update();
        });
    });
}

#[cfg(test)]
mod tests {
    use super::shortened;

    #[test]
    fn shortens_long_lines() {
        assert_eq!(shortened("1 item changed", 70), "1 item changed");
        let long = "could not connect to sftp://deploy@example.com: Connection refused, then more words here";
        let short = shortened(long, 40);
        assert!(short.chars().count() <= 41 && short.ends_with('\u{2026}'), "{short}");
        assert!(long.starts_with(short.trim_end_matches('\u{2026}')), "cut at a word: {short}");
    }
}
