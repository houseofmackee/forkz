//! Finding duplicate files: files of the same size are compared by a hash
//! of their start, then of all of their contents (SHA-256), so only files
//! with the same bytes end up together.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Read};
use std::path::Path;

use sha2::{Digest, Sha256};

use super::Entry;
use super::checksum::{Algorithm, checksum};
use super::ops::CancelToken;
use crate::vfs::FsRef;

/// Files with the same contents.
#[derive(Debug, Clone)]
pub struct Group {
    pub size: u64,
    /// The oldest first.
    pub files: Vec<Entry>,
}

impl Group {
    /// The space the extra copies take.
    pub fn wasted(&self) -> u64 {
        self.size * (self.files.len() as u64).saturating_sub(1)
    }
}

/// How far a search is, for showing progress.
pub enum Stage {
    /// Listing folders: files seen so far.
    Listing(usize),
    /// Comparing contents: files done, of how many.
    Comparing(usize, usize),
}

/// Finds the duplicate files in `root` and everything in it (empty files
/// are left out). The groups that waste the most space come first.
pub fn find(
    fs: &FsRef,
    root: &Path,
    hidden: bool,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(Stage),
) -> io::Result<Vec<Group>> {
    let stopped = || io::Error::new(io::ErrorKind::Interrupted, "Stopped");
    // Every file, by size.
    let mut by_size: HashMap<u64, Vec<Entry>> = HashMap::new();
    let mut folders = VecDeque::from([root.to_path_buf()]);
    let (mut first, mut seen) = (true, 0);
    while let Some(folder) = folders.pop_front() {
        if cancel.is_cancelled() {
            return Err(stopped());
        }
        let entries = match fs.list(&folder) {
            Ok(entries) => entries,
            Err(e) if first => return Err(e),
            Err(_) => continue,
        };
        first = false;
        for entry in entries {
            if entry.hidden && !hidden {
                continue;
            }
            if entry.is_folder_like() {
                folders.push_back(entry.path.clone());
            } else if entry.kind == super::EntryKind::File && entry.size > 0 {
                seen += 1;
                by_size.entry(entry.size).or_default().push(entry);
            }
        }
        progress(Stage::Listing(seen));
    }
    let candidates: Vec<Vec<Entry>> = by_size.into_values().filter(|files| files.len() > 1).collect();
    let total: usize = candidates.iter().map(Vec::len).sum();
    let mut done = 0;
    let mut groups = Vec::new();
    for files in candidates {
        // By the start first (cheap), then by everything.
        let mut by_start: BTreeMap<Vec<u8>, Vec<Entry>> = BTreeMap::new();
        for file in files {
            if cancel.is_cancelled() {
                return Err(stopped());
            }
            if let Ok(start) = start_hash(fs, &file.path) {
                by_start.entry(start).or_default().push(file);
            }
            done += 1;
            progress(Stage::Comparing(done, total));
        }
        for same_start in by_start.into_values().filter(|f| f.len() > 1) {
            let mut by_hash: BTreeMap<String, Vec<Entry>> = BTreeMap::new();
            for file in same_start {
                if cancel.is_cancelled() {
                    return Err(stopped());
                }
                // Small files were read whole already: the start is all.
                if let Ok(hash) = checksum(fs, &file.path, Algorithm::Sha256) {
                    by_hash.entry(hash).or_default().push(file);
                }
            }
            for mut same in by_hash.into_values().filter(|f| f.len() > 1) {
                same.sort_by_key(|f| (f.modified, f.path.clone()));
                groups.push(Group { size: same[0].size, files: same });
            }
        }
    }
    groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()).then_with(|| a.files[0].path.cmp(&b.files[0].path)));
    Ok(groups)
}

/// A hash of a file's first 16 KB.
fn start_hash(fs: &FsRef, path: &Path) -> io::Result<Vec<u8>> {
    let mut start = Vec::with_capacity(16 * 1024);
    fs.open_read(path)?.take(16 * 1024).read_to_end(&mut start)?;
    Ok(Sha256::digest(&start).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_files_with_the_same_contents() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("one.txt"), "same contents").unwrap();
        std::fs::write(root.join("a/two.txt"), "same contents").unwrap();
        std::fs::write(root.join("a/b/three.txt"), "same contents").unwrap();
        // Same size, other bytes.
        std::fs::write(root.join("a/other.txt"), "same contentz").unwrap();
        // Same start, different end (past the first 16 KB).
        let mut big = vec![7u8; 20_000];
        std::fs::write(root.join("big1.bin"), &big).unwrap();
        big[19_999] = 8;
        std::fs::write(root.join("big2.bin"), &big).unwrap();
        // A larger pair, and empty files (left out).
        std::fs::write(root.join("x.dat"), vec![1u8; 100]).unwrap();
        std::fs::write(root.join("a/y.dat"), vec![1u8; 100]).unwrap();
        std::fs::write(root.join("empty1"), "").unwrap();
        std::fs::write(root.join("empty2"), "").unwrap();
        // Hidden files only when asked.
        std::fs::write(root.join(".hidden.txt"), "same contents").unwrap();

        let fs = crate::vfs::local();
        let groups = find(&fs, root, false, &CancelToken::default(), &mut |_| {}).unwrap();
        let names: Vec<Vec<String>> = groups.iter().map(|g| g.files.iter().map(|f| f.name.clone()).collect()).collect();
        assert_eq!(groups.len(), 2, "{names:?}");
        // The pair of 100 bytes wastes more than three of 13.
        assert_eq!(groups[0].size, 100);
        assert_eq!(groups[0].wasted(), 100);
        let mut three = names[1].clone();
        three.sort();
        assert_eq!(three, ["one.txt", "three.txt", "two.txt"]);
        assert_eq!(groups[1].wasted(), 26);

        let groups = find(&fs, root, true, &CancelToken::default(), &mut |_| {}).unwrap();
        assert_eq!(groups[1].files.len(), 4);
    }
}
