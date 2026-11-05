//! File operations (copy, move, trash, delete) run as jobs on a worker
//! thread, between any two file systems (local or remote). The job reports
//! progress and asks about name conflicts through an [`Observer`], so the
//! same code runs under the UI and under tests.

use std::ffi::{CString, c_char, c_int, c_void};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use crate::fs::{Entry, EntryKind};
use crate::vfs::{self, FsRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Copy,
    Move,
    /// Local only: to the Trash, so it can be put back.
    Trash,
    /// Permanently (remote servers have no Trash).
    Delete,
}

#[derive(Debug, Clone)]
pub struct Job {
    pub kind: JobKind,
    pub source_fs: FsRef,
    pub sources: Vec<PathBuf>,
    pub dest_fs: FsRef,
    /// The folder to copy or move into; unused for Trash and Delete.
    pub destination: PathBuf,
    /// Used for conflicts instead of asking (for example "Replace" when
    /// downloading a file to open it).
    pub on_conflict: Option<Resolution>,
    /// Open what was copied when done (a downloaded remote file).
    pub open_when_done: bool,
    /// Keep modification dates on streamed copies (uploads, downloads).
    pub keep_dates: bool,
}

impl Job {
    /// A job on one file system (both ends the same).
    pub fn local(kind: JobKind, sources: Vec<PathBuf>, destination: PathBuf) -> Job {
        Job::between(kind, vfs::local(), sources, vfs::local(), destination)
    }

    pub fn between(
        kind: JobKind,
        source_fs: FsRef,
        sources: Vec<PathBuf>,
        dest_fs: FsRef,
        destination: PathBuf,
    ) -> Job {
        Job {
            kind,
            source_fs,
            sources,
            dest_fs,
            destination,
            on_conflict: None,
            open_when_done: false,
            keep_dates: true,
        }
    }

    /// "Copying 3 items to Documents" and similar, for the activity view.
    pub fn title(&self) -> String {
        let what = match self.sources.as_slice() {
            [one] => format!("\u{201c}{}\u{201d}", file_name(one)),
            many => format!("{} items", many.len()),
        };
        let mut to = format!("\u{201c}{}\u{201d}", file_name(&self.destination));
        if !self.dest_fs.is_local() {
            to.push_str(&format!(" on {}", self.dest_fs.label()));
        }
        match self.kind {
            JobKind::Copy if !self.source_fs.is_local() && self.dest_fs.is_local() => {
                format!("Downloading {what} to {to}")
            }
            JobKind::Copy if self.source_fs.is_local() && !self.dest_fs.is_local() => {
                format!("Uploading {what} to {to}")
            }
            JobKind::Copy => format!("Copying {what} to {to}"),
            JobKind::Move => format!("Moving {what} to {to}"),
            JobKind::Trash => format!("Moving {what} to the Trash"),
            JobKind::Delete => format!("Deleting {what}"),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub done_items: usize,
    pub total_items: usize,
    /// The name of the item being worked on.
    pub current: String,
}

impl Progress {
    /// From 0 to 1; by bytes when there are any, otherwise by items.
    pub fn fraction(&self) -> f64 {
        if self.total_bytes > 0 {
            (self.done_bytes as f64 / self.total_bytes as f64).min(1.0)
        } else if self.total_items > 0 {
            (self.done_items as f64 / self.total_items as f64).min(1.0)
        } else {
            0.0
        }
    }
}

/// An item that already exists at the destination.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub source: PathBuf,
    pub existing: PathBuf,
    pub source_is_folder: bool,
    pub existing_is_folder: bool,
    pub source_modified: Option<SystemTime>,
    pub existing_modified: Option<SystemTime>,
}

impl Conflict {
    /// Merge is only offered when both are folders.
    pub fn can_merge(&self) -> bool {
        self.source_is_folder && self.existing_is_folder
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Resolution {
    Replace,
    /// Replace only if the existing item is older; otherwise skip.
    ReplaceOlder,
    KeepBoth,
    Skip,
    /// Folders only: copy the contents into the existing folder, asking
    /// again about conflicts inside it.
    Merge,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub resolution: Resolution,
    /// Use the same answer for later conflicts of the same kind (file or
    /// folder) in this job.
    pub apply_to_all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorAnswer {
    Skip,
    Retry,
    Stop,
}

pub trait Observer {
    fn progress(&mut self, progress: &Progress);
    fn conflict(&mut self, conflict: &Conflict) -> Answer;
    fn error(&mut self, path: &Path, error: &io::Error) -> ErrorAnswer;
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// What was made at the top level of the destination (to select it).
    pub created: Vec<PathBuf>,
    pub skipped: usize,
    pub failed: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

/// Cancels (or pauses) a running job from another thread.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Pauses (true) or resumes (false) at the next chunk or item.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    /// Waits while paused (and not cancelled).
    pub fn wait_while_paused(&self) {
        while self.is_paused() && !self.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

/// A shared speed limit: callers wait their turn so that all of them
/// together stay under it.
struct Limiter {
    /// Bytes a second; 0 for no limit.
    rate: std::sync::atomic::AtomicU64,
    next: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Limiter {
    const fn new() -> Limiter {
        Limiter { rate: std::sync::atomic::AtomicU64::new(0), next: std::sync::Mutex::new(None) }
    }

    /// Waits until `bytes` more may go.
    fn take(&self, bytes: usize) {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        let now = std::time::Instant::now();
        let start = {
            let mut next = self.next.lock().unwrap_or_else(|e| e.into_inner());
            let start = next.map_or(now, |t| t.max(now));
            *next = Some(start + std::time::Duration::from_secs_f64(bytes as f64 / rate as f64));
            start
        };
        if start > now {
            std::thread::sleep(start - now);
        }
    }
}

static UPLOADS: Limiter = Limiter::new();
static DOWNLOADS: Limiter = Limiter::new();

/// Speed limits for uploads and downloads, in bytes a second (0: none),
/// shared by all transfers.
pub fn set_speed_limits(upload: u64, download: u64) {
    UPLOADS.rate.store(upload, Ordering::Relaxed);
    DOWNLOADS.rate.store(download, Ordering::Relaxed);
}

/// Stops the job: cancelled, or "Stop" chosen.
struct Stopped;

struct Runner<'a> {
    src: FsRef,
    dst: FsRef,
    /// Both ends are the Mac: clones, renames across folders, and
    /// copyfile(3) keep everything (links, metadata).
    both_local: bool,
    keep_dates: bool,
    observer: &'a mut dyn Observer,
    cancel: CancelToken,
    progress: Progress,
    file_answer: Option<Resolution>,
    folder_answer: Option<Resolution>,
    outcome: Outcome,
    fxp: Fxp,
}

/// Copying straight from one FTP server to another (FXP), for this job.
enum Fxp {
    /// Both ends are FTP servers; not tried yet.
    Untried(Box<(vfs::fxp::Endpoint, vfs::fxp::Endpoint)>),
    Ready(Box<vfs::fxp::Pair>),
    /// Not FTP to FTP, refused, or failed: files stream through the Mac.
    Off,
}

/// Runs a job to the end (or until cancelled or stopped). Remote ends get
/// their own connection for the job.
pub fn run(job: &Job, observer: &mut dyn Observer, cancel: CancelToken) -> Outcome {
    let connect = |fs: &FsRef| if fs.is_local() { Ok(fs.clone()) } else { fs.connect_another() };
    let ends = connect(&job.source_fs).and_then(|src| {
        // Copies within one server read and write at once, which an FTP
        // connection cannot do: they get a second connection.
        let shared = vfs::same(&job.source_fs, &job.dest_fs) && (job.kind != JobKind::Copy || src.is_local());
        let dst = if shared { src.clone() } else { connect(&job.dest_fs)? };
        Ok((src, dst))
    });
    let (src, dst) = match ends {
        Ok(ends) => ends,
        Err(e) => {
            let mut outcome = Outcome::default();
            outcome.failed.push((job.destination.clone(), format!("Could not connect: {e}")));
            return outcome;
        }
    };
    let both_local = src.is_local() && dst.is_local();
    let fxp = match (src.fxp_endpoint(), dst.fxp_endpoint()) {
        (Some(from), Some(to)) => Fxp::Untried(Box::new((from, to))),
        _ => Fxp::Off,
    };
    let mut runner = Runner {
        src,
        dst,
        both_local,
        keep_dates: job.keep_dates,
        observer,
        cancel,
        progress: Progress::default(),
        file_answer: job.on_conflict,
        folder_answer: job.on_conflict,
        outcome: Outcome::default(),
        fxp,
    };
    let result = match job.kind {
        JobKind::Trash => runner.trash_all(&job.sources),
        JobKind::Delete => runner.delete_all(&job.sources),
        JobKind::Copy | JobKind::Move => runner.transfer_all(job),
    };
    if result.is_err() {
        runner.outcome.cancelled = true;
    }
    runner.outcome
}

impl Runner<'_> {
    fn check_cancel(&self) -> Result<(), Stopped> {
        self.cancel.wait_while_paused();
        if self.cancel.is_cancelled() { Err(Stopped) } else { Ok(()) }
    }

    /// The speed limit that applies: downloads (from a server) or uploads.
    fn limiter(&self) -> Option<&'static Limiter> {
        if !self.src.is_local() {
            Some(&DOWNLOADS)
        } else if !self.dst.is_local() {
            Some(&UPLOADS)
        } else {
            None
        }
    }

    fn report(&mut self) {
        self.observer.progress(&self.progress);
    }

    /// Runs `f` until it works, the user skips (Ok(false)), or stops.
    fn attempt(&mut self, path: &Path, mut f: impl FnMut(&mut Self) -> io::Result<()>) -> Result<bool, Stopped> {
        loop {
            self.check_cancel()?;
            match f(self) {
                Ok(()) => return Ok(true),
                Err(e) if e.kind() == io::ErrorKind::Interrupted && self.cancel.is_cancelled() => return Err(Stopped),
                Err(e) => match self.observer.error(path, &e) {
                    ErrorAnswer::Retry => continue,
                    ErrorAnswer::Skip => {
                        self.outcome.failed.push((path.to_path_buf(), e.to_string()));
                        return Ok(false);
                    }
                    ErrorAnswer::Stop => return Err(Stopped),
                },
            }
        }
    }

    fn trash_all(&mut self, sources: &[PathBuf]) -> Result<(), Stopped> {
        self.progress.total_items = sources.len();
        for source in sources {
            self.progress.current = file_name(source);
            self.report();
            if self.attempt(source, |_| trash(source))? {
                self.outcome.created.push(source.clone());
            }
            self.progress.done_items += 1;
            self.report();
        }
        Ok(())
    }

    fn delete_all(&mut self, sources: &[PathBuf]) -> Result<(), Stopped> {
        for source in sources {
            self.progress.total_items += measure(&self.src, source).1;
        }
        self.report();
        for source in sources {
            if self.delete_tree(source)? {
                self.outcome.created.push(source.clone());
            }
        }
        Ok(())
    }

    /// Deletes an item and everything in it, counting items as it goes.
    fn delete_tree(&mut self, path: &Path) -> Result<bool, Stopped> {
        self.check_cancel()?;
        let src = self.src.clone();
        let Ok(entry) = src.stat(path) else { return Ok(true) };
        self.progress.current = entry.name.clone();
        let mut complete = true;
        if entry.kind == EntryKind::Folder {
            let children = match src.list(path) {
                Ok(children) => children,
                Err(e) => {
                    self.outcome.failed.push((path.to_path_buf(), e.to_string()));
                    return Ok(false);
                }
            };
            for child in children {
                complete &= self.delete_tree(&child.path)?;
            }
            if complete {
                complete = self.attempt(path, |_| src.remove_dir(path))?;
            }
        } else {
            complete = self.attempt(path, |_| src.remove_file(path))?;
        }
        self.progress.done_items += 1;
        self.report();
        Ok(complete)
    }

    /// A move is a rename: on one volume of the Mac, or on one server.
    fn renames(&self, source: &Path, destination: &Path) -> bool {
        if !vfs::same(&self.src, &self.dst) {
            return false;
        }
        !self.both_local || super::device_of(source) == super::device_of(destination)
    }

    fn transfer_all(&mut self, job: &Job) -> Result<(), Stopped> {
        let moving = job.kind == JobKind::Move;
        // Totals first, for a meaningful progress bar. Renames count as
        // items, not bytes.
        for source in &job.sources {
            let (bytes, items) =
                if moving && self.renames(source, &job.destination) { (0, 1) } else { measure(&self.src, source) };
            self.progress.total_bytes += bytes;
            self.progress.total_items += items;
        }
        self.report();

        let same_fs = vfs::same(&self.src, &self.dst);
        for source in &job.sources {
            self.check_cancel()?;
            let Some(name) = source.file_name() else { continue };
            let is_folder = self.src.stat(source).is_ok_and(|e| e.kind == EntryKind::Folder);
            if same_fs && is_folder && job.destination.starts_with(source) {
                let error = io::Error::other("A folder cannot be copied or moved into itself.");
                self.outcome.failed.push((source.clone(), error.to_string()));
                continue;
            }
            let target = job.destination.join(name);
            if moving && same_fs && source.parent() == Some(job.destination.as_path()) {
                // Moving to where it already is.
                self.outcome.skipped += 1;
                continue;
            }
            if !moving && same_fs && target == *source {
                // Copying into its own folder: a duplicate next to it.
                let target = unique_name(&self.dst, &target);
                if self.copy_item(source, &target)? {
                    self.outcome.created.push(target);
                }
                continue;
            }
            if let Some(target) = self.place(source, &target, moving)? {
                self.outcome.created.push(target);
            }
        }
        Ok(())
    }

    /// Copies or moves `source` to `target`, resolving a conflict if there
    /// is one. Returns where it ended up, or None if skipped.
    fn place(&mut self, source: &Path, target: &Path, moving: bool) -> Result<Option<PathBuf>, Stopped> {
        let mut target = target.to_path_buf();
        if let Ok(existing) = self.dst.stat(&target) {
            let conflict = self.conflict_for(source, &existing);
            let resolution = self.resolve(&conflict)?;
            match resolution {
                Resolution::Skip => {
                    self.skip(source);
                    return Ok(None);
                }
                Resolution::ReplaceOlder if !is_older(&conflict) => {
                    self.skip(source);
                    return Ok(None);
                }
                Resolution::Replace | Resolution::ReplaceOlder => {
                    // Files are replaced once the new copy is complete;
                    // folders (or a file replaced by a folder) go first.
                    if conflict.source_is_folder || conflict.existing_is_folder {
                        let dst = self.dst.clone();
                        let existing = target.clone();
                        if !self.attempt(&existing, |_| remove_tree(&dst, &existing))? {
                            return Ok(None);
                        }
                    }
                }
                Resolution::KeepBoth => target = unique_name(&self.dst, &target),
                Resolution::Merge => {
                    self.merge(source, &target, moving)?;
                    return Ok(Some(target));
                }
                Resolution::Stop => return Err(Stopped),
            }
        }
        let placed = if moving && self.renames(source, &target) {
            self.progress.current = file_name(source);
            self.report();
            let fs = self.src.clone();
            let done = self.attempt(source, |_| fs.rename(source, &target))?;
            self.progress.done_items += 1;
            self.report();
            done
        } else {
            let copied = self.copy_item(source, &target)?;
            if copied && moving {
                let src = self.src.clone();
                self.attempt(source, |_| remove_tree(&src, source))?
            } else {
                copied
            }
        };
        Ok(placed.then_some(target))
    }

    fn merge(&mut self, source: &Path, target: &Path, moving: bool) -> Result<(), Stopped> {
        let children = match self.src.list(source) {
            Ok(children) => children,
            Err(e) => {
                self.outcome.failed.push((source.to_path_buf(), e.to_string()));
                return Ok(());
            }
        };
        for child in children {
            self.check_cancel()?;
            self.place(&child.path, &target.join(&child.name), moving)?;
        }
        if moving {
            // Only removed if everything inside was moved.
            let _ = self.src.remove_dir(source);
        }
        Ok(())
    }

    fn skip(&mut self, source: &Path) {
        let (bytes, items) = measure(&self.src, source);
        self.progress.done_bytes += bytes;
        self.progress.done_items += items;
        self.outcome.skipped += 1;
        self.report();
    }

    fn conflict_for(&self, source: &Path, existing: &Entry) -> Conflict {
        let source_entry = self.src.stat(source).ok();
        Conflict {
            source: source.to_path_buf(),
            existing: existing.path.clone(),
            source_is_folder: source_entry.as_ref().is_some_and(|e| e.kind == EntryKind::Folder),
            existing_is_folder: existing.kind == EntryKind::Folder,
            source_modified: source_entry.and_then(|e| e.modified),
            existing_modified: existing.modified,
        }
    }

    fn resolve(&mut self, conflict: &Conflict) -> Result<Resolution, Stopped> {
        let remembered = if conflict.source_is_folder { self.folder_answer } else { self.file_answer };
        if let Some(resolution) = remembered {
            // A remembered Merge cannot apply to a file.
            if resolution != Resolution::Merge || conflict.can_merge() {
                return Ok(resolution);
            }
        }
        self.check_cancel()?;
        let answer = self.observer.conflict(conflict);
        if answer.resolution == Resolution::Merge && !conflict.can_merge() {
            return Ok(Resolution::KeepBoth);
        }
        if answer.apply_to_all {
            if conflict.source_is_folder {
                self.folder_answer = Some(answer.resolution);
            } else {
                self.file_answer = Some(answer.resolution);
            }
        }
        Ok(answer.resolution)
    }

    /// Copies a file, link, or whole folder to `target` (which does not
    /// exist, or is a file to be replaced). Returns false if skipped.
    fn copy_item(&mut self, source: &Path, target: &Path) -> Result<bool, Stopped> {
        let Ok(entry) = self.src.stat(source) else {
            let error = io::Error::new(io::ErrorKind::NotFound, "The item no longer exists.");
            self.outcome.failed.push((source.to_path_buf(), error.to_string()));
            return Ok(false);
        };
        match entry.kind {
            EntryKind::Folder => self.copy_folder(&entry, target),
            EntryKind::Symlink { target_is_folder: true } if !self.both_local => {
                // Following links to folders across file systems risks
                // endless loops; they are left out.
                self.outcome.failed.push((source.to_path_buf(), "Links to folders are not copied.".into()));
                Ok(false)
            }
            _ => self.copy_file(&entry, target),
        }
    }

    fn copy_folder(&mut self, entry: &Entry, target: &Path) -> Result<bool, Stopped> {
        let dst = self.dst.clone();
        if !self.attempt(&entry.path, |_| dst.create_dir(target))? {
            return Ok(false);
        }
        self.progress.done_items += 1;
        let children = match self.src.list(&entry.path) {
            Ok(children) => children,
            Err(e) => {
                self.outcome.failed.push((entry.path.clone(), e.to_string()));
                return Ok(false);
            }
        };
        let mut complete = true;
        for child in children {
            complete &= self.copy_item(&child.path, &target.join(&child.name))?;
        }
        // Dates (and on the Mac, permissions and extended attributes) of
        // the folder, after its contents (which change its date).
        if self.both_local {
            let _ = copyfile(&entry.path, target, libc::COPYFILE_METADATA, None);
        } else if self.keep_dates {
            let _ = self.dst.set_attributes(target, entry.modified, None);
        }
        Ok(complete)
    }

    fn copy_file(&mut self, entry: &Entry, target: &Path) -> Result<bool, Stopped> {
        self.progress.current = entry.name.clone();
        self.report();
        let start = self.progress.done_bytes;
        let size = entry.size;
        // Into a temporary name, then renamed: an existing file is only
        // replaced once the copy is complete.
        let temporary = temporary_name(target);
        let source = entry.path.clone();
        let done = self.attempt(&source, |runner| {
            runner.progress.done_bytes = start;
            if runner.both_local {
                let result = runner.copy_local(&source, &temporary, target, start, size);
                if result.is_err() {
                    let _ = runner.dst.remove_file(&temporary);
                }
                result
            } else {
                // A streamed copy keeps its partial file when it fails or is
                // stopped: the next attempt resumes it.
                match runner.copy_fxp(entry, &temporary, target, start, size) {
                    Some(result) => result,
                    None => runner.copy_stream(entry, &temporary, target, start, size),
                }
            }
        })?;
        self.progress.done_bytes = start + size;
        self.progress.done_items += 1;
        self.report();
        Ok(done)
    }

    fn copy_local(&mut self, source: &Path, temporary: &Path, target: &Path, start: u64, size: u64) -> io::Result<()> {
        let _ = std::fs::remove_file(temporary);
        let cancel = self.cancel.clone();
        let progress = &mut self.progress;
        let observer = &mut *self.observer;
        let mut on_bytes = |copied: u64| {
            progress.done_bytes = start + copied.min(size);
            observer.progress(progress);
            !cancel.is_cancelled()
        };
        copyfile(source, temporary, libc::COPYFILE_CLONE, Some(&mut on_bytes))?;
        std::fs::rename(temporary, target)
    }

    /// Streams a file between file systems (upload, download, or server to
    /// server), keeping its modification date.
    fn copy_stream(&mut self, entry: &Entry, temporary: &Path, target: &Path, start: u64, size: u64) -> io::Result<()> {
        let (mut reader, mut writer, mut copied) = match self.resumable(entry, temporary) {
            Some(offset) => {
                match (self.src.open_read_from(&entry.path, offset), self.dst.open_write_from(temporary, offset)) {
                    (Ok(reader), Ok(writer)) => {
                        tracing::info!("resuming {} at {offset} of {size} bytes", entry.name);
                        (reader, writer, offset)
                    }
                    // Cannot resume here: start over.
                    _ => (self.src.open_read(&entry.path)?, self.dst.open_write_sized(temporary, Some(entry.size))?, 0),
                }
            }
            None => (self.src.open_read(&entry.path)?, self.dst.open_write_sized(temporary, Some(entry.size))?, 0),
        };
        self.progress.done_bytes = start + copied.min(size);
        self.observer.progress(&self.progress);
        let limiter = self.limiter();
        // Smaller chunks under a speed limit, for steady progress.
        let chunk = if limiter.is_some_and(|l| l.rate.load(Ordering::Relaxed) > 0) { 32 * 1024 } else { 256 * 1024 };
        let mut buffer = vec![0u8; chunk];
        loop {
            self.cancel.wait_while_paused();
            if self.cancel.is_cancelled() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"));
            }
            let n = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if let Some(limiter) = limiter {
                limiter.take(n);
            }
            writer.write_all(&buffer[..n])?;
            copied += n as u64;
            self.progress.done_bytes = start + copied.min(size);
            self.observer.progress(&self.progress);
        }
        writer.flush()?;
        drop(writer);
        drop(reader);
        self.finish_transfer(entry, temporary, target)
    }

    /// Copies a file from one FTP server straight to another, if they
    /// allow it (FXP). None when they do not, or it failed: the file is
    /// then streamed through the Mac (resuming what FXP copied).
    fn copy_fxp(
        &mut self,
        entry: &Entry,
        temporary: &Path,
        target: &Path,
        start: u64,
        size: u64,
    ) -> Option<io::Result<()>> {
        let mut pair = match std::mem::replace(&mut self.fxp, Fxp::Off) {
            Fxp::Off => return None,
            Fxp::Ready(pair) => pair,
            Fxp::Untried(ends) => match vfs::fxp::Pair::connect(&ends.0, &ends.1) {
                Ok(pair) => {
                    tracing::info!("FXP: {} to {} directly", ends.0.params.host, ends.1.params.host);
                    Box::new(pair)
                }
                Err(e) => {
                    tracing::info!("FXP not used: {e}");
                    return None;
                }
            },
        };
        let offset = self.resumable(entry, temporary).unwrap_or(0);
        let label = format!("{} (server to server)", entry.name);
        let cancel = self.cancel.clone();
        let dst = self.dst.clone();
        let progress = &mut self.progress;
        let observer = &mut *self.observer;
        // The servers do not report progress: the target file's size does.
        let mut tick = || {
            progress.current.clone_from(&label);
            if let Ok(partial) = dst.stat(temporary) {
                progress.done_bytes = start + partial.size.min(size);
                observer.progress(progress);
            }
        };
        let result = pair.copy(&entry.path, temporary, offset, &|| cancel.is_cancelled(), &mut tick);
        self.progress.current = entry.name.clone();
        match result {
            Ok(()) => {
                self.fxp = Fxp::Ready(pair);
                Some(self.finish_transfer(entry, temporary, target))
            }
            Err(vfs::fxp::Error::Cancelled) => Some(Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"))),
            Err(e) => {
                tracing::info!("FXP stopped for this job ({e}); streaming {} instead", entry.name);
                None
            }
        }
    }

    /// A copied file's last steps: its date, then the temporary name
    /// becomes the real one.
    fn finish_transfer(&mut self, entry: &Entry, temporary: &Path, target: &Path) -> io::Result<()> {
        if self.keep_dates {
            let _ = self.dst.set_attributes(temporary, entry.modified, None);
        }
        // SFTP servers may refuse to rename over an existing file.
        if self.dst.stat(target).is_ok() {
            self.dst.remove_file(target)?;
        }
        self.dst.rename(temporary, target)
    }
}

impl Runner<'_> {
    /// How much of a partial file from an earlier attempt can be kept: it is
    /// smaller than the source, and was written after the source last
    /// changed (so it holds the start of this version of the file).
    fn resumable(&self, entry: &Entry, temporary: &Path) -> Option<u64> {
        let partial = self.dst.stat(temporary).ok().filter(|p| p.kind == EntryKind::File)?;
        let fresh = match (partial.modified, entry.modified) {
            (Some(partial), Some(source)) => partial >= source,
            _ => false,
        };
        (fresh && partial.size > 0 && partial.size < entry.size).then_some(partial.size)
    }
}

fn is_older(conflict: &Conflict) -> bool {
    match (conflict.existing_modified, conflict.source_modified) {
        (Some(existing), Some(source)) => existing < source,
        _ => false,
    }
}

/// Total bytes and items under `path` (links are not followed).
pub fn measure(fs: &FsRef, path: &Path) -> (u64, usize) {
    let Ok(entry) = fs.stat(path) else { return (0, 0) };
    if entry.kind != EntryKind::Folder {
        return (entry.size, 1);
    }
    let mut total = (0, 1);
    for child in fs.list(path).unwrap_or_default() {
        let (bytes, items) = measure(fs, &child.path);
        total.0 += bytes;
        total.1 += items;
    }
    total
}

/// "name 2.ext", "name 3.ext", and so on: the first that does not exist.
pub fn unique_name(fs: &FsRef, path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or(Path::new("/"));
    let name = file_name(path);
    let is_folder = fs.stat(path).is_ok_and(|e| e.kind == EntryKind::Folder);
    // Folders and dotfiles keep the whole name as the stem.
    let (stem, extension) = match name.rfind('.') {
        Some(dot) if dot > 0 && !is_folder => (&name[..dot], &name[dot..]),
        _ => (name.as_str(), ""),
    };
    // "report 2" becomes "report 3", not "report 2 2".
    let (base, mut number) = match stem.rsplit_once(' ') {
        Some((base, n)) if !base.is_empty() && n.parse::<u32>().is_ok_and(|n| n >= 2) => {
            (base, n.parse::<u32>().unwrap_or(1))
        }
        _ => (stem, 1),
    };
    loop {
        number += 1;
        let candidate = parent.join(format!("{base} {number}{extension}"));
        if fs.stat(&candidate).is_err() {
            return candidate;
        }
    }
}

fn temporary_name(target: &Path) -> PathBuf {
    let parent = target.parent().unwrap_or(Path::new("/"));
    parent.join(format!(".{}.forkz-partial", file_name(target)))
}

/// Removes a file, link, or folder with its contents.
pub fn remove_tree(fs: &FsRef, path: &Path) -> io::Result<()> {
    if fs.is_local() {
        let meta = std::fs::symlink_metadata(path)?;
        return if meta.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    }
    let entry = fs.stat(path)?;
    if entry.kind == EntryKind::Folder {
        for child in fs.list(path)? {
            remove_tree(fs, &child.path)?;
        }
        fs.remove_dir(path)
    } else {
        fs.remove_file(path)
    }
}

/// Moves an item to the Trash (so it can be put back from Finder).
pub fn trash(path: &Path) -> io::Result<()> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| io::Error::other(e.localizedDescription().to_string()))
}

/// Creates "untitled folder" (or "untitled folder 2", ...) in `parent`.
pub fn new_folder(fs: &FsRef, parent: &Path) -> io::Result<PathBuf> {
    let mut path = parent.join("untitled folder");
    if fs.stat(&path).is_ok() {
        path = unique_name(fs, &path);
    }
    fs.create_dir(&path)?;
    Ok(path)
}

/// Renames an item in place. Fails if the new name is taken or invalid.
pub fn rename(fs: &FsRef, path: &Path, new_name: &str) -> io::Result<PathBuf> {
    if new_name.is_empty() || new_name.contains('/') || new_name == "." || new_name == ".." {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "That name is not allowed."));
    }
    let target = path.with_file_name(new_name);
    if target == path {
        return Ok(target);
    }
    // A change of case only is allowed on case-insensitive volumes.
    let case_only = file_name(path).to_lowercase() == new_name.to_lowercase();
    if !case_only && fs.stat(&target).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("The name \u{201c}{new_name}\u{201d} is already taken."),
        ));
    }
    fs.rename(path, &target)?;
    Ok(target)
}

type ProgressFn<'a> = &'a mut dyn FnMut(u64) -> bool;

/// `copyfile(3)`, reporting bytes copied to `on_bytes` (which returns false
/// to cancel). `extra` is COPYFILE_CLONE for files (clone on APFS, or copy
/// data and metadata), or flags such as COPYFILE_METADATA.
fn copyfile(source: &Path, target: &Path, extra: u32, on_bytes: Option<ProgressFn>) -> io::Result<()> {
    extern "C" fn callback(
        what: c_int,
        stage: c_int,
        state: libc::copyfile_state_t,
        _src: *const c_char,
        _dst: *const c_char,
        ctx: *mut c_void,
    ) -> c_int {
        if what == libc::COPYFILE_COPY_DATA && stage == libc::COPYFILE_PROGRESS && !ctx.is_null() {
            let mut copied: libc::off_t = 0;
            // SAFETY: `state` is the live state; COPIED is an off_t.
            unsafe {
                libc::copyfile_state_get(state, libc::COPYFILE_STATE_COPIED as u32, (&raw mut copied).cast());
            }
            // SAFETY: `ctx` is the `&mut ProgressFn` set below, alive for the call.
            let on_bytes = unsafe { &mut *ctx.cast::<ProgressFn>() };
            if !on_bytes(copied as u64) {
                return libc::COPYFILE_QUIT;
            }
        }
        libc::COPYFILE_CONTINUE
    }

    let src = CString::new(source.as_os_str().as_bytes())?;
    let dst = CString::new(target.as_os_str().as_bytes())?;
    // SAFETY: the state is freed below; the callback context outlives the call.
    unsafe {
        let state = libc::copyfile_state_alloc();
        let mut on_bytes = on_bytes;
        if let Some(on_bytes) = on_bytes.as_mut() {
            let cb: libc::copyfile_callback_t = Some(callback);
            libc::copyfile_state_set(
                state,
                libc::COPYFILE_STATE_STATUS_CB as u32,
                cb.map_or(std::ptr::null(), |f| f as *const c_void),
            );
            libc::copyfile_state_set(
                state,
                libc::COPYFILE_STATE_STATUS_CTX as u32,
                (on_bytes as *mut ProgressFn).cast(),
            );
        }
        let flags = extra | libc::COPYFILE_METADATA | libc::COPYFILE_DATA | libc::COPYFILE_NOFOLLOW_SRC;
        let result = libc::copyfile(src.as_ptr(), dst.as_ptr(), state, flags);
        let error = io::Error::last_os_error();
        libc::copyfile_state_free(state);
        if result == 0 {
            Ok(())
        } else if error.raw_os_error() == Some(libc::ECANCELED) {
            Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"))
        } else {
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_spreads_bytes_over_time() {
        let limiter = Limiter::new();
        limiter.rate.store(10_000_000, Ordering::Relaxed);
        let start = std::time::Instant::now();
        for _ in 0..4 {
            limiter.take(2_500_000);
        }
        // 10 MB at 10 MB/s: the first chunk goes at once, the rest wait.
        let elapsed = start.elapsed().as_secs_f64();
        assert!((0.7..1.5).contains(&elapsed), "{elapsed}");
        limiter.rate.store(0, Ordering::Relaxed);
        let start = std::time::Instant::now();
        limiter.take(100_000_000);
        assert!(start.elapsed().as_millis() < 50);
    }

    #[test]
    fn pause_holds_until_resumed() {
        let token = CancelToken::default();
        token.set_paused(true);
        let waiter = token.clone();
        let handle = std::thread::spawn(move || {
            let start = std::time::Instant::now();
            waiter.wait_while_paused();
            start.elapsed()
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        token.set_paused(false);
        assert!(handle.join().unwrap().as_millis() >= 250);
        // Cancelling also ends the wait.
        token.set_paused(true);
        token.cancel();
        token.wait_while_paused();
    }

    /// Answers every conflict the same way and records what happened.
    struct Scripted {
        answer: Answer,
        conflicts: usize,
        last: Progress,
    }

    impl Scripted {
        fn new(resolution: Resolution) -> Self {
            Scripted { answer: Answer { resolution, apply_to_all: false }, conflicts: 0, last: Progress::default() }
        }
    }

    impl Observer for Scripted {
        fn progress(&mut self, progress: &Progress) {
            self.last = progress.clone();
        }
        fn conflict(&mut self, _conflict: &Conflict) -> Answer {
            self.conflicts += 1;
            self.answer
        }
        fn error(&mut self, _path: &Path, _error: &io::Error) -> ErrorAnswer {
            ErrorAnswer::Skip
        }
    }

    fn tree(root: &Path) {
        std::fs::create_dir_all(root.join("src/folder/inner")).unwrap();
        std::fs::write(root.join("src/a.txt"), "aaaa").unwrap();
        std::fs::write(root.join("src/folder/b.txt"), "bb").unwrap();
        std::fs::write(root.join("src/folder/inner/c.txt"), "c").unwrap();
        std::os::unix::fs::symlink("a.txt", root.join("src/link")).unwrap();
        std::fs::create_dir(root.join("dst")).unwrap();
    }

    fn job(kind: JobKind, root: &Path, names: &[&str]) -> Job {
        Job::local(kind, names.iter().map(|n| root.join("src").join(n)).collect(), root.join("dst"))
    }

    /// The same job, but from a "remote" file system (streamed copies).
    fn download(kind: JobKind, root: &Path, names: &[&str]) -> Job {
        let remote: FsRef = Arc::new(crate::vfs::FakeRemote);
        Job::between(
            kind,
            remote,
            names.iter().map(|n| root.join("src").join(n)).collect(),
            vfs::local(),
            root.join("dst"),
        )
    }

    fn upload(kind: JobKind, root: &Path, names: &[&str]) -> Job {
        let remote: FsRef = Arc::new(crate::vfs::FakeRemote);
        Job::between(
            kind,
            vfs::local(),
            names.iter().map(|n| root.join("src").join(n)).collect(),
            remote,
            root.join("dst"),
        )
    }

    #[test]
    fn copies_folders_files_and_links() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        let mut observer = Scripted::new(Resolution::Skip);
        let outcome =
            run(&job(JobKind::Copy, root, &["a.txt", "folder", "link"]), &mut observer, CancelToken::default());
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        assert_eq!(std::fs::read_to_string(root.join("dst/folder/inner/c.txt")).unwrap(), "c");
        assert_eq!(std::fs::read_link(root.join("dst/link")).unwrap(), Path::new("a.txt"));
        // 7 bytes of files; the link counts as an item, not bytes.
        assert_eq!(observer.last.done_bytes, 7);
        assert_eq!(observer.last.total_bytes, 7);
        assert_eq!(observer.last.fraction(), 1.0);
        assert!(root.join("src/a.txt").exists(), "copy keeps the source");
        // No temporary files left behind.
        let names: Vec<_> = std::fs::read_dir(root.join("dst")).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn conflicts_replace_keep_both_skip() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        std::fs::write(root.join("dst/a.txt"), "old").unwrap();

        let mut skip = Scripted::new(Resolution::Skip);
        let outcome = run(&job(JobKind::Copy, root, &["a.txt"]), &mut skip, CancelToken::default());
        assert_eq!((skip.conflicts, outcome.skipped), (1, 1));
        assert_eq!(std::fs::read_to_string(root.join("dst/a.txt")).unwrap(), "old");

        let mut keep = Scripted::new(Resolution::KeepBoth);
        let outcome = run(&job(JobKind::Copy, root, &["a.txt"]), &mut keep, CancelToken::default());
        assert_eq!(outcome.created, [root.join("dst/a 2.txt")]);

        let mut replace = Scripted::new(Resolution::Replace);
        run(&job(JobKind::Copy, root, &["a.txt"]), &mut replace, CancelToken::default());
        assert_eq!(std::fs::read_to_string(root.join("dst/a.txt")).unwrap(), "aaaa");
    }

    #[test]
    fn merge_folders() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        std::fs::create_dir_all(root.join("dst/folder")).unwrap();
        std::fs::write(root.join("dst/folder/keep.txt"), "k").unwrap();
        std::fs::write(root.join("dst/folder/b.txt"), "old").unwrap();
        // Merge the folder; inside it, b.txt conflicts and is then replaced
        // (Merge is not possible for a file, so it falls back to Keep Both).
        let mut observer = Scripted::new(Resolution::Merge);
        run(&job(JobKind::Copy, root, &["folder"]), &mut observer, CancelToken::default());
        assert_eq!(observer.conflicts, 2);
        assert!(root.join("dst/folder/keep.txt").exists());
        assert!(root.join("dst/folder/inner/c.txt").exists());
        assert_eq!(std::fs::read_to_string(root.join("dst/folder/b 2.txt")).unwrap(), "bb");
    }

    #[test]
    fn moves_and_skips_moving_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        let mut observer = Scripted::new(Resolution::Skip);
        let outcome = run(&job(JobKind::Move, root, &["folder", "a.txt"]), &mut observer, CancelToken::default());
        assert_eq!(outcome.created.len(), 2);
        assert!(!root.join("src/folder").exists());
        assert!(root.join("dst/folder/inner/c.txt").exists());

        let in_place = Job::local(JobKind::Move, vec![root.join("dst/a.txt")], root.join("dst"));
        let outcome = run(&in_place, &mut observer, CancelToken::default());
        assert_eq!(outcome.skipped, 1);
        assert!(root.join("dst/a.txt").exists());
    }

    #[test]
    fn duplicate_in_same_folder_and_not_into_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        let mut observer = Scripted::new(Resolution::Skip);
        let duplicate = Job::local(JobKind::Copy, vec![root.join("src/a.txt")], root.join("src"));
        let outcome = run(&duplicate, &mut observer, CancelToken::default());
        assert_eq!(outcome.created, [root.join("src/a 2.txt")]);

        let into_itself = Job::local(JobKind::Copy, vec![root.join("src")], root.join("src/folder"));
        let outcome = run(&into_itself, &mut observer, CancelToken::default());
        assert_eq!(outcome.failed.len(), 1);
    }

    #[test]
    fn cancelled_before_start() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        let cancel = CancelToken::default();
        cancel.cancel();
        let outcome = run(&job(JobKind::Copy, root, &["a.txt"]), &mut Scripted::new(Resolution::Skip), cancel);
        assert!(outcome.cancelled);
        assert!(!root.join("dst/a.txt").exists());
    }

    #[test]
    fn unique_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(p.join("report.pdf"), "").unwrap();
        std::fs::write(p.join("report 2.pdf"), "").unwrap();
        let fs = vfs::local();
        assert_eq!(unique_name(&fs, &p.join("report.pdf")), p.join("report 3.pdf"));
        assert_eq!(unique_name(&fs, &p.join("report 2.pdf")), p.join("report 3.pdf"));
        assert_eq!(unique_name(&fs, &p.join(".zshrc")), p.join(".zshrc 2"));
        std::fs::create_dir(p.join("my.folder")).unwrap();
        assert_eq!(unique_name(&fs, &p.join("my.folder")), p.join("my.folder 2"));
    }

    #[test]
    fn renames() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(p.join("a"), "").unwrap();
        std::fs::write(p.join("b"), "").unwrap();
        let fs = vfs::local();
        assert!(rename(&fs, &p.join("a"), "b").is_err());
        assert!(rename(&fs, &p.join("a"), "x/y").is_err());
        assert_eq!(rename(&fs, &p.join("a"), "A").unwrap(), p.join("A"));
        assert_eq!(new_folder(&fs, p).unwrap(), p.join("untitled folder"));
        assert_eq!(new_folder(&fs, p).unwrap(), p.join("untitled folder 2"));
    }

    #[test]
    fn downloads_and_uploads_stream_and_keep_dates() {
        for make in [download as fn(JobKind, &Path, &[&str]) -> Job, upload] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            tree(root);
            let old = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_600_000_000);
            std::fs::File::options().write(true).open(root.join("src/a.txt")).unwrap().set_modified(old).unwrap();
            let mut observer = Scripted::new(Resolution::Skip);
            let outcome =
                run(&make(JobKind::Copy, root, &["a.txt", "folder", "link"]), &mut observer, CancelToken::default());
            assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
            assert_eq!(std::fs::read_to_string(root.join("dst/folder/inner/c.txt")).unwrap(), "c");
            // Links to files are copied as files across file systems.
            assert_eq!(std::fs::read_to_string(root.join("dst/link")).unwrap(), "aaaa");
            let modified = std::fs::metadata(root.join("dst/a.txt")).unwrap().modified().unwrap();
            assert_eq!(modified, old);
            assert_eq!(observer.last.fraction(), 1.0);
            let names: Vec<_> = std::fs::read_dir(root.join("dst")).unwrap().flatten().map(|e| e.file_name()).collect();
            assert_eq!(names.len(), 3, "no partial files left: {names:?}");
        }
    }

    #[test]
    fn streamed_replace_and_move_across_file_systems() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        std::fs::write(root.join("dst/a.txt"), "old").unwrap();
        let mut replace = Scripted::new(Resolution::Replace);
        run(&upload(JobKind::Move, root, &["a.txt", "folder"]), &mut replace, CancelToken::default());
        assert_eq!(std::fs::read_to_string(root.join("dst/a.txt")).unwrap(), "aaaa");
        assert!(!root.join("src/a.txt").exists(), "moved, so the source is gone");
        assert!(!root.join("src/folder").exists());
        assert!(root.join("dst/folder/inner/c.txt").exists());
    }

    #[test]
    fn default_resolution_skips_the_question() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        std::fs::write(root.join("dst/a.txt"), "old").unwrap();
        let mut job = download(JobKind::Copy, root, &["a.txt"]);
        job.on_conflict = Some(Resolution::Replace);
        let mut observer = Scripted::new(Resolution::Stop);
        run(&job, &mut observer, CancelToken::default());
        assert_eq!(observer.conflicts, 0);
        assert_eq!(std::fs::read_to_string(root.join("dst/a.txt")).unwrap(), "aaaa");
    }

    #[test]
    fn deletes_trees() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        let remote: FsRef = Arc::new(crate::vfs::FakeRemote);
        let job = Job::between(JobKind::Delete, remote.clone(), vec![root.join("src")], remote, PathBuf::new());
        let mut observer = Scripted::new(Resolution::Skip);
        let outcome = run(&job, &mut observer, CancelToken::default());
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        assert!(!root.join("src").exists());
        assert_eq!(observer.last.done_items, observer.last.total_items);
    }

    /// Records the smallest progress seen, to see where a copy started.
    struct Watching {
        first: Option<u64>,
    }

    impl Observer for Watching {
        fn progress(&mut self, progress: &Progress) {
            if progress.done_bytes > 0 && self.first.is_none() {
                self.first = Some(progress.done_bytes);
            }
        }
        fn conflict(&mut self, _conflict: &Conflict) -> Answer {
            Answer { resolution: Resolution::Replace, apply_to_all: true }
        }
        fn error(&mut self, _path: &Path, _error: &io::Error) -> ErrorAnswer {
            ErrorAnswer::Stop
        }
    }

    #[test]
    fn resumes_a_fresh_partial_file_and_restarts_a_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("dst")).unwrap();
        let data: Vec<u8> = (0..1_000_000u32).map(|i| (i % 253) as u8).collect();
        std::fs::write(root.join("src/big.bin"), &data).unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(root.join("src/big.bin")).unwrap().set_modified(old).unwrap();
        let partial = root.join("dst/.big.bin.forkz-partial");

        // The first 300 000 bytes arrived before the transfer stopped.
        std::fs::write(&partial, &data[..300_000]).unwrap();
        let mut watching = Watching { first: None };
        let outcome = run(&upload(JobKind::Copy, root, &["big.bin"]), &mut watching, CancelToken::default());
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        assert_eq!(std::fs::read(root.join("dst/big.bin")).unwrap(), data);
        assert_eq!(watching.first, Some(300_000), "progress starts where the partial file ends");
        assert!(!partial.exists());

        // A partial file older than the source holds another version: start over.
        std::fs::remove_file(root.join("dst/big.bin")).unwrap();
        std::fs::write(&partial, vec![9u8; 300_000]).unwrap();
        let older = old - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&partial).unwrap().set_modified(older).unwrap();
        let mut watching = Watching { first: None };
        run(&upload(JobKind::Copy, root, &["big.bin"]), &mut watching, CancelToken::default());
        assert_eq!(std::fs::read(root.join("dst/big.bin")).unwrap(), data);
        assert!(watching.first.unwrap() < 300_000, "started from the beginning");
    }

    #[test]
    fn a_stopped_streamed_copy_keeps_its_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("dst")).unwrap();
        std::fs::write(root.join("src/big.bin"), vec![1u8; 2_000_000]).unwrap();
        // Stops the job once some bytes are copied.
        struct StopEarly(CancelToken);
        impl Observer for StopEarly {
            fn progress(&mut self, progress: &Progress) {
                if progress.done_bytes > 0 {
                    self.0.cancel();
                }
            }
            fn conflict(&mut self, _conflict: &Conflict) -> Answer {
                Answer { resolution: Resolution::Replace, apply_to_all: true }
            }
            fn error(&mut self, _path: &Path, _error: &io::Error) -> ErrorAnswer {
                ErrorAnswer::Stop
            }
        }
        let cancel = CancelToken::default();
        let outcome = run(&upload(JobKind::Copy, root, &["big.bin"]), &mut StopEarly(cancel.clone()), cancel);
        assert!(outcome.cancelled);
        let partial = std::fs::metadata(root.join("dst/.big.bin.forkz-partial")).unwrap();
        assert!(partial.len() > 0 && partial.len() < 2_000_000);
        assert!(!root.join("dst/big.bin").exists());
    }
}
