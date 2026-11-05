//! Workspaces: saved sets of tabs for both panes (folders on the Mac and
//! on saved sites), with which panes are hidden. Window > Workspaces opens
//! one (Control-Command-1 to 9) or saves the current one.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;

use dispatch2::DispatchQueue;

use super::dialogs;
use crate::pane::PaneState;
use crate::settings::{Workspace, WorkspacePane, WorkspaceTab};
use crate::vfs;

/// A server tab still to open: the pane, the site, the folder, and
/// whether it opens in a new tab (or the pane's current one).
struct Pending {
    pane: usize,
    site: crate::sites::Site,
    path: PathBuf,
    new_tab: bool,
}

thread_local! {
    static PENDING: RefCell<VecDeque<Pending>> = const { RefCell::new(VecDeque::new()) };
    static BUSY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// What the window shows now, as a workspace.
fn capture(name: &str) -> Option<Workspace> {
    let app = super::app()?;
    let pane = |index: usize| {
        let controller = &app.panes[index];
        let tabs: Vec<WorkspaceTab> = controller
            .tab_locations()
            .into_iter()
            .filter_map(|(fs, path)| {
                // Search results are kept as the folder searched.
                let fs = fs.browse_fs().unwrap_or(fs);
                // Saved sites (mounted shares too); archives and unsaved
                // servers are left out.
                if let Some(site) = super::site_of(&fs) {
                    Some(WorkspaceTab { site: Some(site), path })
                } else if fs.is_local() {
                    Some(WorkspaceTab { site: None, path })
                } else {
                    None
                }
            })
            .collect();
        WorkspacePane {
            active_tab: controller.active_tab().min(tabs.len().saturating_sub(1)),
            tabs,
            hidden: controller.view().isHidden(),
        }
    };
    Some(Workspace { name: name.to_string(), left: pane(0), right: pane(1), active: app.active.get() })
}

/// Asks for a name and saves the current tabs (replacing a workspace of
/// the same name).
pub fn save() {
    let Some(name) = dialogs::prompt(
        "Save Workspace",
        "A name for the tabs in both panes. Folders on servers are kept for saved sites.",
        "Save",
        "",
        None,
    ) else {
        return;
    };
    let name = name.trim().to_string();
    let Some(workspace) = capture(&name) else { return };
    super::update_settings(|s| match s.workspaces.iter_mut().find(|w| w.name == name) {
        Some(existing) => *existing = workspace,
        None => s.workspaces.push(workspace),
    });
    super::oplog::record(super::oplog::Kind::Change, format!("Saved the workspace \u{201c}{name}\u{201d}"), "");
}

pub fn delete(index: usize) {
    let Some(workspace) = super::current_settings().and_then(|s| s.workspaces.get(index).cloned()) else { return };
    let message = format!("Delete the workspace \u{201c}{}\u{201d}?", workspace.name);
    if dialogs::confirm(&message, "The folders and sites in it are not changed.", "Delete", true) {
        super::update_settings(|s| {
            s.workspaces.retain(|w| w.name != workspace.name);
        });
    }
}

/// Opens a saved workspace: each pane's tabs are replaced by its tabs.
pub fn open(index: usize) {
    let Some(app) = super::app() else { return };
    let Some(workspace) = app.settings.borrow().workspaces.get(index).cloned() else { return };
    let sites = app.sites.borrow().clone();
    let show_hidden = app.settings.borrow().show_hidden;
    PENDING.with(|p| p.borrow_mut().clear());
    for (index, saved) in [&workspace.left, &workspace.right].into_iter().enumerate() {
        let controller = &app.panes[index];
        // One tab left, then the workspace's tabs.
        while controller.tab_count() > 1 {
            controller.close_tab(controller.tab_count() - 1);
        }
        let mut first_used = false;
        let local = vfs::local();
        for tab in &saved.tabs {
            match &tab.site {
                None => {
                    let path = if tab.path.is_dir() { tab.path.clone() } else { local.home() };
                    if first_used {
                        let sort = controller.sort();
                        controller.add_background_tab(PaneState::new(local.clone(), path, sort, show_hidden));
                    } else {
                        controller.navigate_to(local.clone(), path);
                        first_used = true;
                    }
                }
                Some(site_id) => {
                    let Some(site) = sites.get(site_id).cloned() else { continue };
                    PENDING.with(|p| {
                        p.borrow_mut().push_back(Pending {
                            pane: index,
                            site,
                            path: tab.path.clone(),
                            new_tab: first_used,
                        })
                    });
                    first_used = true;
                }
            }
        }
        controller.select_tab(saved.active_tab.min(controller.tab_count().saturating_sub(1)));
        if controller.view().isHidden() != saved.hidden {
            super::toggle_pane(index);
        }
    }
    super::set_active(workspace.active.min(1));
    app.panes[workspace.active.min(1)].focus();
    super::oplog::record(
        super::oplog::Kind::Change,
        format!("Opened the workspace \u{201c}{}\u{201d}", workspace.name),
        "",
    );
    BUSY.with(|b| b.set(false));
    next();
}

/// Opens the next server tab of the workspace being opened.
fn next() {
    if BUSY.with(|b| b.get()) {
        return;
    }
    let Some(pending) = PENDING.with(|p| p.borrow_mut().pop_front()) else { return };
    let Some(app) = super::app() else { return };
    let controller = &app.panes[pending.pane];
    if pending.new_tab {
        controller.new_tab(controller.fs(), controller.location());
    }
    let mut site = pending.site;
    // (For WebDAV the remote folder is the server's WebDAV root.)
    // (For WebDAV the remote folder is the server's WebDAV root; for shares,
    // the share, and the tab's path is where it is mounted.)
    if !site.protocol.is_share()
        && !matches!(site.protocol, crate::sites::Protocol::Webdav | crate::sites::Protocol::Webdavs)
    {
        site.remote_dir = Some(pending.path.to_string_lossy().into_owned());
    }
    BUSY.with(|b| b.set(true));
    super::connect_site(site, pending.pane, None, false);
}

/// A connection finished (or failed): on to the next server tab.
pub fn connection_done() {
    if !BUSY.with(|b| b.replace(false)) {
        return;
    }
    DispatchQueue::main().exec_async(next);
}
