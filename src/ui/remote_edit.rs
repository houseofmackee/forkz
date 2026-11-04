//! Upload on save: a server file opened from forkz is downloaded to a
//! temporary folder and watched; each time it is saved (its modification
//! date changes), it is uploaded back over the original, as a normal job
//! with progress. With the "Ask first" setting, forkz asks each time.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dispatch2::{DispatchQueue, DispatchTime};
use notify::{RecursiveMode, Watcher};

use super::dialogs;
use crate::fs::ops::{JobKind, Resolution};
use crate::pane::title_of;
use crate::vfs::{self, FsRef};

struct Edit {
    fs: FsRef,
    remote: PathBuf,
    local: PathBuf,
    /// The modification date when last downloaded or uploaded.
    synced: Option<SystemTime>,
    /// A check is waiting (saves come as bursts of events).
    scheduled: bool,
}

thread_local! {
    static EDITS: RefCell<Vec<Edit>> = const { RefCell::new(Vec::new()) };
    static WATCHER: RefCell<Option<notify::RecommendedWatcher>> = const { RefCell::new(None) };
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Starts watching a downloaded copy of `remote` (on `fs`) at `local`.
pub fn track(fs: FsRef, remote: PathBuf, local: PathBuf) {
    // FSEvents reports real paths (/private/var/... for the temporary
    // folder at /var/...): compare those.
    let local = std::fs::canonicalize(&local).unwrap_or(local);
    let Some(folder) = local.parent().map(Path::to_path_buf) else { return };
    let synced = modified(&local);
    EDITS.with(|edits| {
        let mut edits = edits.borrow_mut();
        edits.retain(|e| e.local != local);
        edits.push(Edit { fs, remote, local, synced, scheduled: false });
    });
    WATCHER.with(|watcher| {
        let mut watcher = watcher.borrow_mut();
        if watcher.is_none() {
            let made = notify::recommended_watcher(|event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                DispatchQueue::main().exec_async(move || changed(&event.paths));
            });
            match made {
                Ok(made) => *watcher = Some(made),
                Err(e) => {
                    tracing::warn!("cannot watch opened server files: {e}");
                    return;
                }
            }
        }
        if let Some(watcher) = watcher.as_mut()
            && let Err(e) = watcher.watch(&folder, RecursiveMode::NonRecursive)
        {
            tracing::warn!("cannot watch {}: {e}", folder.display());
        }
    });
}

/// Something changed in a watched folder: check the files there soon.
fn changed(paths: &[PathBuf]) {
    let due: Vec<PathBuf> = EDITS.with(|edits| {
        let mut edits = edits.borrow_mut();
        edits
            .iter_mut()
            .filter(|e| !e.scheduled)
            .filter(|e| paths.iter().any(|p| p == &e.local || p.parent() == e.local.parent()))
            .map(|e| {
                e.scheduled = true;
                e.local.clone()
            })
            .collect()
    });
    for local in due {
        let when = DispatchTime::try_from(Duration::from_millis(700)).unwrap_or(DispatchTime::NOW);
        let _ = DispatchQueue::main().after(when, move || check(&local));
    }
}

/// Uploads the copy if it was saved since it was last synced.
fn check(local: &Path) {
    let found = EDITS.with(|edits| {
        let mut edits = edits.borrow_mut();
        let edit = edits.iter_mut().find(|e| e.local == local)?;
        edit.scheduled = false;
        // Missing for a moment while an editor replaces it: the next event
        // brings it back.
        let now = modified(local)?;
        if Some(now) == edit.synced {
            return None;
        }
        edit.synced = Some(now);
        Some((edit.fs.clone(), edit.remote.clone()))
    });
    let Some((fs, remote)) = found else { return };
    let name = title_of(&remote);
    if super::current_settings().is_some_and(|s| s.transfers.ask_before_upload_on_save) {
        let message = format!("\u{201c}{name}\u{201d} was saved. Upload it to {}?", fs.label());
        let detail = format!("It replaces {} on the server.", fs.display_path(&remote));
        if !dialogs::confirm(&message, &detail, "Upload", false) {
            return;
        }
    }
    let Some(folder) = remote.parent().map(Path::to_path_buf) else { return };
    let mut job = super::transfer_job(JobKind::Copy, vfs::local(), vec![local.to_path_buf()], fs, folder);
    // The point is to replace the original.
    job.on_conflict = Some(Resolution::Replace);
    tracing::info!("uploading the saved {name}");
    super::start_job(job);
}
