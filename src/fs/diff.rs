//! Comparing: two texts line by line (with the changed words within
//! changed lines), two folders and everything in them, and the items of
//! two listings (Compare Panes).

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use similar::{Algorithm, DiffTag, capture_diff_slices};

use super::Entry;
use super::ops::CancelToken;
use crate::vfs::FsRef;

/// Dates closer than this count as equal (FAT and FTP keep two-second or
/// one-minute dates).
const TOLERANCE: Duration = Duration::from_secs(2);

// Texts.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Same,
    /// Different on the two sides.
    Changed,
    /// Only on the left.
    Removed,
    /// Only on the right.
    Added,
}

/// One row of the side-by-side view: a line of each text (by index), or a
/// gap on the side that has no line there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub kind: LineKind,
    pub left: Option<usize>,
    pub right: Option<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct TextDiff {
    pub left: Vec<String>,
    pub right: Vec<String>,
    pub rows: Vec<Row>,
    /// The first row of each change (a run of rows that are not the same).
    pub changes: Vec<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextOptions {
    /// Lines that differ only in spaces and tabs count as the same.
    pub ignore_whitespace: bool,
    pub ignore_case: bool,
}

impl TextDiff {
    /// How many lines are changed, removed, and added.
    pub fn counts(&self) -> (usize, usize, usize) {
        let count = |kind| self.rows.iter().filter(|r| r.kind == kind).count();
        (count(LineKind::Changed), count(LineKind::Removed), count(LineKind::Added))
    }
}

/// Tabs as spaces (to the next multiple of four), so lines line up.
fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 8);
    let mut column = 0;
    for c in line.chars() {
        if c == '\t' {
            let spaces = 4 - column % 4;
            out.extend(std::iter::repeat_n(' ', spaces));
            column += spaces;
        } else {
            out.push(c);
            column += 1;
        }
    }
    out
}

/// What a line is compared as.
fn key(line: &str, options: TextOptions) -> String {
    let line =
        if options.ignore_whitespace { line.split_whitespace().collect::<Vec<_>>().join(" ") } else { line.into() };
    if options.ignore_case { line.to_lowercase() } else { line }
}

/// Compares two texts line by line.
pub fn diff_texts(left: &str, right: &str, options: TextOptions) -> TextDiff {
    let left: Vec<String> = left.lines().map(expand_tabs).collect();
    let right: Vec<String> = right.lines().map(expand_tabs).collect();
    let left_keys: Vec<String> = left.iter().map(|l| key(l, options)).collect();
    let right_keys: Vec<String> = right.iter().map(|l| key(l, options)).collect();
    let mut rows = Vec::with_capacity(left.len().max(right.len()));
    for op in capture_diff_slices(Algorithm::Myers, &left_keys, &right_keys) {
        let (tag, old, new) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => {
                rows.extend(old.zip(new).map(|(l, r)| Row { kind: LineKind::Same, left: Some(l), right: Some(r) }))
            }
            DiffTag::Delete => rows.extend(old.map(|l| Row { kind: LineKind::Removed, left: Some(l), right: None })),
            DiffTag::Insert => rows.extend(new.map(|r| Row { kind: LineKind::Added, left: None, right: Some(r) })),
            DiffTag::Replace => {
                // Paired up line by line; the longer side's rest is removed
                // or added.
                let (old, new): (Vec<usize>, Vec<usize>) = (old.collect(), new.collect());
                for i in 0..old.len().max(new.len()) {
                    let (l, r) = (old.get(i).copied(), new.get(i).copied());
                    let kind = match (l, r) {
                        (Some(_), Some(_)) => LineKind::Changed,
                        (Some(_), None) => LineKind::Removed,
                        _ => LineKind::Added,
                    };
                    rows.push(Row { kind, left: l, right: r });
                }
            }
        }
    }
    let changes = rows
        .iter()
        .enumerate()
        .filter(|(i, row)| row.kind != LineKind::Same && (*i == 0 || rows[i - 1].kind == LineKind::Same))
        .map(|(i, _)| i)
        .collect();
    TextDiff { left, right, rows, changes }
}

/// Words, runs of spaces, and single other characters: what changed lines
/// are compared by.
fn tokens(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start = 0;
    let class = |c: char| {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let kind = class(c);
        let next = chars.peek().map(|(_, n)| class(*n));
        if kind == 2 || next != Some(kind) {
            tokens.push(&line[start..i + c.len_utf8()]);
            start = i + c.len_utf8();
        }
    }
    tokens
}

/// The parts of two changed lines that differ, as UTF-16 ranges (for
/// highlighting in AppKit text).
pub fn changed_parts(left: &str, right: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (a, b) = (tokens(left), tokens(right));
    let offsets = |tokens: &[&str]| -> Vec<usize> {
        let mut at = 0;
        let mut offsets = vec![0];
        for t in tokens {
            at += t.encode_utf16().count();
            offsets.push(at);
        }
        offsets
    };
    let (oa, ob) = (offsets(&a), offsets(&b));
    let (mut left_parts, mut right_parts) = (Vec::new(), Vec::new());
    let add = |parts: &mut Vec<Range<usize>>, range: Range<usize>| {
        if range.is_empty() {
            return;
        }
        match parts.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => parts.push(range),
        }
    };
    for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
        let (tag, old, new) = op.as_tag_tuple();
        if tag != DiffTag::Equal {
            add(&mut left_parts, oa[old.start]..oa[old.end]);
            add(&mut right_parts, ob[new.start]..ob[new.end]);
        }
    }
    (left_parts, right_parts)
}

/// Where two byte strings first differ (None: they are the same).
pub fn first_difference(left: &[u8], right: &[u8]) -> Option<usize> {
    match left.iter().zip(right).position(|(a, b)| a != b) {
        Some(at) => Some(at),
        None if left.len() != right.len() => Some(left.len().min(right.len())),
        None => None,
    }
}

// Folders.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Same,
    /// A folder on both sides with differences inside.
    Inside,
    DifferentSize,
    DifferentContents,
    /// Same size, but newer on one side (contents not compared).
    LeftNewer,
    RightNewer,
    OnlyLeft,
    OnlyRight,
    /// A file on one side, a folder on the other.
    KindDiffers,
}

impl Status {
    pub fn describe(self) -> &'static str {
        match self {
            Status::Same => "Same",
            Status::Inside => "Differences inside",
            Status::DifferentSize => "Different size",
            Status::DifferentContents => "Different contents",
            Status::LeftNewer => "Newer on the left",
            Status::RightNewer => "Newer on the right",
            Status::OnlyLeft => "Only on the left",
            Status::OnlyRight => "Only on the right",
            Status::KindDiffers => "File on one side, folder on the other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FolderOptions {
    /// Compare files of the same size byte by byte (instead of by date).
    pub contents: bool,
    pub hidden: bool,
}

/// One item of a folder comparison, in order (a folder before what is in
/// it).
#[derive(Debug, Clone)]
pub struct FolderItem {
    /// Relative to both folders compared.
    pub path: PathBuf,
    pub depth: usize,
    pub folder: bool,
    pub status: Status,
    pub left: Option<Entry>,
    pub right: Option<Entry>,
}

/// Compares two folders and everything in them. `cancel` stops it (an
/// Interrupted error).
pub fn compare_folders(
    left: &FsRef,
    left_root: &Path,
    right: &FsRef,
    right_root: &Path,
    options: FolderOptions,
    cancel: &CancelToken,
) -> io::Result<Vec<FolderItem>> {
    let mut items = Vec::new();
    compare_level(left, left_root, right, right_root, Path::new(""), 0, options, cancel, &mut items)?;
    Ok(items)
}

/// One folder level; returns whether anything in it differs.
#[allow(clippy::too_many_arguments)]
fn compare_level(
    left: &FsRef,
    left_root: &Path,
    right: &FsRef,
    right_root: &Path,
    relative: &Path,
    depth: usize,
    options: FolderOptions,
    cancel: &CancelToken,
    items: &mut Vec<FolderItem>,
) -> io::Result<bool> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
    }
    let list = |fs: &FsRef, root: &Path| -> io::Result<BTreeMap<String, Entry>> {
        Ok(fs
            .list(&root.join(relative))?
            .into_iter()
            .filter(|e| options.hidden || !e.hidden)
            .map(|e| (e.name.clone(), e))
            .collect())
    };
    let (mut lefts, mut rights) = (list(left, left_root)?, list(right, right_root)?);
    let mut names: Vec<String> = lefts.keys().chain(rights.keys()).cloned().collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    // Folders first, as in the lists.
    let is_folder =
        |name: &String| lefts.get(name).or(rights.get(name)).is_some_and(|e: &Entry| e.is_folder_like() && !e.package);
    names.sort_by_key(|n| !is_folder(n));
    let mut differs = false;
    for name in names {
        let (l, r) = (lefts.remove(&name), rights.remove(&name));
        let path = relative.join(&name);
        let folder = l.as_ref().or(r.as_ref()).is_some_and(Entry::is_folder_like);
        let status = match (&l, &r) {
            (Some(_), None) => Status::OnlyLeft,
            (None, Some(_)) => Status::OnlyRight,
            (Some(a), Some(b)) if a.is_folder_like() != b.is_folder_like() => Status::KindDiffers,
            (Some(_), Some(_)) if folder => {
                let at = items.len();
                items.push(FolderItem { path: path.clone(), depth, folder, status: Status::Same, left: l, right: r });
                let inside =
                    compare_level(left, left_root, right, right_root, &path, depth + 1, options, cancel, items)?;
                if inside {
                    items[at].status = Status::Inside;
                    differs = true;
                }
                continue;
            }
            (Some(a), Some(b)) => {
                compare_files(left, &left_root.join(&path), a, right, &right_root.join(&path), b, options)?
            }
            (None, None) => continue,
        };
        differs |= status != Status::Same;
        items.push(FolderItem { path, depth, folder, status, left: l, right: r });
    }
    Ok(differs)
}

fn compare_files(
    left: &FsRef,
    left_path: &Path,
    a: &Entry,
    right: &FsRef,
    right_path: &Path,
    b: &Entry,
    options: FolderOptions,
) -> io::Result<Status> {
    if a.size != b.size {
        return Ok(Status::DifferentSize);
    }
    if options.contents {
        return Ok(if same_bytes(left, left_path, right, right_path)? {
            Status::Same
        } else {
            Status::DifferentContents
        });
    }
    Ok(match (a.modified, b.modified) {
        (Some(x), Some(y)) if x > y + TOLERANCE => Status::LeftNewer,
        (Some(x), Some(y)) if y > x + TOLERANCE => Status::RightNewer,
        _ => Status::Same,
    })
}

/// Whether two files have the same bytes (read side by side).
fn same_bytes(left: &FsRef, left_path: &Path, right: &FsRef, right_path: &Path) -> io::Result<bool> {
    let (mut a, mut b) = (left.open_read(left_path)?, right.open_read(right_path)?);
    let (mut x, mut y) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let n = read_full(&mut a, &mut x)?;
        let m = read_full(&mut b, &mut y)?;
        if n != m || x[..n] != y[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// Fills the buffer as far as the reader goes.
fn read_full(reader: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

// Listings (Compare Panes).

/// The items of two listings that differ: on each side, what the other
/// side does not have, files that are newer than the other side's, and
/// files of the same date but a different size (on both sides). Folders on
/// both sides are not looked into.
pub fn compare_listings(left: &[Entry], right: &[Entry]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let by_name = |entries: &[Entry]| -> BTreeMap<String, Entry> {
        entries.iter().map(|e| (e.name.clone(), e.clone())).collect()
    };
    let (lefts, rights) = (by_name(left), by_name(right));
    let (mut on_left, mut on_right) = (Vec::new(), Vec::new());
    for (name, a) in &lefts {
        match rights.get(name) {
            None => on_left.push(a.path.clone()),
            Some(b) if a.is_folder_like() && b.is_folder_like() => {}
            Some(b) if a.is_folder_like() != b.is_folder_like() => {
                on_left.push(a.path.clone());
                on_right.push(b.path.clone());
            }
            Some(b) => match (a.modified, b.modified) {
                (Some(x), Some(y)) if x > y + TOLERANCE => on_left.push(a.path.clone()),
                (Some(x), Some(y)) if y > x + TOLERANCE => on_right.push(b.path.clone()),
                _ if a.size != b.size => {
                    on_left.push(a.path.clone());
                    on_right.push(b.path.clone());
                }
                _ => {}
            },
        }
    }
    on_right.extend(rights.iter().filter(|(name, _)| !lefts.contains_key(*name)).map(|(_, e)| e.path.clone()));
    (on_left, on_right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::EntryKind;
    use std::time::SystemTime;

    #[test]
    fn diffs_lines_side_by_side() {
        let left = "one\ntwo\nthree\nfour\n";
        let right = "one\n2\nthree\nfour\nfive\n";
        let diff = diff_texts(left, right, TextOptions::default());
        let kinds: Vec<LineKind> = diff.rows.iter().map(|r| r.kind).collect();
        use LineKind::*;
        assert_eq!(kinds, [Same, Changed, Same, Same, Added]);
        assert_eq!(diff.changes, [1, 4]);
        assert_eq!(diff.counts(), (1, 0, 1));
        assert_eq!(diff.rows[4], Row { kind: Added, left: None, right: Some(4) });
        // A longer replacement: paired, then the rest added.
        let diff = diff_texts("a\nb\nc", "a\nx\ny\nz\nc", TextOptions::default());
        let kinds: Vec<LineKind> = diff.rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [Same, Changed, Added, Added, Same]);
        assert_eq!(diff.changes, [1]);
        // Identical texts.
        assert!(diff_texts("a\nb", "a\nb", TextOptions::default()).changes.is_empty());
    }

    #[test]
    fn ignores_whitespace_and_case_when_asked() {
        let options = TextOptions { ignore_whitespace: true, ignore_case: true };
        let diff = diff_texts("Hello  World\n\tx = 1", "hello world\nx =  1", options);
        assert!(diff.changes.is_empty());
        // The lines shown are the real ones, tabs expanded.
        assert_eq!(diff.left[1], "    x = 1");
        assert!(!diff_texts("a b", "a  b", TextOptions::default()).changes.is_empty());
    }

    #[test]
    fn finds_changed_words() {
        let (l, r) = changed_parts("let total = price * 2;", "let total = cost * 3;");
        assert_eq!(l, [12..17, 20..21]);
        assert_eq!(r, [12..16, 19..20]);
        // UTF-16 offsets: an emoji is two units.
        let (l, r) = changed_parts("\u{1F600} a", "\u{1F600} b");
        assert_eq!((l.len(), l[0].clone(), r.len(), r[0].clone()), (1, 3..4, 1, 3..4));
        let (l, r) = changed_parts("same", "same");
        assert!(l.is_empty() && r.is_empty());
    }

    #[test]
    fn finds_the_first_differing_byte() {
        assert_eq!(first_difference(b"abc", b"abc"), None);
        assert_eq!(first_difference(b"abc", b"abd"), Some(2));
        assert_eq!(first_difference(b"ab", b"abc"), Some(2));
    }

    fn entry(name: &str, folder: bool, size: u64, secs: u64) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from("/x").join(name),
            kind: if folder { EntryKind::Folder } else { EntryKind::File },
            size,
            modified: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs)),
            hidden: false,
            package: false,
            permissions: None,
            owner: None,
            group: None,
        }
    }

    #[test]
    fn compares_listings() {
        let left = [
            entry("only-left.txt", false, 1, 100),
            entry("same.txt", false, 5, 100),
            entry("newer-left.txt", false, 5, 200),
            entry("newer-right.txt", false, 5, 100),
            entry("size.txt", false, 5, 100),
            entry("both", true, 0, 100),
            entry("kind", true, 0, 100),
        ];
        let right = [
            entry("only-right", true, 0, 100),
            entry("same.txt", false, 5, 101),
            entry("newer-left.txt", false, 5, 100),
            entry("newer-right.txt", false, 9, 300),
            entry("size.txt", false, 6, 100),
            entry("both", true, 0, 999),
            entry("kind", false, 3, 100),
        ];
        let names = |paths: Vec<PathBuf>| {
            let mut n: Vec<String> = paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().into()).collect();
            n.sort();
            n
        };
        let (l, r) = compare_listings(&left, &right);
        assert_eq!(names(l), ["kind", "newer-left.txt", "only-left.txt", "size.txt"]);
        assert_eq!(names(r), ["kind", "newer-right.txt", "only-right", "size.txt"]);
    }

    #[test]
    fn compares_folders() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        for root in [&a, &b] {
            std::fs::create_dir_all(root.join("sub/deep")).unwrap();
            std::fs::create_dir_all(root.join("calm")).unwrap();
            std::fs::write(root.join("same.txt"), "same").unwrap();
            std::fs::write(root.join("calm/x.txt"), "x").unwrap();
        }
        std::fs::write(a.join("sub/deep/changed.txt"), "left").unwrap();
        std::fs::write(b.join("sub/deep/changed.txt"), "rite").unwrap();
        std::fs::write(a.join("size.txt"), "1").unwrap();
        std::fs::write(b.join("size.txt"), "22").unwrap();
        std::fs::write(a.join("left-only.txt"), "").unwrap();
        std::fs::create_dir(b.join("right-only")).unwrap();
        let fs = crate::vfs::local();
        let options = FolderOptions { contents: true, hidden: false };
        let items = compare_folders(&fs, &a, &fs, &b, options, &CancelToken::default()).unwrap();
        let found: Vec<(String, Status, usize)> =
            items.iter().map(|i| (i.path.to_string_lossy().into_owned(), i.status, i.depth)).collect();
        let expected = [
            ("calm", Status::Same, 0),
            ("calm/x.txt", Status::Same, 1),
            ("right-only", Status::OnlyRight, 0),
            ("sub", Status::Inside, 0),
            ("sub/deep", Status::Inside, 1),
            ("sub/deep/changed.txt", Status::DifferentContents, 2),
            ("left-only.txt", Status::OnlyLeft, 0),
            ("same.txt", Status::Same, 0),
            ("size.txt", Status::DifferentSize, 0),
        ];
        let expected: Vec<(String, Status, usize)> = expected.iter().map(|(p, s, d)| (p.to_string(), *s, *d)).collect();
        assert_eq!(found, expected);
    }
}
