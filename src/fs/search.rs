//! Searching a folder and everything in it, by name or by contents: with
//! Spotlight on the Mac (fast, for indexed folders), or by walking the
//! folders through any file system (servers and archives too).

use std::collections::VecDeque;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::Entry;
use super::glob::Patterns;
use crate::vfs::FsRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Names containing the text, or matching it if it has wildcards.
    Name,
    /// Files whose contents contain the text (ignoring case).
    Contents,
}

#[derive(Debug, Clone)]
pub struct Query {
    pub text: String,
    pub mode: Mode,
    pub include_hidden: bool,
}

/// Results stop here.
pub const LIMIT: usize = 10_000;

/// Files larger than this are not searched for contents (when walking).
const CONTENT_LIMIT: u64 = 20 * 1024 * 1024;

/// Searches by walking the folders. `cancel` stops it early (with what
/// was found so far).
pub fn walk(fs: &FsRef, root: &Path, query: &Query, cancel: &Arc<AtomicBool>) -> io::Result<Vec<Entry>> {
    let patterns = Patterns::new(&query.text);
    let needle = query.text.to_lowercase();
    let mut found = Vec::new();
    let mut folders = VecDeque::from([root.to_path_buf()]);
    let mut first = true;
    while let Some(folder) = folders.pop_front() {
        if cancel.load(Ordering::Relaxed) || found.len() >= LIMIT {
            break;
        }
        let entries = match fs.list(&folder) {
            Ok(entries) => entries,
            // The folder searched must be readable; others are skipped.
            Err(e) if first => return Err(e),
            Err(_) => continue,
        };
        first = false;
        for entry in entries {
            if entry.hidden && !query.include_hidden {
                continue;
            }
            let hit = match query.mode {
                Mode::Name => patterns.matches(&entry.name),
                Mode::Contents => !entry.is_folder_like() && contains(fs, &entry, &needle),
            };
            if entry.is_folder_like() {
                folders.push_back(entry.path.clone());
            }
            if hit {
                found.push(entry);
            }
        }
    }
    Ok(found)
}

/// Whether a file contains the (lowercase) text.
fn contains(fs: &FsRef, entry: &Entry, needle: &str) -> bool {
    if needle.is_empty() || entry.size > CONTENT_LIMIT {
        return false;
    }
    let mut data = Vec::new();
    let Ok(mut reader) = fs.open_read(&entry.path) else { return false };
    if reader.read_to_end(&mut data).is_err() {
        return false;
    }
    String::from_utf8_lossy(&data).to_lowercase().contains(needle)
}

/// The Spotlight query for `mdfind` (a raw query expression).
pub fn spotlight_query(query: &Query) -> String {
    let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    match query.mode {
        Mode::Name => {
            let names: Vec<String> = query
                .text
                .split([';', ','])
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(|p| if p.contains(['*', '?']) { p.to_string() } else { format!("*{p}*") })
                .map(|p| format!("kMDItemFSName == \"{}\"cd", escape(&p)))
                .collect();
            names.join(" || ")
        }
        Mode::Contents => format!("kMDItemTextContent == \"*{}*\"cd", escape(query.text.trim())),
    }
}

/// Everything Spotlight knows with this Finder tag, on every volume.
pub fn spotlight_tag(tag: &str) -> io::Result<Vec<PathBuf>> {
    let query = format!("kMDItemUserTags == \"{}\"cd", tag.replace('\\', "\\\\").replace('"', "\\\""));
    let output = Command::new("/usr/bin/mdfind").arg(query).output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_string()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .take(LIMIT)
        .collect())
}

/// Searches with Spotlight (only what it has indexed). Returns paths.
pub fn spotlight(root: &Path, query: &Query) -> io::Result<Vec<PathBuf>> {
    let output = Command::new("/usr/bin/mdfind").arg("-onlyin").arg(root).arg(spotlight_query(query)).output()?;
    if !output.status.success() {
        return Err(io::Error::other(String::from_utf8_lossy(&output.stderr).trim().to_string()));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .filter(|p| p != root)
        .filter(|p| {
            query.include_hidden
                || !p
                    .strip_prefix(root)
                    .unwrap_or(p)
                    .components()
                    .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        })
        .take(LIMIT)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("a/deep")).unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join("notes.txt"), "Remember the Milk").unwrap();
        std::fs::write(root.join("a/report.pdf"), "binary-ish").unwrap();
        std::fs::write(root.join("a/deep/Notes 2.md"), "nothing here").unwrap();
        std::fs::write(root.join(".hidden/notes.txt"), "milk").unwrap();
        dir
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        let mut names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        names.sort();
        names
    }

    #[test]
    fn walks_by_name_and_contents() {
        let dir = tree();
        let fs = crate::vfs::local();
        let cancel = Arc::new(AtomicBool::new(false));
        let by_name = Query { text: "notes".into(), mode: Mode::Name, include_hidden: false };
        assert_eq!(names(&walk(&fs, dir.path(), &by_name, &cancel).unwrap()), ["Notes 2.md", "notes.txt"]);
        let hidden = Query { include_hidden: true, ..by_name.clone() };
        assert_eq!(walk(&fs, dir.path(), &hidden, &cancel).unwrap().len(), 3);
        let pattern = Query { text: "*.pdf; deep".into(), mode: Mode::Name, include_hidden: false };
        assert_eq!(names(&walk(&fs, dir.path(), &pattern, &cancel).unwrap()), ["deep", "report.pdf"]);
        let contents = Query { text: "MILK".into(), mode: Mode::Contents, include_hidden: false };
        assert_eq!(names(&walk(&fs, dir.path(), &contents, &cancel).unwrap()), ["notes.txt"]);
        assert!(walk(&fs, &dir.path().join("missing"), &by_name, &cancel).is_err());
        cancel.store(true, Ordering::Relaxed);
        assert!(walk(&fs, dir.path(), &by_name, &cancel).unwrap().is_empty());
    }

    #[test]
    fn spotlight_queries() {
        let q = Query { text: "*.jpg; notes".into(), mode: Mode::Name, include_hidden: false };
        assert_eq!(spotlight_query(&q), r#"kMDItemFSName == "*.jpg"cd || kMDItemFSName == "*notes*"cd"#);
        let q = Query { text: r#"say "hi""#.into(), mode: Mode::Contents, include_hidden: false };
        assert_eq!(spotlight_query(&q), r#"kMDItemTextContent == "*say \"hi\"*"cd"#);
    }
}
