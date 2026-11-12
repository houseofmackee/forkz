//! The last listing of server and cloud folders visited this session, so
//! a folder shows at once when it is opened again while the fresh listing
//! loads (as Cyberduck and Transmit do). Folders on this Mac are read
//! directly and need none.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::fs::Entry;
use crate::vfs::FsRef;

/// Folders kept before the least recently used are let go.
const LIMIT: usize = 300;

#[derive(Default)]
struct Cache {
    /// (file system key, folder) to the listing and when it was last used.
    folders: HashMap<(String, PathBuf), (Vec<Entry>, u64)>,
    clock: u64,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Whether a file system's listings are kept: servers and cloud services,
/// not this Mac, archives (read from memory), or search results; and not
/// when turned off (Settings > Transfers > Server folders).
pub fn applies(fs: &FsRef) -> bool {
    !fs.is_local()
        && fs.browse_fs().is_none()
        && fs.outside().is_none()
        && super::current_settings().is_none_or(|s| s.transfers.cache_listings)
}

/// Lets go of every listing (the cache was turned off).
pub fn clear() {
    CACHE.with(|cache| cache.borrow_mut().folders.clear());
}

/// The last listing of a folder, if there is one.
pub fn get(fs: &FsRef, folder: &Path) -> Option<Vec<Entry>> {
    if !applies(fs) {
        return None;
    }
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.clock += 1;
        let now = cache.clock;
        cache.folders.get_mut(&(fs.key(), folder.to_path_buf())).map(|(entries, used)| {
            *used = now;
            entries.clone()
        })
    })
}

/// Keeps a folder's fresh listing.
pub fn put(fs: &FsRef, folder: &Path, entries: &[Entry]) {
    if applies(fs) {
        put_key(fs.key(), folder, entries);
    }
}

fn put_key(key: String, folder: &Path, entries: &[Entry]) {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.clock += 1;
        let now = cache.clock;
        cache.folders.insert((key, folder.to_path_buf()), (entries.to_vec(), now));
        if cache.folders.len() > LIMIT {
            let oldest = cache.folders.iter().min_by_key(|(_, (_, used))| *used).map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                cache.folders.remove(&oldest);
            }
        }
    });
}

/// A folder changed (forkz changed it): its listing is no longer known.
pub fn forget(fs: &FsRef, folder: &Path) {
    forget_key(&fs.key(), folder);
}

fn forget_key(key: &str, folder: &Path) {
    CACHE.with(|cache| {
        cache.borrow_mut().folders.retain(|(k, f), _| !(k == key && f == folder));
    });
}

/// Everything of a file system (after disconnecting from it).
pub fn forget_all(key: &str) {
    CACHE.with(|cache| cache.borrow_mut().folders.retain(|(k, _), _| k != key));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from("/a").join(name),
            kind: crate::fs::EntryKind::File,
            size: 1,
            modified: None,
            hidden: false,
            package: false,
            permissions: None,
            owner: None,
            group: None,
        }
    }

    fn cached(key: &str, folder: &str) -> Option<usize> {
        CACHE.with(|c| c.borrow().folders.get(&(key.to_string(), PathBuf::from(folder))).map(|(e, _)| e.len()))
    }

    #[test]
    fn keeps_forgets_and_lets_go_of_the_oldest() {
        put_key("box://1".into(), Path::new("/a"), &[entry("x"), entry("y")]);
        put_key("box://1".into(), Path::new("/b"), &[entry("z")]);
        put_key("s3://k@h".into(), Path::new("/a"), &[]);
        assert_eq!(cached("box://1", "/a"), Some(2));
        forget_key("box://1", Path::new("/a"));
        assert_eq!(cached("box://1", "/a"), None);
        assert_eq!(cached("box://1", "/b"), Some(1));
        forget_all("box://1");
        assert_eq!(cached("box://1", "/b"), None);
        assert_eq!(cached("s3://k@h", "/a"), Some(0));
        // Past the limit, the least recently used folder goes.
        for i in 0..LIMIT {
            put_key("sftp://h".into(), &PathBuf::from(format!("/f{i}")), &[]);
        }
        assert_eq!(cached("s3://k@h", "/a"), None);
        assert_eq!(cached("sftp://h", &format!("/f{}", LIMIT - 1)), Some(0));
        CACHE.with(|c| assert!(c.borrow().folders.len() <= LIMIT));
    }

    #[test]
    fn only_for_servers() {
        assert!(!applies(&crate::vfs::local()));
    }
}
