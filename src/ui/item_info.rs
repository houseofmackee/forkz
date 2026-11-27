//! View > Show Item Info: a line under each name in the icon view, as in
//! Finder: how many items a folder holds, an image's size in pixels, or how
//! long a movie or song is. It is read in the background, one item at a
//! time, and kept for the session by path, size, and date (a folder's date
//! changes when items come or go).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::Sender;
use std::time::UNIX_EPOCH;

use dispatch2::DispatchQueue;

use crate::fs::Entry;

/// An item as it is now: its path, size, and modification time.
type Key = (PathBuf, u64, u64);

/// Kept lines before they are let go.
const LIMIT: usize = 5000;

thread_local! {
    static KNOWN: RefCell<HashMap<Key, String>> = RefCell::new(HashMap::new());
    static ASKED: RefCell<HashSet<Key>> = RefCell::new(HashSet::new());
}

fn key(entry: &Entry) -> Key {
    let modified = entry.modified.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    (entry.path.clone(), entry.size, modified)
}

/// Whether an item can have a line: on this Mac (servers would need a
/// listing of every folder).
pub fn applies(entry: &Entry, local: bool) -> bool {
    local && Path::new(&entry.path).is_absolute()
}

/// The line for an item if it is known ("" for none); otherwise it is
/// asked for (once), and the icon views get it when it is ready.
pub fn get(entry: &Entry) -> Option<String> {
    let key = key(entry);
    if let Some(text) = KNOWN.with(|known| known.borrow().get(&key).cloned()) {
        return Some(text);
    }
    if ASKED.with(|asked| asked.borrow_mut().insert(key.clone())) {
        let folder = entry.is_folder_like();
        if let Some(worker) = worker() {
            let _ = worker.send((key, folder));
        }
    }
    None
}

/// The one thread that reads the lines, in the order asked.
fn worker() -> Option<&'static Sender<(Key, bool)>> {
    static WORKER: OnceLock<Option<Sender<(Key, bool)>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (send, receive) = std::sync::mpsc::channel::<(Key, bool)>();
            std::thread::Builder::new()
                .name("item info".into())
                .spawn(move || {
                    for (key, folder) in receive {
                        let text = read(&key.0, folder);
                        DispatchQueue::main().exec_async(move || ready(key, text));
                    }
                })
                .ok()
                .map(|_| send)
        })
        .as_ref()
}

/// A folder's count of items (as Finder counts them: not hidden ones), or
/// a file's size in pixels or length.
fn read(path: &Path, folder: bool) -> String {
    if folder {
        let Ok(items) = std::fs::read_dir(path) else { return String::new() };
        let count =
            items.filter_map(Result::ok).filter(|item| !item.file_name().to_string_lossy().starts_with('.')).count();
        return match count {
            0 => "Empty".into(),
            1 => "1 item".into(),
            n => format!("{n} items"),
        };
    }
    let meta = crate::fs::metadata::read(path);
    if let Some((width, height)) = meta.pixels {
        format!("{width} \u{d7} {height}")
    } else if let Some(seconds) = meta.duration {
        crate::fs::metadata::format_duration(seconds)
    } else {
        String::new()
    }
}

fn ready(key: Key, text: String) {
    let path = key.0.clone();
    let show = !text.is_empty();
    KNOWN.with(|known| {
        let mut known = known.borrow_mut();
        if known.len() >= LIMIT {
            known.clear();
            ASKED.with(|asked| asked.borrow_mut().clear());
        }
        known.insert(key, text);
    });
    if show {
        super::item_info_ready(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::read;

    #[test]
    fn counts_a_folders_items() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path(), true), "Empty");
        std::fs::write(dir.path().join("a.txt"), "a").unwrap();
        std::fs::write(dir.path().join(".hidden"), "h").unwrap();
        assert_eq!(read(dir.path(), true), "1 item", "hidden items are not counted");
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(read(dir.path(), true), "2 items");
        assert_eq!(read(&dir.path().join("a.txt"), false), "", "a text file has no line");
    }
}
