//! Archives as folders, through the libarchive that comes with macOS: zip,
//! tar (plain, gzip, bzip2, xz, zstd), 7z, rar, iso, cpio, xar, and more.
//! The archive is indexed once when opened (folders it only implies are
//! filled in); a file's contents are streamed out when read.
//!
//! Zip, tar (plain, gzip, bzip2, xz), and 7z archives on this Mac can also
//! be changed: items added, renamed, moved, and deleted, and folders made.
//! Changes are written by making the archive again next to the old one
//! (which it then replaces); a job's changes are written once, at its end.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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
    fn archive_write_fail(a: *mut RawArchive) -> c_int;
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
        Ok(self.next_raw()?.map(|(_, item)| item))
    }

    /// The next entry (libarchive's, valid until the next one) and its
    /// details, or None at the end.
    fn next_raw(&mut self) -> io::Result<Option<(*mut RawEntry, Item)>> {
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
                Ok(Some((
                    entry,
                    Item {
                        path: normalize(&name),
                        folder: kind == AE_IFDIR || name.ends_with('/'),
                        link: kind == AE_IFLNK,
                        size: u64::try_from(unsafe { archive_entry_size(entry) }).unwrap_or(0),
                        modified,
                        permissions: u32::from(unsafe { archive_entry_perm(entry) }),
                    },
                )))
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

/// Where an item's contents come from.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    /// An entry of the archive file, by its path there.
    Original(PathBuf),
    /// A folder the archive only implies ("a/b/c.txt" alone).
    Implied,
    /// Written since the archive was opened: a temporary file.
    Staged(PathBuf),
    /// A folder made since the archive was opened.
    NewFolder,
}

#[derive(Debug, Clone)]
struct Node {
    entry: Entry,
    source: Source,
}

/// What the archive holds now: its items (with changes not yet written
/// into the file), and how many jobs are changing it.
struct Model {
    nodes: BTreeMap<PathBuf, Node>,
    /// Jobs changing the archive: the file is written once they are done.
    batch: usize,
    dirty: bool,
}

struct Index {
    /// The archive file on this Mac (a temporary copy for archives on
    /// servers or inside other archives).
    file: PathBuf,
    name: String,
    /// Where the archive is, for going up out of it and for addresses.
    outside: Option<(FsRef, PathBuf)>,
    /// How to write it again, for archives that can be changed: on this
    /// Mac (not a copy), in a format libarchive can write.
    format: Option<Format>,
    model: Mutex<Model>,
}

pub struct ArchiveFs {
    index: Arc<Index>,
}

/// Reads an archive's items (with the folders it only implies).
fn read_nodes(file: &Path) -> io::Result<BTreeMap<PathBuf, Node>> {
    let mut handle = Handle::open(file)?;
    let mut nodes: BTreeMap<PathBuf, Node> = BTreeMap::new();
    while let Some(item) = handle.next()? {
        unsafe { archive_read_data_skip(handle.0) };
        if item.path == Path::new("/") {
            continue;
        }
        for parent in item.path.ancestors().skip(1).filter(|p| *p != Path::new("/")) {
            nodes.entry(parent.to_path_buf()).or_insert_with(|| Node {
                entry: entry(parent, EntryKind::Folder, 0, None, None),
                source: Source::Implied,
            });
        }
        let permissions = Some(item.permissions);
        let kind = if item.folder {
            EntryKind::Folder
        } else if item.link {
            EntryKind::Symlink { target_is_folder: false }
        } else {
            EntryKind::File
        };
        let size = if item.folder { 0 } else { item.size };
        let node = Node {
            entry: entry(&item.path, kind, size, item.modified, permissions),
            source: Source::Original(item.path.clone()),
        };
        nodes.insert(item.path, node);
    }
    Ok(nodes)
}

impl ArchiveFs {
    /// Opens `file` (on this Mac). `outside` is where the archive really
    /// is: the file system and path shown when going up out of it. An
    /// archive that is that file itself, in a format forkz can write, can
    /// be changed.
    pub fn open(file: &Path, outside: Option<(FsRef, PathBuf)>) -> io::Result<Arc<ArchiveFs>> {
        let nodes = read_nodes(file)?;
        let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let name = outside.as_ref().and_then(|(_, p)| p.file_name()).map_or(name, |n| n.to_string_lossy().into_owned());
        let real = outside.as_ref().is_some_and(|(fs, path)| fs.is_local() && path == file);
        let format = if real { Format::for_name(&name) } else { None };
        Ok(Arc::new(ArchiveFs {
            index: Arc::new(Index {
                file: file.to_path_buf(),
                name,
                outside,
                format,
                model: Mutex::new(Model { nodes, batch: 0, dirty: false }),
            }),
        }))
    }

    fn model(&self) -> std::sync::MutexGuard<'_, Model> {
        self.index.model.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// (files, folders, total size of the files).
    pub fn summary(&self) -> (usize, usize, u64) {
        let model = self.model();
        let files: Vec<&Entry> = model.nodes.values().map(|n| &n.entry).filter(|e| !e.is_folder_like()).collect();
        (files.len(), model.nodes.len() - files.len(), files.iter().map(|e| e.size).sum())
    }

    /// Every item, in path order.
    pub fn all_entries(&self) -> Vec<Entry> {
        self.model().nodes.values().map(|n| n.entry.clone()).collect()
    }

    /// The archive can be changed, or why not.
    fn writable(&self) -> io::Result<Format> {
        self.index.format.ok_or_else(read_only)
    }

    /// A folder to put an item in: the top, or a folder in the archive.
    fn check_parent(model: &Model, path: &Path) -> io::Result<()> {
        let parent = path.parent().unwrap_or(Path::new("/"));
        if parent == Path::new("/") || model.nodes.get(parent).is_some_and(|n| n.entry.is_folder_like()) {
            Ok(())
        } else {
            Err(io::Error::new(io::ErrorKind::NotFound, "No such folder in the archive."))
        }
    }

    /// A change was made: written into the file now, unless a job is
    /// still changing the archive.
    fn changed(&self, model: &mut Model) -> io::Result<()> {
        model.dirty = true;
        if model.batch == 0 { self.commit(model) } else { Ok(()) }
    }

    /// Writes the archive again with the changes: into a new file next to
    /// it, which then replaces it. If that fails, the changes are dropped
    /// and the archive stays as it was.
    fn commit(&self, model: &mut Model) -> io::Result<()> {
        if !model.dirty {
            return Ok(());
        }
        let format = self.writable()?;
        let result = rewrite(&self.index.file, format, &model.nodes);
        let staged: Vec<PathBuf> = model
            .nodes
            .values()
            .filter_map(|n| match &n.source {
                Source::Staged(temp) => Some(temp.clone()),
                _ => None,
            })
            .collect();
        model.dirty = false;
        // What the file holds now (the old contents if writing failed).
        let reread = read_nodes(&self.index.file);
        for temp in staged {
            let _ = std::fs::remove_file(temp);
        }
        model.nodes = reread?;
        result
    }
}

/// Writes `file` again holding `nodes`: original entries copied over (under
/// their new names), then new folders and files.
fn rewrite(file: &Path, format: Format, nodes: &BTreeMap<PathBuf, Node>) -> io::Result<()> {
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let partial = file.with_file_name(format!(".{name}.forkz-partial"));
    let result = (|| {
        let writer = open_writer(&partial, format)?;
        // Where each original entry went (gone: deleted or replaced).
        let moved: HashMap<&Path, &Path> = nodes
            .iter()
            .filter_map(|(path, node)| match &node.source {
                Source::Original(original) => Some((original.as_path(), path.as_path())),
                _ => None,
            })
            .collect();
        let mut buffer = vec![0u8; 256 * 1024];
        let mut reader = Handle::open(file)?;
        while let Some((raw, item)) = reader.next_raw()? {
            let Some(path) = moved.get(item.path.as_path()) else {
                unsafe { archive_read_data_skip(reader.0) };
                continue;
            };
            let relative = path.strip_prefix("/").unwrap_or(path).to_string_lossy().into_owned();
            let name =
                CString::new(if item.folder { format!("{relative}/") } else { relative }).map_err(io::Error::other)?;
            unsafe { archive_entry_set_pathname_utf8(raw, name.as_ptr()) };
            writer.check(unsafe { archive_write_header(writer.0, raw) })?;
            if !item.folder && !item.link {
                loop {
                    let n = unsafe { archive_read_data(reader.0, buffer.as_mut_ptr().cast(), buffer.len()) };
                    if n < 0 {
                        return Err(reader.error("A file in the archive cannot be read"));
                    }
                    if n == 0 {
                        break;
                    }
                    if unsafe { archive_write_data(writer.0, buffer.as_ptr().cast(), n as usize) } < 0 {
                        return Err(writer.error());
                    }
                }
            }
            writer.check(unsafe { archive_write_finish_entry(writer.0) })?;
        }
        for (path, node) in nodes {
            let relative = path.strip_prefix("/").unwrap_or(path).to_string_lossy().into_owned();
            let modified = node.entry.modified.unwrap_or_else(SystemTime::now);
            let seconds = modified.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as libc::time_t;
            match &node.source {
                Source::Original(_) => continue,
                Source::Implied | Source::NewFolder => {
                    let name = CString::new(format!("{relative}/")).map_err(io::Error::other)?;
                    let entry = unsafe { archive_entry_new() };
                    unsafe {
                        archive_entry_set_pathname_utf8(entry, name.as_ptr());
                        archive_entry_set_filetype(entry, AE_IFDIR);
                        archive_entry_set_perm(entry, 0o755);
                        archive_entry_set_mtime(entry, seconds, 0);
                    }
                    let header = writer.check(unsafe { archive_write_header(writer.0, entry) });
                    unsafe { archive_entry_free(entry) };
                    header?;
                }
                Source::Staged(temp) => {
                    let mut input = std::fs::File::open(temp)?;
                    let size = input.metadata()?.len();
                    let name = CString::new(relative).map_err(io::Error::other)?;
                    let entry = unsafe { archive_entry_new() };
                    unsafe {
                        archive_entry_set_pathname_utf8(entry, name.as_ptr());
                        archive_entry_set_filetype(entry, AE_IFREG);
                        archive_entry_set_perm(
                            entry,
                            node.entry.permissions.map_or(0o644, |p| p & 0o7777) as libc::mode_t,
                        );
                        archive_entry_set_size(entry, size as i64);
                        archive_entry_set_mtime(entry, seconds, 0);
                    }
                    let header = writer.check(unsafe { archive_write_header(writer.0, entry) });
                    unsafe { archive_entry_free(entry) };
                    header?;
                    loop {
                        let n = input.read(&mut buffer)?;
                        if n == 0 {
                            break;
                        }
                        if unsafe { archive_write_data(writer.0, buffer.as_ptr().cast(), n) } < 0 {
                            return Err(writer.error());
                        }
                    }
                }
            }
            writer.check(unsafe { archive_write_finish_entry(writer.0) })?;
        }
        writer.check(unsafe { archive_write_close(writer.0) })?;
        writer.1.set(true);
        Ok(())
    })();
    match result {
        Ok(()) => std::fs::rename(&partial, file),
        Err(e) => {
            let _ = std::fs::remove_file(&partial);
            Err(e)
        }
    }
}

/// A file being written into an archive: kept in a temporary file, and
/// put into the archive when flushed or closed.
struct StagedWriter {
    fs: ArchiveFs,
    path: PathBuf,
    temp: PathBuf,
    file: std::fs::File,
    /// Written to since last put into the archive.
    fresh: bool,
}

impl StagedWriter {
    /// Puts the file (as it is now) into the archive's items.
    fn stage(&mut self) -> io::Result<()> {
        self.file.flush()?;
        self.fresh = false;
        let size = self.file.metadata()?.len();
        let mut model = self.fs.model();
        let modified = Some(SystemTime::now());
        let old = model.nodes.insert(
            self.path.clone(),
            Node {
                entry: entry(&self.path, EntryKind::File, size, modified, Some(0o644)),
                source: Source::Staged(self.temp.clone()),
            },
        );
        if let Some(Node { source: Source::Staged(previous), .. }) = old
            && previous != self.temp
        {
            let _ = std::fs::remove_file(previous);
        }
        self.fs.changed(&mut model)
    }
}

impl Write for StagedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.fresh = true;
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stage()
    }
}

impl Drop for StagedWriter {
    fn drop(&mut self) {
        if self.fresh
            && let Err(e) = self.stage()
        {
            tracing::warn!("{} could not be added to the archive: {e}", self.path.display());
        }
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
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "This archive cannot be changed: only zip, tar (with gzip, bzip2, or xz), and 7z archives on this Mac can.",
    )
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "No such item in the archive.")
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
        let model = self.model();
        if dir != Path::new("/") && !model.nodes.get(dir).is_some_and(|n| n.entry.is_folder_like()) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "No such folder in the archive."));
        }
        Ok(model
            .nodes
            .range(dir.to_path_buf()..)
            .skip_while(|(path, _)| path.as_path() == dir)
            .take_while(|(path, _)| path.starts_with(dir))
            .filter(|(path, _)| path.parent() == Some(dir))
            .map(|(_, node)| node.entry.clone())
            .collect())
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        if path == Path::new("/") {
            return Ok(entry(Path::new("/"), EntryKind::Folder, 0, None, None));
        }
        self.model().nodes.get(path).map(|n| n.entry.clone()).ok_or_else(not_found)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.writable()?;
        let mut model = self.model();
        if model.nodes.contains_key(path) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "An item with this name is already there."));
        }
        Self::check_parent(&model, path)?;
        let folder = entry(path, EntryKind::Folder, 0, Some(SystemTime::now()), Some(0o755));
        model.nodes.insert(path.to_path_buf(), Node { entry: folder, source: Source::NewFolder });
        self.changed(&mut model)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.writable()?;
        let mut model = self.model();
        if !model.nodes.contains_key(from) {
            return Err(not_found());
        }
        if model.nodes.contains_key(to) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "An item with this name is already there."));
        }
        if to.starts_with(from) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "A folder cannot go into itself."));
        }
        Self::check_parent(&model, to)?;
        // The item and everything in it.
        let moving: Vec<PathBuf> = model.nodes.keys().filter(|p| p.starts_with(from)).cloned().collect();
        for old in moving {
            if let Some(mut node) = model.nodes.remove(&old) {
                let new = to.join(old.strip_prefix(from).unwrap_or(Path::new("")));
                let new = if new.as_os_str().is_empty() { to.to_path_buf() } else { new };
                let new = PathBuf::from(new.to_string_lossy().trim_end_matches('/'));
                node.entry.name = new.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                node.entry.hidden = node.entry.name.starts_with('.');
                node.entry.path = new.clone();
                model.nodes.insert(new, node);
            }
        }
        self.changed(&mut model)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.writable()?;
        let mut model = self.model();
        match model.nodes.get(path) {
            None => return Err(not_found()),
            Some(node) if node.entry.is_folder_like() => {
                return Err(io::Error::new(io::ErrorKind::IsADirectory, "This is a folder."));
            }
            Some(_) => {}
        }
        if let Some(Node { source: Source::Staged(temp), .. }) = model.nodes.remove(path) {
            let _ = std::fs::remove_file(temp);
        }
        self.changed(&mut model)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.writable()?;
        let mut model = self.model();
        if !model.nodes.get(path).is_some_and(|n| n.entry.is_folder_like()) {
            return Err(not_found());
        }
        if model.nodes.keys().any(|p| p != path && p.starts_with(path)) {
            return Err(io::Error::new(io::ErrorKind::DirectoryNotEmpty, "The folder is not empty."));
        }
        model.nodes.remove(path);
        self.changed(&mut model)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        let source = self.model().nodes.get(path).map(|n| n.source.clone()).ok_or_else(not_found)?;
        let original = match source {
            Source::Staged(temp) => return Ok(Box::new(std::fs::File::open(temp)?)),
            Source::Original(original) => original,
            Source::Implied | Source::NewFolder => {
                return Err(io::Error::new(io::ErrorKind::IsADirectory, "This is a folder."));
            }
        };
        let mut handle = Handle::open(&self.index.file)?;
        while let Some(item) = handle.next()? {
            if item.path == original && !item.folder {
                return Ok(Box::new(EntryReader { handle }));
            }
            unsafe { archive_read_data_skip(handle.0) };
        }
        Err(io::Error::new(io::ErrorKind::NotFound, "No such file in the archive."))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.writable()?;
        {
            let model = self.model();
            Self::check_parent(&model, path)?;
            if model.nodes.get(path).is_some_and(|n| n.entry.is_folder_like()) {
                return Err(io::Error::new(io::ErrorKind::IsADirectory, "A folder has this name."));
            }
        }
        let folder = std::env::temp_dir().join("forkz-archive-edits");
        std::fs::create_dir_all(&folder)?;
        let temp = folder.join(crate::sites::new_id());
        let file = std::fs::File::create(&temp)?;
        Ok(Box::new(StagedWriter {
            fs: ArchiveFs { index: self.index.clone() },
            path: path.to_path_buf(),
            temp,
            file,
            fresh: true,
        }))
    }

    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, _permissions: Option<u32>) -> io::Result<()> {
        self.writable()?;
        let mut model = self.model();
        let node = model.nodes.get_mut(path).ok_or_else(not_found)?;
        // Only new files take a date (original entries keep theirs).
        if matches!(node.source, Source::Staged(_)) && modified.is_some() {
            node.entry.modified = modified;
            return self.changed(&mut model);
        }
        Ok(())
    }

    fn begin_changes(&self) {
        self.model().batch += 1;
    }

    fn end_changes(&self) -> io::Result<()> {
        let mut model = self.model();
        model.batch = model.batch.saturating_sub(1);
        if model.batch == 0 { self.commit(&mut model) } else { Ok(()) }
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
        self.index.format.is_none()
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

/// Writes an archive in progress; freed when dropped. Unless it was closed
/// properly, it is marked failed first, so freeing does not finish it (7z
/// compresses everything when closing, which can take minutes).
struct Writer(*mut RawArchive, std::cell::Cell<bool>);

impl Drop for Writer {
    fn drop(&mut self) {
        unsafe {
            if !self.1.get() {
                archive_write_fail(self.0);
            }
            archive_write_free(self.0);
        }
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

/// Starts writing an archive file in a format.
fn open_writer(file: &Path, format: Format) -> io::Result<Writer> {
    let writer = Writer(unsafe { archive_write_new() }, std::cell::Cell::new(false));
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
    Ok(writer)
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
    let writer = open_writer(file, format)?;
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
    // Stopped while 7z compresses on closing: it checks only at the end.
    if !progress(written) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
    }
    writer.check(unsafe { archive_write_close(writer.0) })?;
    writer.1.set(true);
    // Stopped while closing (which cannot be interrupted): not kept.
    if !progress(written) {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "Stopped"));
    }
    Ok(())
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
    fn changes_archives_in_place() {
        let dir = sample();
        let src = dir.path().join("src");
        let sources = vec![src.join("a.txt"), src.join("docs")];
        for name in ["edit.zip", "edit.tar.gz", "edit.7z", "edit.tar"] {
            let file = dir.path().join(name);
            compress(&sources, &file, &mut |_| true).unwrap();
            let open = || ArchiveFs::open(&file, Some((crate::vfs::local(), file.clone()))).unwrap();
            let fs = open();
            assert!(!fs.read_only(), "{name}");
            // One change at a time: written right away.
            fs.create_dir(Path::new("/new")).unwrap();
            fs.rename(Path::new("/docs"), Path::new("/new/docs")).unwrap();
            fs.remove_file(Path::new("/a.txt")).unwrap();
            let mut writer = fs.open_write(Path::new("/new/added.txt")).unwrap();
            writer.write_all(b"delta").unwrap();
            writer.flush().unwrap();
            drop(writer);
            // As a job does: held, then written once.
            fs.begin_changes();
            let mut writer = fs.open_write(Path::new("/new/.part")).unwrap();
            writer.write_all(b"echo").unwrap();
            drop(writer);
            fs.rename(Path::new("/new/.part"), Path::new("/echo.txt")).unwrap();
            assert_eq!(read(&fs, "/echo.txt"), "echo", "{name}: readable before it is written");
            fs.end_changes().unwrap();

            // Read back from the file itself.
            let fresh = open();
            assert_eq!(names(&fresh, "/"), [("echo.txt".into(), false), ("new".into(), true)], "{name}");
            assert_eq!(names(&fresh, "/new"), [("added.txt".into(), false), ("docs".into(), true)], "{name}");
            assert_eq!(read(&fresh, "/new/added.txt"), "delta", "{name}");
            assert_eq!(read(&fresh, "/new/docs/b.md"), "# bravo", "{name}");
            let deep = fresh.list(Path::new("/new/docs/deep")).unwrap().remove(0).path;
            assert_eq!(read(&fresh, &deep.to_string_lossy()).len(), 70_000, "{name}");
            // Deleting a folder: its items first, then the folder.
            fresh.remove_file(&deep).unwrap();
            fresh.remove_dir(Path::new("/new/docs/deep")).unwrap();
            assert!(fresh.remove_dir(Path::new("/new")).is_err(), "{name}: not empty");
            assert_eq!(names(&open(), "/new/docs"), [("b.md".into(), false)], "{name}");
            assert!(!dir.path().join(format!(".{name}.forkz-partial")).exists());
        }
        // Copies, and formats libarchive cannot write, stay read-only.
        let zip = dir.path().join("edit.zip");
        let copy = ArchiveFs::open(&zip, None).unwrap();
        assert!(copy.read_only());
        assert!(copy.create_dir(Path::new("/x")).is_err());
    }

    #[test]
    fn not_an_archive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fake.zip");
        std::fs::write(&file, "this is not a zip file at all").unwrap();
        assert!(ArchiveFs::open(&file, None).is_err());
    }
}
