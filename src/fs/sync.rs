//! Folder sync: compares two folders (on any file systems) and plans what
//! to copy or delete to make them match, one way or both ways. The plan is
//! carried out as ordinary jobs.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::Entry;
use super::glob::Patterns;
use super::ops::{CancelToken, Job, JobKind, Resolution};
use crate::vfs::FsRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Direction {
    /// The right folder becomes like the left one.
    #[default]
    LeftToRight,
    RightToLeft,
    /// Each side gets what the other has new or newer (nothing is deleted).
    Both,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub direction: Direction,
    pub subfolders: bool,
    pub hidden: bool,
    /// Copy items the other side does not have.
    pub add: bool,
    /// Copy items that differ (one way), or are newer (both ways).
    pub update: bool,
    /// Delete items only the target has (one way only).
    pub delete_orphans: bool,
    /// Copy folders with nothing in them.
    pub empty_folders: bool,
    /// Added to the right side's dates before comparing (servers whose
    /// clocks or time zones are off).
    pub time_offset_hours: i32,
    /// Names to leave out (patterns, separated by ;).
    pub exclude: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            direction: Direction::LeftToRight,
            subfolders: true,
            hidden: false,
            add: true,
            update: true,
            delete_orphans: false,
            empty_folders: true,
            time_offset_hours: 0,
            exclude: ".DS_Store".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    CopyToRight,
    CopyToLeft,
    DeleteRight,
    DeleteLeft,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    OnlyLeft,
    OnlyRight,
    LeftNewer,
    RightNewer,
    /// Same date, different size.
    Different,
    /// A file on one side, a folder on the other.
    KindDiffers,
}

impl Reason {
    pub fn describe(self) -> &'static str {
        match self {
            Reason::OnlyLeft => "Only on the left",
            Reason::OnlyRight => "Only on the right",
            Reason::LeftNewer => "Newer on the left",
            Reason::RightNewer => "Newer on the right",
            Reason::Different => "Different size",
            Reason::KindDiffers => "File on one side, folder on the other",
        }
    }
}

/// One difference, and what to do about it (None: left alone, as for a
/// file that is a folder on the other side).
#[derive(Debug, Clone)]
pub struct Item {
    /// Relative to both folders compared.
    pub path: PathBuf,
    pub folder: bool,
    pub reason: Reason,
    pub action: Option<Action>,
    pub left: Option<Entry>,
    pub right: Option<Entry>,
}

/// Dates closer than this count as equal (FAT and FTP keep two-second or
/// one-minute dates).
const TOLERANCE: Duration = Duration::from_secs(2);

/// Compares the two folders. `cancel` stops it (an Interrupted error).
pub fn compare(
    left: &FsRef,
    left_root: &Path,
    right: &FsRef,
    right_root: &Path,
    options: &Options,
    cancel: &CancelToken,
) -> io::Result<Vec<Item>> {
    let exclude = Patterns::new(&options.exclude);
    let mut items = Vec::new();
    // Both folders must be readable.
    let mut pending = vec![PathBuf::new()];
    let mut first = true;
    while let Some(relative) = pending.pop() {
        if cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
        }
        let list = |fs: &FsRef, root: &Path| -> io::Result<BTreeMap<String, Entry>> {
            Ok(fs
                .list(&root.join(&relative))?
                .into_iter()
                .filter(|e| options.hidden || !e.hidden)
                .filter(|e| exclude.is_empty() || !exclude.matches(&e.name))
                .map(|e| (e.name.clone(), e))
                .collect())
        };
        let (lefts, rights) = if first {
            (list(left, left_root)?, list(right, right_root)?)
        } else {
            (list(left, left_root).unwrap_or_default(), list(right, right_root).unwrap_or_default())
        };
        first = false;
        let mut names: Vec<&String> = lefts.keys().chain(rights.keys()).collect();
        names.sort();
        names.dedup();
        for name in names {
            let path = relative.join(name);
            let (l, r) = (lefts.get(name), rights.get(name));
            match (l, r) {
                (Some(l), Some(r)) if l.is_folder_like() && r.is_folder_like() => {
                    if options.subfolders {
                        pending.push(path);
                    }
                }
                (Some(l), Some(r)) if l.is_folder_like() != r.is_folder_like() => {
                    items.push(Item {
                        path,
                        folder: false,
                        reason: Reason::KindDiffers,
                        action: None,
                        left: Some(l.clone()),
                        right: Some(r.clone()),
                    });
                }
                (Some(l), Some(r)) => {
                    if let Some((reason, action)) = compare_files(l, r, options) {
                        items.push(Item {
                            path,
                            folder: false,
                            reason,
                            action,
                            left: Some(l.clone()),
                            right: Some(r.clone()),
                        });
                    }
                }
                (Some(l), None) => {
                    if let Some(action) = one_sided(true, l, left, left_root, &path, options) {
                        items.push(Item {
                            path,
                            folder: l.is_folder_like(),
                            reason: Reason::OnlyLeft,
                            action: Some(action),
                            left: Some(l.clone()),
                            right: None,
                        });
                    }
                }
                (None, Some(r)) => {
                    if let Some(action) = one_sided(false, r, right, right_root, &path, options) {
                        items.push(Item {
                            path,
                            folder: r.is_folder_like(),
                            reason: Reason::OnlyRight,
                            action: Some(action),
                            left: None,
                            right: Some(r.clone()),
                        });
                    }
                }
                (None, None) => {}
            }
        }
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(items)
}

fn compare_files(l: &Entry, r: &Entry, options: &Options) -> Option<(Reason, Option<Action>)> {
    let offset = Duration::from_secs(u64::from(options.time_offset_hours.unsigned_abs()) * 3600);
    let right_date = r.modified.map(|d| if options.time_offset_hours >= 0 { d + offset } else { d - offset });
    let reason = match (l.modified, right_date) {
        (Some(ld), Some(rd)) if ld > rd + TOLERANCE => Reason::LeftNewer,
        (Some(ld), Some(rd)) if rd > ld + TOLERANCE => Reason::RightNewer,
        _ if l.size != r.size => Reason::Different,
        _ => return None,
    };
    if !options.update {
        return None;
    }
    let action = match (options.direction, reason) {
        (Direction::LeftToRight, _) => Some(Action::CopyToRight),
        (Direction::RightToLeft, _) => Some(Action::CopyToLeft),
        (Direction::Both, Reason::LeftNewer) => Some(Action::CopyToRight),
        (Direction::Both, Reason::RightNewer) => Some(Action::CopyToLeft),
        // Both ways, same date but different: which one wins is unclear.
        (Direction::Both, _) => None,
    };
    Some((reason, action))
}

/// What to do with an item only one side has.
fn one_sided(on_left: bool, entry: &Entry, fs: &FsRef, root: &Path, path: &Path, options: &Options) -> Option<Action> {
    let copy = if on_left { Action::CopyToRight } else { Action::CopyToLeft };
    let delete = if on_left { Action::DeleteLeft } else { Action::DeleteRight };
    // Is this side the source?
    let source = match options.direction {
        Direction::LeftToRight => on_left,
        Direction::RightToLeft => !on_left,
        Direction::Both => true,
    };
    if source {
        if !options.add {
            return None;
        }
        if entry.is_folder_like() && !options.empty_folders && fs.list(&root.join(path)).is_ok_and(|l| l.is_empty()) {
            return None;
        }
        Some(copy)
    } else if options.delete_orphans && options.direction != Direction::Both {
        Some(delete)
    } else {
        None
    }
}

/// The jobs that carry out the chosen items: copies grouped by folder
/// (replacing what is there), then deletions.
pub fn jobs(
    items: &[&Item],
    left: &FsRef,
    left_root: &Path,
    right: &FsRef,
    right_root: &Path,
    keep_dates: bool,
) -> Vec<Job> {
    let mut copies: BTreeMap<(bool, PathBuf), Vec<PathBuf>> = BTreeMap::new();
    let mut delete_left = Vec::new();
    let mut delete_right = Vec::new();
    for item in items {
        let parent = item.path.parent().map(Path::to_path_buf).unwrap_or_default();
        match item.action {
            Some(Action::CopyToRight) => copies.entry((true, parent)).or_default().push(left_root.join(&item.path)),
            Some(Action::CopyToLeft) => copies.entry((false, parent)).or_default().push(right_root.join(&item.path)),
            Some(Action::DeleteRight) => delete_right.push(right_root.join(&item.path)),
            Some(Action::DeleteLeft) => delete_left.push(left_root.join(&item.path)),
            None => {}
        }
    }
    let mut jobs = Vec::new();
    for ((to_right, parent), sources) in copies {
        let (from, to, root) = if to_right { (left, right, right_root) } else { (right, left, left_root) };
        let mut job = Job::between(JobKind::Copy, from.clone(), sources, to.clone(), root.join(parent));
        job.on_conflict = Some(Resolution::Replace);
        job.keep_dates = keep_dates;
        jobs.push(job);
    }
    for (fs, root, paths) in [(left, left_root, delete_left), (right, right_root, delete_right)] {
        if !paths.is_empty() {
            jobs.push(Job::between(JobKind::Delete, fs.clone(), paths, fs.clone(), root.to_path_buf()));
        }
    }
    jobs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::ops::{self, Answer, Conflict, ErrorAnswer, Observer, Progress};
    use std::time::SystemTime;

    struct Quiet;
    impl Observer for Quiet {
        fn progress(&mut self, _: &Progress) {}
        fn conflict(&mut self, _: &Conflict) -> Answer {
            Answer { resolution: Resolution::Replace, apply_to_all: true }
        }
        fn error(&mut self, path: &Path, error: &io::Error) -> ErrorAnswer {
            panic!("{}: {error}", path.display())
        }
    }

    fn write(path: &Path, text: &str, age_seconds: u64) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        let when = SystemTime::now() - Duration::from_secs(age_seconds);
        std::fs::File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
    }

    /// left: same.txt, newer.txt (newer), only-left.txt, folder/inner.txt,
    /// empty/; right: same.txt, newer.txt (older), only-right.txt,
    /// folder/ (empty), .DS_Store.
    fn folders() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let (l, r) = (dir.path().join("left"), dir.path().join("right"));
        write(&l.join("same.txt"), "same", 1000);
        write(&r.join("same.txt"), "same", 1000);
        write(&l.join("newer.txt"), "new version", 100);
        write(&r.join("newer.txt"), "old", 5000);
        write(&l.join("only-left.txt"), "left", 100);
        write(&l.join("folder/inner.txt"), "inner", 100);
        std::fs::create_dir_all(l.join("empty")).unwrap();
        std::fs::create_dir_all(r.join("folder")).unwrap();
        write(&r.join("only-right.txt"), "right", 100);
        write(&r.join(".DS_Store"), "x", 100);
        (dir, l, r)
    }

    fn summary(items: &[Item]) -> Vec<(String, Option<Action>)> {
        items.iter().map(|i| (i.path.display().to_string(), i.action)).collect()
    }

    #[test]
    fn plans_each_direction() {
        let (_dir, l, r) = folders();
        let fs = crate::vfs::local();
        let cancel = CancelToken::default();
        let one_way = Options { delete_orphans: true, ..Options::default() };
        let items = compare(&fs, &l, &fs, &r, &one_way, &cancel).unwrap();
        assert_eq!(
            summary(&items),
            [
                ("empty".into(), Some(Action::CopyToRight)),
                ("folder/inner.txt".into(), Some(Action::CopyToRight)),
                ("newer.txt".into(), Some(Action::CopyToRight)),
                ("only-left.txt".into(), Some(Action::CopyToRight)),
                ("only-right.txt".into(), Some(Action::DeleteRight)),
            ]
        );
        let back = Options { direction: Direction::RightToLeft, empty_folders: false, ..Options::default() };
        let items = compare(&fs, &l, &fs, &r, &back, &cancel).unwrap();
        // Without deleting orphans or empty folders: only copies to the left.
        assert_eq!(
            summary(&items),
            [("newer.txt".into(), Some(Action::CopyToLeft)), ("only-right.txt".into(), Some(Action::CopyToLeft)),]
        );
        let both = Options { direction: Direction::Both, delete_orphans: true, ..Options::default() };
        let items = compare(&fs, &l, &fs, &r, &both, &cancel).unwrap();
        assert_eq!(
            summary(&items),
            [
                ("empty".into(), Some(Action::CopyToRight)),
                ("folder/inner.txt".into(), Some(Action::CopyToRight)),
                ("newer.txt".into(), Some(Action::CopyToRight)),
                ("only-left.txt".into(), Some(Action::CopyToRight)),
                ("only-right.txt".into(), Some(Action::CopyToLeft)),
            ]
        );
    }

    #[test]
    fn time_offset_lines_up_dates() {
        let dir = tempfile::tempdir().unwrap();
        let (l, r) = (dir.path().join("left"), dir.path().join("right"));
        write(&l.join("a.txt"), "same", 100);
        write(&r.join("a.txt"), "same", 100 + 7200);
        let fs = crate::vfs::local();
        let cancel = CancelToken::default();
        let plain = compare(&fs, &l, &fs, &r, &Options::default(), &cancel).unwrap();
        assert_eq!(summary(&plain), [("a.txt".into(), Some(Action::CopyToRight))]);
        let shifted = Options { time_offset_hours: 2, ..Options::default() };
        assert!(compare(&fs, &l, &fs, &r, &shifted, &cancel).unwrap().is_empty());
    }

    #[test]
    fn syncing_makes_the_folders_match() {
        let (_dir, l, r) = folders();
        let fs = crate::vfs::local();
        let options = Options { delete_orphans: true, ..Options::default() };
        let items = compare(&fs, &l, &fs, &r, &options, &CancelToken::default()).unwrap();
        let chosen: Vec<&Item> = items.iter().collect();
        for job in jobs(&chosen, &fs, &l, &fs, &r, true) {
            let outcome = ops::run(&job, &mut Quiet, CancelToken::default());
            assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        }
        assert_eq!(std::fs::read_to_string(r.join("newer.txt")).unwrap(), "new version");
        assert_eq!(std::fs::read_to_string(r.join("folder/inner.txt")).unwrap(), "inner");
        assert!(r.join("empty").is_dir() && r.join("only-left.txt").exists());
        assert!(!r.join("only-right.txt").exists());
        // Nothing left to do (the excluded .DS_Store stays).
        let again = compare(&fs, &l, &fs, &r, &options, &CancelToken::default()).unwrap();
        assert!(again.is_empty(), "{:?}", summary(&again));
        assert!(r.join(".DS_Store").exists());
    }
}
