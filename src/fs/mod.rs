//! The file system as the panes see it: listing folders, sorting entries,
//! and formatting sizes. Pure Rust, so it can be tested without AppKit.

pub mod checksum;
pub mod glob;
pub mod ops;
pub mod search;
pub mod tags;

use std::cmp::Ordering;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// One item in a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub kind: EntryKind,
    /// The size in bytes; 0 for folders (their size is not calculated).
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Starts with a dot, or has the hidden flag (like `~/Library`).
    pub hidden: bool,
    /// A folder macOS shows as a single file, such as an app.
    pub package: bool,
    /// Unix permission bits, if known.
    pub permissions: Option<u32>,
    /// Owner and group names (or numbers when the name is unknown).
    pub owner: Option<String>,
    pub group: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Folder,
    /// A symbolic link; `target_is_folder` tells how to open it.
    Symlink {
        target_is_folder: bool,
    },
}

impl Entry {
    /// Opens like a folder (into the pane) rather than like a document.
    pub fn is_folder_like(&self) -> bool {
        !self.package && matches!(self.kind, EntryKind::Folder | EntryKind::Symlink { target_is_folder: true })
    }

    /// The extension, lowercased, or "" (folders have none).
    pub fn extension(&self) -> String {
        if self.is_folder_like() {
            return String::new();
        }
        Path::new(&self.name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
    }

    pub fn from_path(path: &Path) -> io::Result<Entry> {
        let meta = std::fs::symlink_metadata(path)?;
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let file_type = meta.file_type();
        let kind = if file_type.is_symlink() {
            EntryKind::Symlink { target_is_folder: std::fs::metadata(path).is_ok_and(|m| m.is_dir()) }
        } else if file_type.is_dir() {
            EntryKind::Folder
        } else {
            EntryKind::File
        };
        let hidden = name.starts_with('.') || has_hidden_flag(&meta);
        let package = kind == EntryKind::Folder && is_package_name(&name);
        use std::os::unix::fs::PermissionsExt;
        Ok(Entry {
            permissions: Some(meta.permissions().mode() & 0o7777),
            owner: Some(user_name(std::os::unix::fs::MetadataExt::uid(&meta))),
            group: Some(group_name(std::os::unix::fs::MetadataExt::gid(&meta))),
            size: if kind == EntryKind::File { meta.len() } else { 0 },
            modified: meta.modified().ok(),
            name,
            path: path.to_path_buf(),
            kind,
            hidden,
            package,
        })
    }
}

fn has_hidden_flag(meta: &std::fs::Metadata) -> bool {
    use std::os::darwin::fs::MetadataExt;
    meta.st_flags() & libc::UF_HIDDEN != 0
}

/// Folders with these extensions are bundles that open as one item. (Launch
/// Services knows more; these are the common ones.)
pub fn is_package_name(name: &str) -> bool {
    const PACKAGES: &[&str] = &[
        "app",
        "bundle",
        "framework",
        "plugin",
        "kext",
        "pkg",
        "mpkg",
        "rtfd",
        "photoslibrary",
        "musiclibrary",
        "xcodeproj",
        "xcworkspace",
        "playground",
        "pages",
        "numbers",
        "key",
        "band",
        "logicx",
        "fcpbundle",
        "lproj",
        "appex",
        "xpc",
        "prefpane",
        "qlgenerator",
        "mdimporter",
        "saver",
        "theater",
        "imovielibrary",
    ];
    Path::new(name).extension().is_some_and(|e| {
        let e = e.to_string_lossy().to_lowercase();
        e != "lproj" && PACKAGES.contains(&e.as_str())
    })
}

/// Lists a folder. Unreadable entries are left out; an unreadable folder is
/// an error.
pub fn list_dir(path: &Path) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in std::fs::read_dir(path)? {
        let Ok(item) = item else { continue };
        if let Ok(entry) = Entry::from_path(&item.path()) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SortKey {
    #[default]
    Name,
    Size,
    Modified,
    Kind,
}

impl SortKey {
    pub fn id(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Size => "size",
            SortKey::Modified => "modified",
            SortKey::Kind => "kind",
        }
    }

    pub fn from_id(id: &str) -> Option<SortKey> {
        [SortKey::Name, SortKey::Size, SortKey::Modified, SortKey::Kind].into_iter().find(|k| k.id() == id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sort {
    pub key: SortKey,
    pub ascending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort { key: SortKey::Name, ascending: true }
    }
}

/// Sorts like Finder: folders first, then by the key; ties by name.
/// Descending order reverses the key, but folders stay first.
pub fn sort_entries(entries: &mut [Entry], sort: Sort) {
    entries.sort_by(|a, b| {
        let folders = b.is_folder_like().cmp(&a.is_folder_like());
        if folders != Ordering::Equal {
            return folders;
        }
        let by_key = match sort.key {
            SortKey::Name => Ordering::Equal,
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Kind => a.extension().cmp(&b.extension()),
        };
        let ordering = by_key.then_with(|| natural_cmp(&a.name, &b.name));
        if sort.ascending { ordering } else { ordering.reverse() }
    });
}

/// Compares names like Finder: case-insensitive, with runs of digits
/// compared as numbers ("file 2" before "file 10").
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut a = a.chars().peekable();
    let mut b = b.chars().peekable();
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        it.next();
                    }
                    digits
                };
                let (da, db) = (take(&mut a), take(&mut b));
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let ordering = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ordering != Ordering::Equal {
                    return ordering;
                }
            }
            (Some(x), Some(y)) => {
                let ordering = x.to_lowercase().cmp(y.to_lowercase());
                if ordering != Ordering::Equal {
                    return ordering;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// A size as Finder shows it (decimal units): "0 bytes", "1 byte",
/// "12 KB", "3.4 MB".
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["KB", "MB", "GB", "TB", "PB"];
    match bytes {
        1 => "1 byte".to_string(),
        0..1000 => format!("{bytes} bytes"),
        _ => {
            let mut value = bytes as f64 / 1000.0;
            let mut unit = 0;
            while value >= 999.95 && unit < UNITS.len() - 1 {
                value /= 1000.0;
                unit += 1;
            }
            if unit == 0 || value >= 100.0 {
                format!("{value:.0} {}", UNITS[unit])
            } else {
                format!("{value:.1} {}", UNITS[unit])
            }
        }
    }
}

/// Permissions as `ls -l` shows them: "drwxr-xr-x", "-rw-r--r--",
/// "lrwxrwxrwx" (with s/S and t/T for setuid, setgid, and sticky).
pub fn format_permissions(entry: &Entry) -> String {
    let Some(mode) = entry.permissions else { return String::new() };
    let kind = match entry.kind {
        EntryKind::Folder => 'd',
        EntryKind::Symlink { .. } => 'l',
        EntryKind::File => '-',
    };
    let mut text = String::with_capacity(10);
    text.push(kind);
    for (shift, special, special_char) in [(6, 0o4000, 's'), (3, 0o2000, 's'), (0, 0o1000, 't')] {
        let bits = (mode >> shift) & 0o7;
        text.push(if bits & 4 != 0 { 'r' } else { '-' });
        text.push(if bits & 2 != 0 { 'w' } else { '-' });
        let execute = bits & 1 != 0;
        text.push(match (mode & special != 0, execute) {
            (true, true) => special_char,
            (true, false) => special_char.to_ascii_uppercase(),
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    text
}

/// The name of a local user (its number if it has none), cached.
pub fn user_name(uid: u32) -> String {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static NAMES: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    names
        .get_or_insert_with(HashMap::new)
        .entry(uid)
        .or_insert_with(|| {
            let mut buffer = vec![0 as libc::c_char; 4096];
            let mut record: libc::passwd = unsafe { std::mem::zeroed() };
            let mut result: *mut libc::passwd = std::ptr::null_mut();
            // SAFETY: valid buffers; the name is copied before they go away.
            let found = unsafe {
                libc::getpwuid_r(uid, &mut record, buffer.as_mut_ptr(), buffer.len(), &mut result) == 0
                    && !result.is_null()
            };
            if found {
                unsafe { std::ffi::CStr::from_ptr(record.pw_name) }.to_string_lossy().into_owned()
            } else {
                uid.to_string()
            }
        })
        .clone()
}

/// The name of a local group (its number if it has none), cached.
pub fn group_name(gid: u32) -> String {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static NAMES: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);
    let mut names = NAMES.lock().unwrap_or_else(|e| e.into_inner());
    names
        .get_or_insert_with(HashMap::new)
        .entry(gid)
        .or_insert_with(|| {
            let mut buffer = vec![0 as libc::c_char; 4096];
            let mut record: libc::group = unsafe { std::mem::zeroed() };
            let mut result: *mut libc::group = std::ptr::null_mut();
            // SAFETY: valid buffers; the name is copied before they go away.
            let found = unsafe {
                libc::getgrgid_r(gid, &mut record, buffer.as_mut_ptr(), buffer.len(), &mut result) == 0
                    && !result.is_null()
            };
            if found {
                unsafe { std::ffi::CStr::from_ptr(record.gr_name) }.to_string_lossy().into_owned()
            } else {
                gid.to_string()
            }
        })
        .clone()
}

/// The space available on the volume holding `path`, in bytes.
pub fn available_space(path: &Path) -> Option<u64> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a zeroed struct for the result.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    Some(stats.f_bavail * u64::from(stats.f_bsize))
}

/// The device a path is on, to tell whether a move can be a rename.
pub fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).ok().map(|m| m.dev())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from("/x").join(name),
            kind,
            size,
            modified: None,
            hidden: false,
            package: is_package_name(name),
            permissions: None,
            owner: None,
            group: None,
        }
    }

    #[test]
    fn natural_order() {
        let mut names = vec!["file 10", "File 2", "file 1", "apple", "file 02b"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["apple", "file 1", "File 2", "file 02b", "file 10"]);
    }

    #[test]
    fn folders_first_even_descending() {
        let mut entries = vec![
            entry("b.txt", EntryKind::File, 5),
            entry("Zed", EntryKind::Folder, 0),
            entry("a.txt", EntryKind::File, 50),
            entry("Mail.app", EntryKind::Folder, 0),
        ];
        sort_entries(&mut entries, Sort { key: SortKey::Name, ascending: false });
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Zed", "Mail.app", "b.txt", "a.txt"]);
        sort_entries(&mut entries, Sort { key: SortKey::Size, ascending: true });
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        // Apps sort as files (packages), by size 0.
        assert_eq!(names, ["Zed", "Mail.app", "b.txt", "a.txt"]);
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(0), "0 bytes");
        assert_eq!(format_size(1), "1 byte");
        assert_eq!(format_size(999), "999 bytes");
        assert_eq!(format_size(1000), "1 KB");
        assert_eq!(format_size(12_345), "12 KB");
        assert_eq!(format_size(999_960), "1.0 MB");
        assert_eq!(format_size(3_400_000_000), "3.4 GB");
        assert_eq!(format_size(250_000_000), "250 MB");
    }

    #[test]
    fn local_owner_names() {
        let me = unsafe { libc::getuid() };
        assert_eq!(user_name(me), std::env::var("USER").unwrap());
        assert_eq!(group_name(0), "wheel");
        assert_eq!(user_name(4_000_000_000), "4000000000", "unknown users show their number");
        let dir = tempfile::tempdir().unwrap();
        let entry = Entry::from_path(dir.path()).unwrap();
        assert_eq!(entry.owner.as_deref(), Some(std::env::var("USER").unwrap().as_str()));
    }

    #[test]
    fn permission_strings() {
        let mut e = entry("x", EntryKind::Folder, 0);
        e.permissions = Some(0o755);
        assert_eq!(format_permissions(&e), "drwxr-xr-x");
        e.kind = EntryKind::File;
        e.permissions = Some(0o644);
        assert_eq!(format_permissions(&e), "-rw-r--r--");
        e.permissions = Some(0o4755);
        assert_eq!(format_permissions(&e), "-rwsr-xr-x");
        e.permissions = Some(0o1777);
        e.kind = EntryKind::Folder;
        assert_eq!(format_permissions(&e), "drwxrwxrwt");
        e.permissions = Some(0o2640);
        assert_eq!(format_permissions(&e), "drw-r-S---");
        e.permissions = None;
        assert_eq!(format_permissions(&e), "");
    }

    #[test]
    fn lists_with_kinds() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("sub"), dir.path().join("link")).unwrap();
        let mut entries = list_dir(dir.path()).unwrap();
        sort_entries(&mut entries, Sort::default());
        let summary: Vec<_> = entries.iter().map(|e| (e.name.as_str(), e.is_folder_like(), e.hidden, e.size)).collect();
        assert_eq!(
            summary,
            [
                ("link", true, false, 0),
                ("sub", true, false, 0),
                (".hidden", false, true, 0),
                ("a.txt", false, false, 5)
            ]
        );
    }
}
