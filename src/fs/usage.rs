//! Disk usage: a folder and everything in it as a tree, each folder with
//! the total size of what is inside, largest first. Folders keep their
//! largest items; the rest are summed up in one "smaller items" entry, so
//! huge folders stay quick to show.

use std::io;
use std::path::{Path, PathBuf};

use super::EntryKind;
use super::ops::CancelToken;
use crate::vfs::FsRef;

/// Items kept in each folder (largest first); the rest are summed up.
const KEEP: usize = 300;

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub path: PathBuf,
    /// Bytes (for a folder: everything in it).
    pub size: u64,
    /// Files in it (1 for a file).
    pub files: u64,
    pub folder: bool,
    /// Several items summed up (not a real item).
    pub summary: bool,
    /// By index into `Usage::nodes`, largest first.
    pub children: Vec<usize>,
}

/// The measured tree; the folder measured is node 0.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub nodes: Vec<Node>,
    /// Folders that could not be read.
    pub unreadable: usize,
}

/// Measures `root` and everything in it. `progress` gets the number of
/// files seen so far (now and then). `cancel` stops it (an Interrupted
/// error).
pub fn measure(
    fs: &FsRef,
    root: &Path,
    hidden: bool,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u64),
) -> io::Result<Usage> {
    // The folder measured must be readable.
    fs.list(root)?;
    let mut usage = Usage::default();
    let mut seen = 0u64;
    let name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
    folder(fs, root, name, hidden, cancel, progress, &mut usage, &mut seen)?;
    Ok(usage)
}

#[allow(clippy::too_many_arguments)]
fn folder(
    fs: &FsRef,
    path: &Path,
    name: String,
    hidden: bool,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(u64),
    usage: &mut Usage,
    seen: &mut u64,
) -> io::Result<usize> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
    }
    let index = usage.nodes.len();
    usage.nodes.push(Node {
        name,
        path: path.to_path_buf(),
        size: 0,
        files: 0,
        folder: true,
        summary: false,
        children: Vec::new(),
    });
    let entries = match fs.list(path) {
        Ok(entries) => entries,
        Err(_) => {
            usage.unreadable += 1;
            return Ok(index);
        }
    };
    let mut children = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.hidden && !hidden {
            continue;
        }
        // Packages (apps) are folders here: they take space too. Links
        // are not followed.
        let child = if entry.kind == EntryKind::Folder {
            folder(fs, &entry.path, entry.name.clone(), hidden, cancel, progress, usage, seen)?
        } else {
            *seen += 1;
            if seen.is_multiple_of(2_000) {
                progress(*seen);
            }
            let size = if matches!(entry.kind, EntryKind::Symlink { .. }) { 0 } else { entry.size };
            usage.nodes.push(Node {
                name: entry.name.clone(),
                path: entry.path.clone(),
                size,
                files: 1,
                folder: false,
                summary: false,
                children: Vec::new(),
            });
            usage.nodes.len() - 1
        };
        children.push(child);
    }
    children.sort_by(|a, b| usage.nodes[*b].size.cmp(&usage.nodes[*a].size));
    let (size, files) = children.iter().fold((0, 0), |(s, f), c| (s + usage.nodes[*c].size, f + usage.nodes[*c].files));
    if children.len() > KEEP {
        let rest = children.split_off(KEEP);
        let (rest_size, rest_files) =
            rest.iter().fold((0, 0), |(s, f), c| (s + usage.nodes[*c].size, f + usage.nodes[*c].files));
        usage.nodes.push(Node {
            name: format!("{} smaller items", rest.len()),
            path: path.to_path_buf(),
            size: rest_size,
            files: rest_files,
            folder: false,
            summary: true,
            children: Vec::new(),
        });
        children.push(usage.nodes.len() - 1);
    }
    let node = &mut usage.nodes[index];
    node.size = size;
    node.files = files;
    node.children = children;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_a_tree_largest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("big/deeper")).unwrap();
        std::fs::create_dir_all(root.join("empty")).unwrap();
        std::fs::write(root.join("small.txt"), vec![0u8; 10]).unwrap();
        std::fs::write(root.join("big/a.bin"), vec![0u8; 1000]).unwrap();
        std::fs::write(root.join("big/deeper/b.bin"), vec![0u8; 500]).unwrap();
        std::fs::write(root.join(".hidden"), vec![0u8; 5000]).unwrap();
        let fs = crate::vfs::local();
        let usage = measure(&fs, &root, false, &CancelToken::default(), &mut |_| {}).unwrap();
        let top = &usage.nodes[0];
        assert_eq!((top.size, top.files), (1510, 3));
        let names: Vec<&str> = top.children.iter().map(|c| usage.nodes[*c].name.as_str()).collect();
        assert_eq!(names, ["big", "small.txt", "empty"]);
        let big = &usage.nodes[top.children[0]];
        assert_eq!((big.size, big.files, big.folder), (1500, 2, true));
        // Hidden items when asked.
        let all = measure(&fs, &root, true, &CancelToken::default(), &mut |_| {}).unwrap();
        assert_eq!(all.nodes[0].size, 6510);
        assert_eq!(all.nodes[all.nodes[0].children[0]].name, ".hidden");
    }

    #[test]
    fn sums_up_the_smallest_of_many() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..(KEEP + 5) {
            std::fs::write(dir.path().join(format!("f{i:04}")), vec![0u8; i + 1]).unwrap();
        }
        let fs = crate::vfs::local();
        let usage = measure(&fs, dir.path(), false, &CancelToken::default(), &mut |_| {}).unwrap();
        let top = &usage.nodes[0];
        assert_eq!(top.children.len(), KEEP + 1);
        let rest = &usage.nodes[*top.children.last().unwrap()];
        assert!(rest.summary);
        assert_eq!(rest.name, "5 smaller items");
        assert_eq!(rest.size, 1 + 2 + 3 + 4 + 5);
        assert_eq!(top.files, (KEEP + 5) as u64);
    }
}
