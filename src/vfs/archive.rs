//! Archives as read-only folders, through the libarchive that comes with
//! macOS: zip, tar (plain, gzip, bzip2, xz, zstd), 7z, rar, iso, cpio, xar,
//! and more. The archive is indexed once when opened (folders it only
//! implies are filled in); a file's contents are streamed out when read.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::{FsRef, Vfs};
use crate::fs::{Entry, EntryKind};

#[repr(C)]
struct RawArchive {
    _private: [u8; 0],
}

#[repr(C)]
struct RawEntry {
    _private: [u8; 0],
}

#[link(name = "archive")]
unsafe extern "C" {
    fn archive_read_new() -> *mut RawArchive;
    fn archive_read_support_filter_all(a: *mut RawArchive) -> c_int;
    fn archive_read_support_format_all(a: *mut RawArchive) -> c_int;
    fn archive_read_open_filename(a: *mut RawArchive, path: *const c_char, block_size: usize) -> c_int;
    fn archive_read_next_header(a: *mut RawArchive, entry: *mut *mut RawEntry) -> c_int;
    fn archive_read_data(a: *mut RawArchive, buffer: *mut c_void, size: usize) -> isize;
    fn archive_read_data_skip(a: *mut RawArchive) -> c_int;
    fn archive_read_free(a: *mut RawArchive) -> c_int;
    fn archive_error_string(a: *mut RawArchive) -> *const c_char;
    fn archive_entry_pathname(e: *mut RawEntry) -> *const c_char;
    fn archive_entry_pathname_utf8(e: *mut RawEntry) -> *const c_char;
    fn archive_entry_size(e: *mut RawEntry) -> i64;
    fn archive_entry_mtime(e: *mut RawEntry) -> libc::time_t;
    fn archive_entry_mtime_is_set(e: *mut RawEntry) -> c_int;
    fn archive_entry_filetype(e: *mut RawEntry) -> libc::mode_t;
    fn archive_entry_perm(e: *mut RawEntry) -> libc::mode_t;

    fn archive_write_new() -> *mut RawArchive;
    fn archive_write_set_format_zip(a: *mut RawArchive) -> c_int;
    fn archive_write_set_format_pax_restricted(a: *mut RawArchive) -> c_int;
    fn archive_write_set_format_7zip(a: *mut RawArchive) -> c_int;
    fn archive_write_add_filter_gzip(a: *mut RawArchive) -> c_int;
    fn archive_write_add_filter_bzip2(a: *mut RawArchive) -> c_int;
    fn archive_write_add_filter_xz(a: *mut RawArchive) -> c_int;
    fn archive_write_open_filename(a: *mut RawArchive, path: *const c_char) -> c_int;
    fn archive_write_header(a: *mut RawArchive, entry: *mut RawEntry) -> c_int;
    fn archive_write_data(a: *mut RawArchive, buffer: *const c_void, size: usize) -> isize;
    fn archive_write_finish_entry(a: *mut RawArchive) -> c_int;
    fn archive_write_close(a: *mut RawArchive) -> c_int;
    fn archive_write_free(a: *mut RawArchive) -> c_int;
    fn archive_entry_new() -> *mut RawEntry;
    fn archive_entry_free(e: *mut RawEntry);
    fn archive_entry_set_pathname_utf8(e: *mut RawEntry, name: *const c_char);
    fn archive_entry_set_size(e: *mut RawEntry, size: i64);
    fn archive_entry_set_filetype(e: *mut RawEntry, kind: u32);
    fn archive_entry_set_perm(e: *mut RawEntry, perm: libc::mode_t);
    fn archive_entry_set_mtime(e: *mut RawEntry, seconds: libc::time_t, nanoseconds: std::ffi::c_long);
    fn archive_entry_set_symlink_utf8(e: *mut RawEntry, target: *const c_char);
}

const ARCHIVE_OK: c_int = 0;
const ARCHIVE_EOF: c_int = 1;
const ARCHIVE_WARN: c_int = -20;
const AE_IFMT: u32 = 0o170000;
const AE_IFDIR: u32 = 0o040000;
const AE_IFLNK: u32 = 0o120000;
const AE_IFREG: u32 = 0o100000;

/// Names forkz opens as archives (by extension).
const EXTENSIONS: &[&str] = &[
    "zip", "jar", "war", "ear", "apk", "ipa", "cbz", "tar", "tgz", "tbz", "tbz2", "txz", "tzst", "7z", "cb7", "rar",
    "cbr", "iso", "cpio", "xar", "deb", "rpm", "lha", "lzh", "cab", "ar",
];
/// Compressed tars ("x.tar.gz"), whose last extension alone says nothing.
const TAR_SUFFIXES: &[&str] = &[".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar.lz", ".tar.lzma", ".tar.z"];

/// Whether forkz treats a file with this name as an archive.
pub fn is_archive_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    TAR_SUFFIXES.iter().any(|s| lower.ends_with(s))
        || Path::new(&lower).extension().is_some_and(|e| EXTENSIONS.contains(&e.to_string_lossy().as_ref()))
}

/// An open libarchive reader. One thread at a time uses it.
struct Handle(*mut RawArchive);

// SAFETY: a libarchive reader may move between threads; it is only ever
// used by its owner.
unsafe impl Send for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe { archive_read_free(self.0) };
    }
}

impl Handle {
    fn open(file: &Path) -> io::Result<Handle> {
        let path = CString::new(file.as_os_str().as_encoded_bytes()).map_err(io::Error::other)?;
        let handle = Handle(unsafe { archive_read_new() });
        if handle.0.is_null() {
            return Err(io::Error::other("libarchive could not start"));
        }
        unsafe {
            archive_read_support_filter_all(handle.0);
            archive_read_support_format_all(handle.0);
        }
        if unsafe { archive_read_open_filename(handle.0, path.as_ptr(), 64 * 1024) } != ARCHIVE_OK {
            return Err(handle.error("This archive cannot be read"));
        }
        Ok(handle)
    }

    fn error(&self, fallback: &str) -> io::Error {
        let text = unsafe { archive_error_string(self.0) };
        let message = if text.is_null() {
            fallback.to_string()
        } else {
            unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()
        };
        io::Error::new(io::ErrorKind::InvalidData, message)
    }

    /// The next entry's details, or None at the end.
    fn next(&mut self) -> io::Result<Option<Item>> {
        let mut entry: *mut RawEntry = std::ptr::null_mut();
        match unsafe { archive_read_next_header(self.0, &mut entry) } {
            ARCHIVE_EOF => Ok(None),
            ARCHIVE_OK | ARCHIVE_WARN => {
                let name = unsafe {
                    let utf8 = archive_entry_pathname_utf8(entry);
                    let raw = if utf8.is_null() { archive_entry_pathname(entry) } else { utf8 };
                    if raw.is_null() { String::new() } else { CStr::from_ptr(raw).to_string_lossy().into_owned() }
                };
                let kind = u32::from(unsafe { archive_entry_filetype(entry) }) & AE_IFMT;
                let modified = (unsafe { archive_entry_mtime_is_set(entry) } != 0)
                    .then(|| unsafe { archive_entry_mtime(entry) })
                    .and_then(|t| u64::try_from(t).ok())
                    .map(|t| SystemTime::UNIX_EPOCH + Duration::from_secs(t));
                Ok(Some(Item {
                    path: normalize(&name),
                    folder: kind == AE_IFDIR || name.ends_with('/'),
                    link: kind == AE_IFLNK,
                    size: u64::try_from(unsafe { archive_entry_size(entry) }).unwrap_or(0),
                    modified,
                    permissions: u32::from(unsafe { archive_entry_perm(entry) }),
                }))
            }
            _ => Err(self.error("This archive is damaged")),
        }
    }
}

struct Item {
    path: PathBuf,
    folder: bool,
    link: bool,
    size: u64,
    modified: Option<SystemTime>,
    permissions: u32,
}

/// "./a/b/", "/a/b", and "a//b" all become "/a/b"; ".." parts are dropped
/// (an archive cannot point outside itself).
fn normalize(name: &str) -> PathBuf {
    let parts: Vec<&str> = name.split('/').filter(|p| !p.is_empty() && *p != "." && *p != "..").collect();
    PathBuf::from(format!("/{}", parts.join("/")))
}

/// Streams one file out of the archive.
struct EntryReader {
    handle: Handle,
}

impl Read for EntryReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = unsafe { archive_read_data(self.handle.0, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 { Err(self.handle.error("This file in the archive cannot be read")) } else { Ok(n as usize) }
    }
}

struct Index {
    /// The archive file on this Mac (a temporary copy for archives on
    /// servers or inside other archives).
    file: PathBuf,
    name: String,
    /// Where the archive is, for going up out of it and for addresses.
    outside: Option<(FsRef, PathBuf)>,
    entries: HashMap<PathBuf, Entry>,
    children: HashMap<PathBuf, Vec<PathBuf>>,
}

pub struct ArchiveFs {
    index: Arc<Index>,
}

impl ArchiveFs {
    /// Opens `file` (on this Mac). `outside` is where the archive really
    /// is: the file system and path shown when going up out of it.
    pub fn open(file: &Path, outside: Option<(FsRef, PathBuf)>) -> io::Result<Arc<ArchiveFs>> {
        let mut handle = Handle::open(file)?;
        let mut entries: BTreeMap<PathBuf, Entry> = BTreeMap::new();
        let add_folder = |entries: &mut BTreeMap<PathBuf, Entry>, path: &Path, modified, permissions| {
            entries
                .entry(path.to_path_buf())
                .or_insert_with(|| entry(path, EntryKind::Folder, 0, modified, permissions));
        };
        while let Some(item) = handle.next()? {
            unsafe { archive_read_data_skip(handle.0) };
            if item.path == Path::new("/") {
                continue;
            }
            // Folders the archive only implies ("a/b/c.txt" alone).
            for parent in item.path.ancestors().skip(1).filter(|p| *p != Path::new("/")) {
                add_folder(&mut entries, parent, None, None);
            }
            let permissions = Some(item.permissions);
            if item.folder {
                // A folder's own entry brings its date.
                let folder = entry(&item.path, EntryKind::Folder, 0, item.modified, permissions);
                entries.insert(item.path, folder);
            } else {
                let kind = if item.link { EntryKind::Symlink { target_is_folder: false } } else { EntryKind::File };
                let file = entry(&item.path, kind, item.size, item.modified, permissions);
                entries.insert(item.path, file);
            }
        }
        let mut children: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for path in entries.keys() {
            let parent = path.parent().unwrap_or(Path::new("/")).to_path_buf();
            children.entry(parent).or_default().push(path.clone());
        }
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name = outside.as_ref().and_then(|(_, p)| p.file_name()).map_or(name, |n| n.to_string_lossy().into_owned());
        Ok(Arc::new(ArchiveFs {
            index: Arc::new(Index {
                file: file.to_path_buf(),
                name,
                outside,
                entries: entries.into_iter().collect(),
                children,
            }),
        }))
    }

    /// (files, folders, total size of the files).
    pub fn summary(&self) -> (usize, usize, u64) {
        let files: Vec<&Entry> = self.index.entries.values().filter(|e| !e.is_folder_like()).collect();
        (files.len(), self.index.entries.len() - files.len(), files.iter().map(|e| e.size).sum())
    }

    /// Every item, in path order.
    pub fn all_entries(&self) -> Vec<Entry> {
        let mut all: Vec<Entry> = self.index.entries.values().cloned().collect();
        all.sort_by(|a, b| a.path.cmp(&b.path));
        all
    }
}

fn entry(path: &Path, kind: EntryKind, size: u64, modified: Option<SystemTime>, permissions: Option<u32>) -> Entry {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Entry {
        hidden: name.starts_with('.'),
        name,
        path: path.to_path_buf(),
        kind,
        size,
        modified,
        package: false,
        permissions,
        owner: None,
        group: None,
    }
}

fn read_only() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "Archives are read-only in forkz.")
}

impl Vfs for ArchiveFs {
    fn key(&self) -> String {
        match &self.index.outside {
            Some((fs, path)) => format!("archive:{}", fs.display_path(path)),
            None => format!("archive:{}", self.index.file.display()),
        }
    }

    fn is_local(&self) -> bool {
        false
    }

    fn label(&self) -> String {
        self.index.name.clone()
    }

    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        if dir != Path::new("/") && !self.index.entries.get(dir).is_some_and(Entry::is_folder_like) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "No such folder in the archive."));
        }
        let children = self.index.children.get(dir).map(Vec::as_slice).unwrap_or_default();
        Ok(children.iter().filter_map(|p| self.index.entries.get(p).cloned()).collect())
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        if path == Path::new("/") {
            return Ok(entry(Path::new("/"), EntryKind::Folder, 0, None, None));
        }
        self.index
            .entries
            .get(path)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No such item in the archive."))
    }

    fn create_dir(&self, _path: &Path) -> io::Result<()> {
        Err(read_only())
    }

    fn rename(&self, _from: &Path, _to: &Path) -> io::Result<()> {
        Err(read_only())
    }

    fn remove_file(&self, _path: &Path) -> io::Result<()> {
        Err(read_only())
    }

    fn remove_dir(&self, _path: &Path) -> io::Result<()> {
        Err(read_only())
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let mut handle = Handle::open(&self.index.file)?;
        while let Some(item) = handle.next()? {
            if item.path == path && !item.folder {
                return Ok(Box::new(EntryReader { handle }));
            }
            unsafe { archive_read_data_skip(handle.0) };
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "No such file in the archive."))
    }

    fn open_write(&self, _path: &Path) -> io::Result<Box<dyn Write + Send>> {
        Err(read_only())
    }

    fn set_attributes(&self, _path: &Path, _modified: Option<SystemTime>, _permissions: Option<u32>) -> io::Result<()> {
        Err(read_only())
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(ArchiveFs { index: self.index.clone() }))
    }

    fn display_path(&self, path: &Path) -> String {
        let base = match &self.index.outside {
            Some((fs, outer)) if fs.is_local() => outer.display().to_string(),
            Some((fs, outer)) => fs.display_path(outer),
            None => self.index.file.display().to_string(),
        };
        if path == Path::new("/") { base } else { format!("{base}{}", path.display()) }
    }

    fn supports_permissions(&self) -> bool {
        false
    }

    fn read_only(&self) -> bool {
        true
    }

    fn outside(&self) -> Option<(FsRef, PathBuf)> {
        self.index.outside.clone()
    }
}

/// What `compress` can make, by the new file's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    SevenZip,
}

impl Format {
    pub fn for_name(name: &str) -> Option<Format> {
        let lower = name.to_lowercase();
        let ends = |s: &str| lower.ends_with(s);
        Some(match () {
            _ if ends(".zip") => Format::Zip,
            _ if ends(".tar.gz") || ends(".tgz") => Format::TarGz,
            _ if ends(".tar.bz2") || ends(".tbz2") => Format::TarBz2,
            _ if ends(".tar.xz") || ends(".txz") => Format::TarXz,
            _ if ends(".tar") => Format::Tar,
            _ if ends(".7z") => Format::SevenZip,
            _ => return None,
        })
    }
}

/// Writes an archive in progress; freed (and closed) when dropped.
struct Writer(*mut RawArchive);

impl Drop for Writer {
    fn drop(&mut self) {
        unsafe { archive_write_free(self.0) };
    }
}

impl Writer {
    fn check(&self, code: c_int) -> io::Result<()> {
        if code >= ARCHIVE_WARN { Ok(()) } else { Err(self.error()) }
    }

    fn error(&self) -> io::Error {
        let text = unsafe { archive_error_string(self.0) };
        let message = if text.is_null() {
            "The archive could not be written".to_string()
        } else {
            unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()
        };
        io::Error::other(message)
    }
}

/// Makes an archive at `destination` (its name decides the format) of
/// `sources` and everything in them, named relative to their folder.
/// `progress` gets the bytes written so far; returning false stops.
/// Nothing is left behind if it fails or is stopped.
pub fn compress(sources: &[PathBuf], destination: &Path, progress: &mut dyn FnMut(u64) -> bool) -> io::Result<()> {
    let name = destination.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let format = Format::for_name(&name).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "Use .zip, .tar, .tar.gz, .tar.bz2, .tar.xz, or .7z.")
    })?;
    let partial = destination.with_file_name(format!(".{name}.forkz-partial"));
    let result = write_archive(sources, &partial, format, progress);
    match result {
        Ok(()) => std::fs::rename(&partial, destination),
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

fn write_archive(
    sources: &[PathBuf],
    file: &Path,
    format: Format,
    progress: &mut dyn FnMut(u64) -> bool,
) -> io::Result<()> {
    let writer = Writer(unsafe { archive_write_new() });
    unsafe {
        let a = writer.0;
        let set = match format {
            Format::Zip => archive_write_set_format_zip(a),
            Format::SevenZip => archive_write_set_format_7zip(a),
            _ => archive_write_set_format_pax_restricted(a),
        };
        writer.check(set)?;
        match format {
            Format::TarGz => writer.check(archive_write_add_filter_gzip(a))?,
            Format::TarBz2 => writer.check(archive_write_add_filter_bzip2(a))?,
            Format::TarXz => writer.check(archive_write_add_filter_xz(a))?,
            _ => {}
        }
        let path = CString::new(file.as_os_str().as_encoded_bytes()).map_err(io::Error::other)?;
        writer.check(archive_write_open_filename(a, path.as_ptr()))?;
    }
    let mut written = 0u64;
    let mut buffer = vec![0u8; 256 * 1024];
    for source in sources {
        let base = source.parent().unwrap_or(Path::new("/"));
        let mut stack = vec![source.clone()];
        while let Some(path) = stack.pop() {
            let meta = std::fs::symlink_metadata(&path)?;
            let relative = path.strip_prefix(base).unwrap_or(&path).to_string_lossy().into_owned();
            let entry = unsafe { archive_entry_new() };
            let name = CString::new(if meta.is_dir() { format!("{relative}/") } else { relative })
                .map_err(io::Error::other)?;
            unsafe {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                archive_entry_set_pathname_utf8(entry, name.as_ptr());
                archive_entry_set_perm(entry, (meta.permissions().mode() & 0o7777) as libc::mode_t);
                archive_entry_set_mtime(entry, meta.mtime(), meta.mtime_nsec() as std::ffi::c_long);
            }
            let mut target = None;
            if meta.file_type().is_symlink() {
                target = Some(
                    CString::new(std::fs::read_link(&path)?.as_os_str().as_encoded_bytes())
                        .map_err(io::Error::other)?,
                );
                unsafe {
                    archive_entry_set_filetype(entry, AE_IFLNK);
                    archive_entry_set_symlink_utf8(entry, target.as_ref().map_or(std::ptr::null(), |t| t.as_ptr()));
                }
            } else if meta.is_dir() {
                unsafe { archive_entry_set_filetype(entry, AE_IFDIR) };
            } else {
                unsafe {
                    archive_entry_set_filetype(entry, AE_IFREG);
                    archive_entry_set_size(entry, meta.len() as i64);
                }
            }
            let header = writer.check(unsafe { archive_write_header(writer.0, entry) });
            unsafe { archive_entry_free(entry) };
            drop(target);
            header?;
            if meta.is_file() {
                let mut input = std::fs::File::open(&path)?;
                loop {
                    let n = input.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    if unsafe { archive_write_data(writer.0, buffer.as_ptr().cast(), n) } < 0 {
                        return Err(writer.error());
                    }
                    written += n as u64;
                    if !progress(written) {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
                    }
                }
            }
            writer.check(unsafe { archive_write_finish_entry(writer.0) })?;
            if meta.is_dir() {
                let mut children: Vec<PathBuf> =
                    std::fs::read_dir(&path)?.map(|e| e.map(|e| e.path())).collect::<io::Result<_>>()?;
                // Popped from the end: reversed for name order.
                children.sort();
                children.reverse();
                stack.extend(children);
            }
        }
    }
    writer.check(unsafe { archive_write_close(writer.0) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn names(fs: &ArchiveFs, dir: &str) -> Vec<(String, bool)> {
        let mut names: Vec<_> =
            fs.list(Path::new(dir)).unwrap().into_iter().map(|e| (e.name.clone(), e.is_folder_like())).collect();
        names.sort();
        names
    }

    fn read(fs: &ArchiveFs, path: &str) -> String {
        let mut text = String::new();
        fs.open_read(Path::new(path)).unwrap().read_to_string(&mut text).unwrap();
        text
    }

    /// A folder with files to pack: a.txt, docs/b.md, docs/deep/c.txt.
    fn sample() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(src.join("docs/deep")).unwrap();
        std::fs::write(src.join("a.txt"), "alpha").unwrap();
        std::fs::write(src.join("docs/b.md"), "# bravo").unwrap();
        std::fs::write(src.join("docs/deep/c \u{e4}.txt"), "charlie".repeat(10_000)).unwrap();
        dir
    }

    #[test]
    fn names_that_are_archives() {
        for name in ["a.zip", "B.TAR.GZ", "c.tgz", "d.7z", "e.tar.xz", "f.rar", "g.iso"] {
            assert!(is_archive_name(name), "{name}");
        }
        for name in ["a.txt", "b.gz", "c.app", "tar", "d.tar.gzip"] {
            assert!(!is_archive_name(name), "{name}");
        }
    }

    #[test]
    fn tar_gz_with_dot_paths() {
        let dir = sample();
        let archive = dir.path().join("sample.tar.gz");
        // "./a.txt" style names, with folder entries.
        let status = Command::new("/usr/bin/tar")
            .args(["-czf", archive.to_str().unwrap(), "-C", dir.path().join("src").to_str().unwrap(), "."])
            .status()
            .unwrap();
        assert!(status.success());
        let fs = ArchiveFs::open(&archive, None).unwrap();
        assert_eq!(names(&fs, "/"), [("a.txt".into(), false), ("docs".into(), true)]);
        assert_eq!(names(&fs, "/docs"), [("b.md".into(), false), ("deep".into(), true)]);
        assert_eq!(read(&fs, "/docs/b.md"), "# bravo");
        // macOS's tar may store "\u{e4}" decomposed: use the listed path.
        let deep = fs.list(Path::new("/docs/deep")).unwrap().remove(0).path;
        assert_eq!(read(&fs, &deep.to_string_lossy()).len(), 70_000);
        assert_eq!(fs.stat(&deep).unwrap().size, 70_000);
        assert_eq!(fs.summary(), (3, 2, 5 + 7 + 70_000));
        assert_eq!(fs.list(Path::new("/nope")).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert_eq!(fs.create_dir(Path::new("/x")).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert!(fs.read_only());
    }

    #[test]
    fn zip_without_folder_entries() {
        let dir = sample();
        let archive = dir.path().join("sample.zip");
        // -D: no entries for folders; they are implied by the paths.
        let status = Command::new("/usr/bin/zip")
            .args(["-q", "-r", "-D", archive.to_str().unwrap(), "."])
            .current_dir(dir.path().join("src"))
            .status()
            .unwrap();
        assert!(status.success());
        let fs = ArchiveFs::open(&archive, None).unwrap();
        assert_eq!(names(&fs, "/"), [("a.txt".into(), false), ("docs".into(), true)]);
        assert_eq!(names(&fs, "/docs/deep"), [("c \u{e4}.txt".into(), false)]);
        assert_eq!(read(&fs, "/a.txt"), "alpha");
        // Copying out works like from any file system.
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let fs: FsRef = fs;
        let job = crate::fs::ops::Job::between(
            crate::fs::ops::JobKind::Copy,
            fs,
            vec![PathBuf::from("/docs")],
            super::super::local(),
            out.clone(),
        );
        struct Quiet;
        impl crate::fs::ops::Observer for Quiet {
            fn progress(&mut self, _: &crate::fs::ops::Progress) {}
            fn conflict(&mut self, _: &crate::fs::ops::Conflict) -> crate::fs::ops::Answer {
                crate::fs::ops::Answer { resolution: crate::fs::ops::Resolution::Replace, apply_to_all: true }
            }
            fn error(&mut self, path: &Path, error: &io::Error) -> crate::fs::ops::ErrorAnswer {
                panic!("{}: {error}", path.display())
            }
        }
        let outcome = crate::fs::ops::run(&job, &mut Quiet, Default::default());
        assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
        assert_eq!(std::fs::read_to_string(out.join("docs/b.md")).unwrap(), "# bravo");
        assert_eq!(std::fs::read_to_string(out.join("docs/deep/c \u{e4}.txt")).unwrap().len(), 70_000);
    }

    #[test]
    fn compresses_in_every_format_and_reads_back() {
        let dir = sample();
        let src = dir.path().join("src");
        let sources = vec![src.join("a.txt"), src.join("docs")];
        for name in ["out.zip", "out.tar", "out.tar.gz", "out.tbz2", "out.tar.xz", "out.7z"] {
            let destination = dir.path().join(name);
            let mut calls = 0;
            compress(&sources, &destination, &mut |_| {
                calls += 1;
                true
            })
            .unwrap();
            assert!(calls > 0, "{name}");
            let fs = ArchiveFs::open(&destination, None).unwrap();
            assert_eq!(names(&fs, "/"), [("a.txt".into(), false), ("docs".into(), true)], "{name}");
            assert_eq!(read(&fs, "/docs/b.md"), "# bravo", "{name}");
            let deep = fs.list(Path::new("/docs/deep")).unwrap().remove(0).path;
            assert_eq!(read(&fs, &deep.to_string_lossy()).len(), 70_000, "{name}");
        }
        // Stopped: nothing is left.
        let stopped = dir.path().join("stopped.zip");
        let error = compress(&sources, &stopped, &mut |_| false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!stopped.exists() && !dir.path().join(".stopped.zip.forkz-partial").exists());
        assert!(compress(&sources, &dir.path().join("out.rar"), &mut |_| true).is_err());
    }

    #[test]
    fn not_an_archive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fake.zip");
        std::fs::write(&file, "this is not a zip file at all").unwrap();
        assert!(ArchiveFs::open(&file, None).is_err());
    }
}
