//! The state of one file pane: where it is, what it shows, and its history.
//! The view (ui::pane_view) renders this and forwards user actions to it.

use std::path::{Path, PathBuf};

use crate::fs::{self, Entry, Sort};
use crate::vfs::{self, FsRef};

/// A place in history: a folder on a file system.
type Place = (FsRef, PathBuf);

#[derive(Debug)]
pub struct PaneState {
    fs: FsRef,
    location: PathBuf,
    /// Everything in the folder, sorted; hidden items included.
    all: Vec<Entry>,
    /// What is shown: `all` without hidden items unless they are shown.
    visible: Vec<Entry>,
    pub sort: Sort,
    show_hidden: bool,
    back: Vec<Place>,
    forward: Vec<Place>,
    /// Why the folder could not be read, shown instead of the list.
    pub error: Option<String>,
    /// A remote listing is on its way.
    pub loading: bool,
    /// Counts loads, so a slow listing for a folder already left is ignored.
    generation: u64,
}

impl PaneState {
    pub fn new(fs: FsRef, location: PathBuf, sort: Sort, show_hidden: bool) -> Self {
        let mut pane = PaneState {
            fs,
            location,
            all: Vec::new(),
            visible: Vec::new(),
            sort,
            show_hidden,
            back: Vec::new(),
            forward: Vec::new(),
            error: None,
            loading: false,
            generation: 0,
        };
        pane.load();
        pane
    }

    pub fn fs(&self) -> &FsRef {
        &self.fs
    }

    pub fn is_local(&self) -> bool {
        self.fs.is_local()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Reads a local folder now; for a remote one, marks it as loading (the
    /// caller lists it in the background and calls `set_listing`).
    pub fn load(&mut self) {
        self.generation += 1;
        if self.fs.is_local() {
            self.reload();
        } else {
            self.loading = true;
            self.error = None;
        }
    }

    /// The result of a background listing; ignored if the pane has moved on.
    pub fn set_listing(&mut self, generation: u64, result: std::io::Result<Vec<Entry>>) -> bool {
        if generation != self.generation {
            return false;
        }
        self.loading = false;
        match result {
            Ok(mut entries) => {
                fs::sort_entries(&mut entries, self.sort);
                self.all = entries;
                self.error = None;
            }
            Err(e) => {
                self.all.clear();
                self.error = Some(e.to_string());
            }
        }
        self.filter();
        true
    }

    pub fn location(&self) -> &Path {
        &self.location
    }

    pub fn entries(&self) -> &[Entry] {
        &self.visible
    }

    pub fn entry(&self, row: usize) -> Option<&Entry> {
        self.visible.get(row)
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// The display name of the location ("Macintosh HD" for "/").
    pub fn title(&self) -> String {
        if self.fs.is_local() || self.location.parent().is_some() { title_of(&self.location) } else { self.fs.label() }
    }

    /// Reads the folder again, now (blocking even for remote folders).
    /// Locally, falls back to the nearest existing parent if the folder is
    /// gone (deleted, or its volume ejected).
    pub fn reload(&mut self) {
        if !self.fs.is_local() {
            let result = self.fs.list(&self.location);
            let generation = self.generation;
            self.set_listing(generation, result);
            return;
        }
        while !self.location.is_dir() {
            match self.location.parent() {
                Some(parent) => self.location = parent.to_path_buf(),
                None => break,
            }
        }
        match fs::list_dir(&self.location) {
            Ok(mut entries) => {
                fs::sort_entries(&mut entries, self.sort);
                self.all = entries;
                self.error = None;
            }
            Err(e) => {
                self.all.clear();
                self.error = Some(e.to_string());
            }
        }
        self.filter();
    }

    fn filter(&mut self) {
        self.visible = self.all.iter().filter(|e| self.show_hidden || !e.hidden).cloned().collect();
    }

    pub fn set_show_hidden(&mut self, show: bool) {
        self.show_hidden = show;
        self.filter();
    }

    pub fn set_sort(&mut self, sort: Sort) {
        self.sort = sort;
        fs::sort_entries(&mut self.all, sort);
        self.filter();
    }

    /// Goes to a folder on the same file system, recording the current one
    /// for Back.
    pub fn navigate(&mut self, path: PathBuf) {
        let fs = self.fs.clone();
        self.navigate_to(fs, path);
    }

    /// Goes to a folder, possibly on another file system (connecting to a
    /// server, or back to the Mac).
    pub fn navigate_to(&mut self, fs: FsRef, path: PathBuf) {
        if path == self.location && vfs::same(&fs, &self.fs) {
            self.load();
            return;
        }
        let previous_fs = std::mem::replace(&mut self.fs, fs);
        let previous = std::mem::replace(&mut self.location, path);
        self.back.push((previous_fs, previous));
        self.forward.clear();
        self.all.clear();
        self.visible.clear();
        self.load();
    }

    /// Forgets history on a file system (after disconnecting from it).
    pub fn forget(&mut self, key: &str) {
        self.back.retain(|(fs, _)| fs.key() != key);
        self.forward.retain(|(fs, _)| fs.key() != key);
    }

    /// Goes to the parent folder. Returns the folder it came from, to
    /// select it.
    pub fn go_up(&mut self) -> Option<PathBuf> {
        let parent = self.location.parent()?.to_path_buf();
        let came_from = self.location.clone();
        self.navigate(parent);
        Some(came_from)
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// Returns the folder it came from, to select it if it is a child.
    pub fn go_back(&mut self) -> Option<PathBuf> {
        let (fs, target) = self.back.pop()?;
        let came_from = self.swap_place(fs, target);
        self.forward.push(came_from.clone());
        Some(came_from.1)
    }

    pub fn go_forward(&mut self) -> Option<PathBuf> {
        let (fs, target) = self.forward.pop()?;
        let came_from = self.swap_place(fs, target);
        self.back.push(came_from.clone());
        Some(came_from.1)
    }

    fn swap_place(&mut self, fs: FsRef, path: PathBuf) -> Place {
        let changed_fs = !vfs::same(&fs, &self.fs);
        let previous = (std::mem::replace(&mut self.fs, fs), std::mem::replace(&mut self.location, path));
        if changed_fs || !self.fs.is_local() {
            self.all.clear();
            self.visible.clear();
        }
        self.load();
        previous
    }

    /// The row showing `path`, if any.
    pub fn row_of(&self, path: &Path) -> Option<usize> {
        self.visible.iter().position(|e| e.path == path)
    }

    /// The first row whose name starts with `prefix` (case-insensitive), for
    /// type-to-select.
    pub fn row_starting_with(&self, prefix: &str) -> Option<usize> {
        let prefix = prefix.to_lowercase();
        self.visible.iter().position(|e| e.name.to_lowercase().starts_with(&prefix))
    }
}

/// The display name of a folder: its name, or "Macintosh HD" for "/".
pub fn title_of(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => "Macintosh HD".to_string(),
    }
}

/// Summary for a status bar: "3 of 42 selected, 1.2 MB" or "42 items".
pub fn selection_summary(entries: &[Entry], selected: &[usize]) -> String {
    let items = |n: usize| if n == 1 { "1 item".to_string() } else { format!("{n} items") };
    if selected.is_empty() {
        return items(entries.len());
    }
    let bytes: u64 = selected.iter().filter_map(|&i| entries.get(i)).map(|e| e.size).sum();
    let mut text = format!("{} of {} selected", selected.len(), entries.len());
    if bytes > 0 {
        text.push_str(&format!(", {}", fs::format_size(bytes)));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub/deeper")).unwrap();
        std::fs::write(dir.path().join("b.txt"), "12345").unwrap();
        std::fs::write(dir.path().join(".secret"), "").unwrap();
        dir
    }

    #[test]
    fn hidden_items_toggle() {
        let dir = folder();
        let mut pane = PaneState::new(vfs::local(), dir.path().to_path_buf(), Sort::default(), false);
        let names = |p: &PaneState| p.entries().iter().map(|e| e.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&pane), ["sub", "b.txt"]);
        pane.set_show_hidden(true);
        assert_eq!(names(&pane), ["sub", ".secret", "b.txt"]);
    }

    #[test]
    fn history() {
        let dir = folder();
        let root = dir.path().to_path_buf();
        let mut pane = PaneState::new(vfs::local(), root.clone(), Sort::default(), false);
        pane.navigate(root.join("sub"));
        pane.navigate(root.join("sub/deeper"));
        assert_eq!(pane.go_up(), Some(root.join("sub/deeper")));
        assert_eq!(pane.location(), root.join("sub"));
        assert_eq!(pane.go_back(), Some(root.join("sub")));
        assert_eq!(pane.location(), root.join("sub/deeper"));
        assert!(pane.can_go_forward());
        pane.go_forward();
        assert_eq!(pane.location(), root.join("sub"));
        // A new navigation clears Forward.
        pane.go_back();
        pane.navigate(root.clone());
        assert!(!pane.can_go_forward());
    }

    #[test]
    fn falls_back_when_folder_disappears() {
        let dir = folder();
        let root = dir.path().to_path_buf();
        let mut pane = PaneState::new(vfs::local(), root.join("sub/deeper"), Sort::default(), false);
        std::fs::remove_dir_all(root.join("sub")).unwrap();
        pane.reload();
        assert_eq!(pane.location(), root);
    }

    #[test]
    fn remote_listings_load_in_the_background() {
        let dir = folder();
        let remote: FsRef = std::sync::Arc::new(crate::vfs::FakeRemote);
        let mut pane = PaneState::new(remote.clone(), dir.path().to_path_buf(), Sort::default(), false);
        assert!(pane.loading);
        assert!(pane.entries().is_empty());
        let stale = pane.generation();
        pane.navigate(dir.path().join("sub"));
        // The listing for the folder we left arrives late: ignored.
        assert!(!pane.set_listing(stale, remote.list(dir.path())));
        let current = pane.generation();
        assert!(pane.set_listing(current, remote.list(&dir.path().join("sub"))));
        assert_eq!(pane.entries().len(), 1);
        // Back to the Mac, and back again through history.
        pane.navigate_to(vfs::local(), dir.path().to_path_buf());
        assert!(pane.is_local() && !pane.loading);
        pane.go_back();
        assert!(!pane.is_local() && pane.loading);
    }

    #[test]
    fn summaries() {
        let dir = folder();
        let pane = PaneState::new(vfs::local(), dir.path().to_path_buf(), Sort::default(), false);
        assert_eq!(selection_summary(pane.entries(), &[]), "2 items");
        assert_eq!(selection_summary(pane.entries(), &[1]), "1 of 2 selected, 5 bytes");
        assert_eq!(pane.row_starting_with("B"), Some(1));
        assert_eq!(title_of(Path::new("/")), "Macintosh HD");
    }
}
