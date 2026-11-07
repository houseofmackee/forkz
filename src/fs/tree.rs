//! A folder and everything in it as indented text (File > Export Folder
//! Tree): folders end in "/", each level is indented by two spaces.

use std::io;
use std::path::Path;

use super::ops::CancelToken;
use crate::vfs::FsRef;

/// Listings stop after this many items.
const LIMIT: usize = 200_000;

/// The tree of `root`, on any file system.
pub fn write(fs: &FsRef, root: &Path, hidden: bool, cancel: &CancelToken) -> io::Result<String> {
    let name = root.file_name().map_or_else(|| root.display().to_string(), |n| n.to_string_lossy().into_owned());
    let mut text = format!("{name}/\n");
    let mut count = 0;
    level(fs, root, 1, hidden, cancel, &mut text, &mut count)?;
    if count >= LIMIT {
        text.push_str(&format!("(stopped after {LIMIT} items)\n"));
    }
    Ok(text)
}

fn level(
    fs: &FsRef,
    folder: &Path,
    depth: usize,
    hidden: bool,
    cancel: &CancelToken,
    text: &mut String,
    count: &mut usize,
) -> io::Result<()> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
    }
    let mut entries = match fs.list(folder) {
        Ok(entries) => entries,
        // The top must be readable; folders below that are not are noted.
        Err(e) if depth == 1 => return Err(e),
        Err(_) => {
            text.push_str(&format!("{}(cannot be read)\n", "  ".repeat(depth)));
            return Ok(());
        }
    };
    entries.retain(|e| hidden || !e.hidden);
    entries.sort_by_key(|e| (!e.is_folder_like(), e.name.to_lowercase()));
    for entry in entries {
        if *count >= LIMIT {
            return Ok(());
        }
        *count += 1;
        let indent = "  ".repeat(depth);
        if entry.is_folder_like() {
            text.push_str(&format!("{indent}{}/\n", entry.name));
            level(fs, &entry.path, depth + 1, hidden, cancel, text, count)?;
        } else {
            text.push_str(&format!("{indent}{}\n", entry.name));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_an_indented_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("site");
        std::fs::create_dir_all(root.join("css")).unwrap();
        std::fs::create_dir_all(root.join("js/lib")).unwrap();
        std::fs::write(root.join("index.html"), "").unwrap();
        std::fs::write(root.join("css/style.css"), "").unwrap();
        std::fs::write(root.join("js/lib/x.js"), "").unwrap();
        std::fs::write(root.join(".env"), "").unwrap();
        let fs = crate::vfs::local();
        let text = write(&fs, &root, false, &CancelToken::default()).unwrap();
        assert_eq!(text, "site/\n  css/\n    style.css\n  js/\n    lib/\n      x.js\n  index.html\n");
        assert!(write(&fs, &root, true, &CancelToken::default()).unwrap().contains("  .env\n"));
    }
}
