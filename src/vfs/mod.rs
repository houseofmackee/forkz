//! File systems a pane can show: the Mac's own, or a remote server. Every
//! call blocks, so remote ones run off the main thread.

pub mod known_hosts;
pub mod sftp;
pub mod ssh_config;

use std::fmt;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::fs::Entry;

pub type FsRef = Arc<dyn Vfs>;

pub trait Vfs: Send + Sync {
    /// Identifies the file system: equal keys mean the same server and
    /// account, so items can be renamed instead of copied.
    fn key(&self) -> String;

    fn is_local(&self) -> bool;

    /// For people: "Macintosh HD" or "user@host".
    fn label(&self) -> String;

    /// The folder to start in.
    fn home(&self) -> PathBuf;

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>>;

    /// The item itself (a link is not followed). NotFound if it is missing.
    fn stat(&self, path: &Path) -> io::Result<Entry>;

    fn create_dir(&self, path: &Path) -> io::Result<()>;

    /// Renames within this file system; an existing file at `to` may be
    /// replaced.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;

    fn remove_file(&self, path: &Path) -> io::Result<()>;

    /// Removes an empty folder.
    fn remove_dir(&self, path: &Path) -> io::Result<()>;

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>>;

    /// Creates or truncates a file.
    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>>;

    /// Sets the modification date (and permissions if given), best effort.
    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, permissions: Option<u32>) -> io::Result<()>;

    /// A separate connection for a long transfer, so browsing stays
    /// responsive. The local file system returns itself.
    fn connect_another(&self) -> io::Result<FsRef>;

    /// A link to show or copy: a path, or "sftp://user@host/path".
    fn display_path(&self, path: &Path) -> String;
}

impl fmt::Debug for dyn Vfs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Vfs({})", self.key())
    }
}

/// The Mac's own file system.
#[derive(Debug, Default)]
pub struct LocalFs;

pub fn local() -> FsRef {
    thread_local! {
        static LOCAL: FsRef = Arc::new(LocalFs);
    }
    LOCAL.with(Arc::clone)
}

pub fn same(a: &FsRef, b: &FsRef) -> bool {
    a.key() == b.key()
}

impl Vfs for LocalFs {
    fn key(&self) -> String {
        "local".into()
    }

    fn is_local(&self) -> bool {
        true
    }

    fn label(&self) -> String {
        "Macintosh HD".into()
    }

    fn home(&self) -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        crate::fs::list_dir(dir)
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        Entry::from_path(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        Ok(Box::new(std::fs::File::open(path)?))
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        Ok(Box::new(std::fs::File::create(path)?))
    }

    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, permissions: Option<u32>) -> io::Result<()> {
        if let Some(mode) = permissions {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))?;
        }
        if let Some(modified) = modified {
            let file = std::fs::File::options().write(true).open(path)?;
            file.set_modified(modified)?;
        }
        Ok(())
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(LocalFs))
    }

    fn display_path(&self, path: &Path) -> String {
        path.display().to_string()
    }
}

/// The local file system pretending to be remote, so tests can run the
/// streaming copy code that remote transfers use.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct FakeRemote;

#[cfg(test)]
impl Vfs for FakeRemote {
    fn key(&self) -> String {
        "fake".into()
    }
    fn is_local(&self) -> bool {
        false
    }
    fn label(&self) -> String {
        "fake".into()
    }
    fn home(&self) -> PathBuf {
        PathBuf::from("/")
    }
    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        LocalFs.list(dir)
    }
    fn stat(&self, path: &Path) -> io::Result<Entry> {
        LocalFs.stat(path)
    }
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        LocalFs.create_dir(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        LocalFs.rename(from, to)
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        LocalFs.remove_file(path)
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        LocalFs.remove_dir(path)
    }
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        LocalFs.open_read(path)
    }
    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        LocalFs.open_write(path)
    }
    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, permissions: Option<u32>) -> io::Result<()> {
        LocalFs.set_attributes(path, modified, permissions)
    }
    fn connect_another(&self) -> io::Result<FsRef> {
        Ok(Arc::new(FakeRemote))
    }
    fn display_path(&self, path: &Path) -> String {
        format!("fake://{}", path.display())
    }
}
