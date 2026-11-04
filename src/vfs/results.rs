//! Search results shown as a folder: listing its top shows what was found
//! (from anywhere below the folder searched), with their real paths, and
//! everything else goes to the file system searched. Opening a folder in
//! the results leaves them for that folder (Back returns).

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use super::{FsRef, Vfs};
use crate::fs::Entry;

pub struct ResultsFs {
    inner: FsRef,
    /// The folder searched; the results are listed there.
    root: PathBuf,
    title: String,
    entries: Vec<Entry>,
    /// Tells result sets apart (each search is its own place).
    serial: u64,
}

impl ResultsFs {
    pub fn new(inner: FsRef, root: PathBuf, title: String, entries: Vec<Entry>) -> Arc<ResultsFs> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        Arc::new(ResultsFs { inner, root, title, entries, serial: SERIAL.fetch_add(1, Ordering::Relaxed) })
    }
}

impl Vfs for ResultsFs {
    fn key(&self) -> String {
        format!("{}#search-{}", self.inner.key(), self.serial)
    }

    fn is_local(&self) -> bool {
        self.inner.is_local()
    }

    fn label(&self) -> String {
        self.inner.label()
    }

    fn home(&self) -> PathBuf {
        self.root.clone()
    }

    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        if dir != self.root {
            return self.inner.list(dir);
        }
        // What was found, less what has since gone (on the Mac).
        let local = self.inner.is_local();
        Ok(self.entries.iter().filter(|e| !local || e.path.symlink_metadata().is_ok()).cloned().collect())
    }

    fn stat(&self, path: &Path) -> io::Result<Entry> {
        self.inner.stat(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_file(path)
    }

    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_dir(path)
    }

    fn open_read(&self, path: &Path) -> io::Result<Box<dyn Read + Send>> {
        self.inner.open_read(path)
    }

    fn open_read_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Read + Send>> {
        self.inner.open_read_from(path, offset)
    }

    fn open_write(&self, path: &Path) -> io::Result<Box<dyn Write + Send>> {
        self.inner.open_write(path)
    }

    fn open_write_sized(&self, path: &Path, size: Option<u64>) -> io::Result<Box<dyn Write + Send>> {
        self.inner.open_write_sized(path, size)
    }

    fn open_write_from(&self, path: &Path, offset: u64) -> io::Result<Box<dyn Write + Send>> {
        self.inner.open_write_from(path, offset)
    }

    fn set_attributes(&self, path: &Path, modified: Option<SystemTime>, permissions: Option<u32>) -> io::Result<()> {
        self.inner.set_attributes(path, modified, permissions)
    }

    fn connect_another(&self) -> io::Result<FsRef> {
        if self.inner.is_local() { Ok(self.inner.clone()) } else { self.inner.connect_another() }
    }

    fn display_path(&self, path: &Path) -> String {
        self.inner.display_path(path)
    }

    fn supports_permissions(&self) -> bool {
        self.inner.supports_permissions()
    }

    fn read_only(&self) -> bool {
        self.inner.read_only()
    }

    fn outside(&self) -> Option<(FsRef, PathBuf)> {
        self.inner.outside()
    }

    fn browse_fs(&self) -> Option<FsRef> {
        Some(self.inner.clone())
    }

    fn title_for(&self, path: &Path) -> Option<String> {
        (path == self.root).then(|| self.title.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_results_at_the_top_and_passes_the_rest_through() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/hit.txt"), "x").unwrap();
        std::fs::write(dir.path().join("gone.txt"), "x").unwrap();
        let local = super::super::local();
        let hits = vec![
            local.stat(&dir.path().join("sub/hit.txt")).unwrap(),
            local.stat(&dir.path().join("gone.txt")).unwrap(),
        ];
        let results = ResultsFs::new(local.clone(), dir.path().to_path_buf(), "hits".into(), hits);
        assert_eq!(results.list(dir.path()).unwrap().len(), 2);
        std::fs::remove_file(dir.path().join("gone.txt")).unwrap();
        let listed = results.list(dir.path()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, dir.path().join("sub/hit.txt"));
        assert_eq!(results.list(&dir.path().join("sub")).unwrap().len(), 1);
        assert_eq!(results.title_for(dir.path()).as_deref(), Some("hits"));
        assert!(results.browse_fs().is_some() && results.is_local());
        assert_ne!(results.key(), local.key());
    }
}
