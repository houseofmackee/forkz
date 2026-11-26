//! Undo and Redo of file changes (Edit > Undo, Command-Z), with the
//! window's undo manager (so the Edit menu names them: "Undo Rename").
//! Renames and moves go back, items moved to the Trash are put back, and
//! items made (new folders and files, copies on the Mac) go to the Trash,
//! as in Finder. Deleting for good, and copies onto servers, cannot be
//! undone (undoing them would delete for good).
//!
//! Each action, when undone, does its opposite at once and registers the
//! opposite of that (which is how Redo works).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::runtime::AnyObject;

use super::{app, dialogs, oplog, start_job};
use crate::fs::ops::{Job, JobKind};
use crate::rename::Planned;
use crate::vfs::{self, FsRef};

/// What undoing does.
#[derive(Clone)]
pub enum Action {
    /// Renames within folders: (current path, name to go back to), each
    /// pair in one folder. Swapped names are handled (through temporary
    /// names).
    Rename { fs: FsRef, pairs: Vec<(PathBuf, PathBuf)> },
    /// Moves on one file system (into other folders): (current, back to).
    Move { fs: FsRef, pairs: Vec<(PathBuf, PathBuf)> },
    /// Moves between file systems (a job): (current on `from`, back to on
    /// `to`).
    MoveBetween { from: FsRef, to: FsRef, pairs: Vec<(PathBuf, PathBuf)> },
    /// Items on the Mac to move to the Trash.
    Trash { paths: Vec<PathBuf> },
    /// Items in the Trash to put back: (in the Trash, back to).
    PutBack { pairs: Vec<(PathBuf, PathBuf)> },
    /// Modification dates to set again.
    Dates { fs: FsRef, items: Vec<(PathBuf, std::time::SystemTime)> },
    /// Images on the Mac to rotate (degrees clockwise).
    Rotate { paths: Vec<PathBuf>, degrees: u32 },
}

/// Registers how to undo a change just made ("Rename", "Move", "Move to
/// Trash", "New Folder", "Copy").
pub fn record(name: &str, action: Action) {
    if is_empty(&action) {
        return;
    }
    register(name.to_string(), action);
}

fn is_empty(action: &Action) -> bool {
    match action {
        Action::Rename { pairs, .. }
        | Action::Move { pairs, .. }
        | Action::MoveBetween { pairs, .. }
        | Action::PutBack { pairs } => pairs.is_empty(),
        Action::Trash { paths } | Action::Rotate { paths, .. } => paths.is_empty(),
        Action::Dates { items, .. } => items.is_empty(),
    }
}

fn register(name: String, action: Action) {
    let Some(app) = app() else { return };
    let Some(manager) = app.window.undoManager() else { return };
    let label = name.clone();
    let handler = RcBlock::new(move |_target: NonNull<AnyObject>| perform(&label, action.clone()));
    let target: &AnyObject = &app.window;
    unsafe { manager.registerUndoWithTarget_handler(target, &handler) };
    manager.setActionName(&super::util::ns(&name));
}

/// Does an action (undoing or redoing), and registers its opposite.
fn perform(name: &str, action: Action) {
    match action {
        Action::Rename { fs, pairs } => {
            let mut done = Vec::new();
            let mut failed = Vec::new();
            // By folder, as Rename Multiple Items renames (it handles names
            // that swap).
            let mut by_folder: BTreeMap<PathBuf, Vec<(PathBuf, PathBuf)>> = BTreeMap::new();
            for (current, back) in pairs {
                let folder = current.parent().map(Path::to_path_buf).unwrap_or_default();
                by_folder.entry(folder).or_default().push((current, back));
            }
            for (folder, pairs) in by_folder {
                let planned: Vec<Planned> = pairs
                    .iter()
                    .map(|(current, back)| Planned { old: name_of(current), new: name_of(back), problem: None })
                    .collect();
                let (renamed, problems) = crate::rename::apply(&fs, &folder, &planned);
                for (current, back) in pairs {
                    if renamed.contains(&back) {
                        done.push((back, current));
                    }
                }
                failed.extend(problems.into_iter().map(|(n, e)| format!("{n}: {e}")));
            }
            finish(name, &fs, &done, &failed);
            register(name.to_string(), Action::Rename { fs, pairs: done });
        }
        Action::Move { fs, pairs } => {
            let mut done = Vec::new();
            let mut failed = Vec::new();
            for (current, back) in pairs {
                // Folders emptied and removed (by Flatten Folder) come back.
                if let Some(folder) = back.parent() {
                    ensure_folder(&fs, folder);
                }
                match fs.rename(&current, &back) {
                    Ok(()) => done.push((back, current)),
                    Err(e) => failed.push(format!("{}: {e}", name_of(&current))),
                }
            }
            finish(name, &fs, &done, &failed);
            register(name.to_string(), Action::Move { fs, pairs: done });
        }
        Action::MoveBetween { from, to, pairs } => {
            // A job per folder they go back to (with progress, as moves go).
            let mut by_folder: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
            for (current, back) in &pairs {
                let folder = back.parent().map(Path::to_path_buf).unwrap_or_default();
                by_folder.entry(folder).or_default().push(current.clone());
            }
            for (folder, items) in by_folder {
                let mut job = Job::between(JobKind::Move, from.clone(), items, to.clone(), folder);
                job.from_undo = true;
                start_job(job);
            }
            let opposite = pairs.into_iter().map(|(current, back)| (back, current)).collect();
            register(name.to_string(), Action::MoveBetween { from: to, to: from, pairs: opposite });
        }
        Action::Trash { paths } => {
            let mut done = Vec::new();
            let mut failed = Vec::new();
            for path in paths {
                match crate::fs::ops::trash_plainly(&path) {
                    Ok(Some(location)) => done.push((location, path)),
                    Ok(None) => failed.push(format!("{}: the Trash did not say where it went", name_of(&path))),
                    Err(e) => failed.push(format!("{}: {e}", name_of(&path))),
                }
            }
            let local = vfs::local();
            let shown: Vec<(PathBuf, PathBuf)> =
                done.iter().map(|(location, path)| (path.clone(), location.clone())).collect();
            finish(name, &local, &shown, &failed);
            if !done.is_empty() {
                super::sounds::play(super::sounds::Cue::Trash);
            }
            register(name.to_string(), Action::PutBack { pairs: done });
        }
        Action::PutBack { pairs } => {
            let mut done = Vec::new();
            let mut failed = Vec::new();
            for (location, back) in pairs {
                if back.symlink_metadata().is_ok() {
                    failed.push(format!("{}: something else has its name now", name_of(&back)));
                    continue;
                }
                match std::fs::rename(&location, &back) {
                    Ok(()) => done.push(back),
                    Err(e) => failed.push(format!("{}: {e}", name_of(&back))),
                }
            }
            let local = vfs::local();
            let shown: Vec<(PathBuf, PathBuf)> = done.iter().map(|p| (p.clone(), p.clone())).collect();
            finish(name, &local, &shown, &failed);
            register(name.to_string(), Action::Trash { paths: done });
        }
        Action::Dates { fs, items } => {
            let (before, failed) = super::file_tools::set_dates(&fs, items);
            let shown: Vec<(PathBuf, PathBuf)> = before.iter().map(|(p, _)| (p.clone(), p.clone())).collect();
            finish(name, &fs, &shown, &failed);
            register(name.to_string(), Action::Dates { fs, items: before });
        }
        Action::Rotate { paths, degrees } => {
            let (done, failed) = super::file_tools::rotate(&paths, degrees);
            let shown: Vec<(PathBuf, PathBuf)> = done.iter().map(|p| (p.clone(), p.clone())).collect();
            finish(name, &vfs::local(), &shown, &failed);
            register(name.to_string(), Action::Rotate { paths: done, degrees: 360 - degrees });
        }
    }
}

/// Makes a folder (and those above it) where missing.
fn ensure_folder(fs: &FsRef, folder: &Path) {
    if fs.stat(folder).is_ok() {
        return;
    }
    if let Some(parent) = folder.parent() {
        ensure_folder(fs, parent);
    }
    let _ = fs.create_dir(folder);
}

fn name_of(path: &Path) -> String {
    crate::pane::title_of(path)
}

/// The panes showing what changed list it again; problems are shown.
fn finish(name: &str, fs: &FsRef, pairs: &[(PathBuf, PathBuf)], failed: &[String]) {
    if let Some(app) = app() {
        let folders: Vec<PathBuf> = pairs
            .iter()
            .flat_map(|(a, b)| [a.parent().map(Path::to_path_buf), b.parent().map(Path::to_path_buf)])
            .flatten()
            .collect();
        for pane in &app.panes {
            if vfs::same(&pane.fs(), fs) && folders.contains(&pane.location()) {
                pane.reload();
            }
        }
    }
    if failed.is_empty() {
        oplog::record(oplog::Kind::Change, format!("Undid or redid {name}"), "");
    } else {
        let detail = failed.join("\n");
        oplog::record(oplog::Kind::Problem, format!("Could not undo or redo all of {name}"), detail.clone());
        dialogs::show_error(format!("Some of \u{201c}{name}\u{201d} could not be undone or redone."), detail);
    }
}
