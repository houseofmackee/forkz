//! The state of one file pane: where it is, what it shows, and its history.
//! The view (ui::pane_view) renders this and forwards user actions to it.

use std::path::{Path, PathBuf};

use crate::fs::{self, Entry, Sort};
use crate::vfs::{self, FsRef};

/// A place in history: a folder on a file system.
type Place = (FsRef, PathBuf);

/// How a pane shows its folder (View > as List, as Icons, as Columns).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewMode {
    #[default]
    List,
    Icons,
    Columns,
}

/// How the list is grouped (View > Group By).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GroupBy {
    #[default]
    None,
    Kind,
    Modified,
    Size,
}

impl GroupBy {
    pub const ALL: [GroupBy; 4] = [GroupBy::None, GroupBy::Kind, GroupBy::Modified, GroupBy::Size];

    pub fn title(self) -> &'static str {
        match self {
            GroupBy::None => "None",
            GroupBy::Kind => "Kind",
            GroupBy::Modified => "Date Modified",
            GroupBy::Size => "Size",
        }
    }
}

/// A row of the list: "..", a group's title, or an entry (its index in
/// `entries()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Parent,
    Header(String),
    Entry(usize),
}

/// The kind of an entry for Group By Kind ("Folder", "PDF document"); the
/// view gives macOS's names.
pub type KindLabel = fn(&Entry) -> String;

fn plain_kind(entry: &Entry) -> String {
    if entry.is_folder_like() {
        "Folder".into()
    } else {
        match entry.path.extension() {
            Some(ext) => format!("{} file", ext.to_string_lossy().to_uppercase()),
            None => "Document".into(),
        }
    }
}

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
    /// Only names containing this are shown (case-insensitive); "" for all.
    filter_text: String,
    /// Sizes of folders calculated in this folder, kept across reloads.
    folder_sizes: std::collections::HashMap<PathBuf, u64>,
    group: GroupBy,
    kind_label: KindLabel,
    /// Items the filename filters hide, and which version of the filters.
    exclusions: Option<crate::fs::filters::Exclusions>,
    exclusions_version: u64,
    /// How many items the filters hide here.
    filtered_out: usize,
    /// The list's rows: "..", group titles, and `visible` (grouped).
    rows: Vec<Row>,
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
            filter_text: String::new(),
            folder_sizes: std::collections::HashMap::new(),
            group: GroupBy::None,
            kind_label: plain_kind,
            exclusions: None,
            exclusions_version: 0,
            filtered_out: 0,
            rows: Vec::new(),
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
            Ok(entries) => {
                self.all = entries;
                self.apply_sizes_and_sort();
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

    /// Every item in the folder, hidden and filtered out ones too.
    pub fn all_entries(&self) -> &[Entry] {
        &self.all
    }

    pub fn entry(&self, row: usize) -> Option<&Entry> {
        self.visible.get(row)
    }

    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// The display name of the location ("Macintosh HD" for "/").
    pub fn title(&self) -> String {
        if let Some(title) = self.fs.title_for(&self.location) {
            return title;
        }
        if self.fs.is_local() || self.location.parent().is_some() { title_of(&self.location) } else { self.fs.label() }
    }

    /// Reads the folder again, now (blocking even for remote folders).
    /// Locally, falls back to the nearest existing parent if the folder is
    /// gone (deleted, or its volume ejected).
    pub fn reload(&mut self) {
        // Remote folders, and views such as search results, list through
        // their file system.
        if !self.fs.is_local() || self.fs.title_for(&self.location).is_some() {
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
            Ok(entries) => {
                self.all = entries;
                self.apply_sizes_and_sort();
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
        let needle = self.filter_text.to_lowercase();
        for entry in &mut self.all {
            if let Some(size) = self.folder_sizes.get(&entry.path) {
                entry.size = *size;
            }
        }
        let excluded = |e: &Entry| self.exclusions.as_ref().is_some_and(|x| x.excludes(&e.name, e.is_folder_like()));
        self.filtered_out = self.all.iter().filter(|e| (self.show_hidden || !e.hidden) && excluded(e)).count();
        self.visible = self
            .all
            .iter()
            .filter(|e| self.show_hidden || !e.hidden)
            .filter(|e| !excluded(e))
            .filter(|e| needle.is_empty() || e.name.to_lowercase().contains(&needle))
            .cloned()
            .collect();
        self.group_rows();
    }

    /// Orders `visible` by group (keeping the sort within each) and lays
    /// out the rows.
    fn group_rows(&mut self) {
        let mut keys: Vec<(i64, String)> = Vec::new();
        if self.group != GroupBy::None {
            let today = local_date(std::time::SystemTime::now());
            keys = self.visible.iter().map(|e| group_key(self.group, e, today, self.kind_label)).collect();
            let mut order: Vec<usize> = (0..self.visible.len()).collect();
            order.sort_by(|&a, &b| {
                keys[a].0.cmp(&keys[b].0).then_with(|| keys[a].1.to_lowercase().cmp(&keys[b].1.to_lowercase()))
            });
            self.visible = order.iter().map(|&i| self.visible[i].clone()).collect();
            keys = order.iter().map(|&i| keys[i].clone()).collect();
        }
        self.rows.clear();
        if self.can_go_up() {
            self.rows.push(Row::Parent);
        }
        for index in 0..self.visible.len() {
            if let Some((_, label)) = keys.get(index)
                && (index == 0 || keys[index - 1].1 != *label)
            {
                self.rows.push(Row::Header(label.clone()));
            }
            self.rows.push(Row::Entry(index));
        }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// The entry in a list row (None for ".." and group titles).
    pub fn entry_at_row(&self, row: usize) -> Option<&Entry> {
        match self.rows.get(row) {
            Some(Row::Entry(index)) => self.visible.get(*index),
            _ => None,
        }
    }

    /// The list row of an entry.
    pub fn table_row_of(&self, path: &Path) -> Option<usize> {
        let index = self.row_of(path)?;
        self.rows.iter().position(|r| *r == Row::Entry(index))
    }

    pub fn group_by(&self) -> GroupBy {
        self.group
    }

    /// Groups the list (`kind` names kinds for Group By Kind).
    pub fn set_group_by(&mut self, group: GroupBy, kind: KindLabel) {
        self.kind_label = kind;
        if self.group != group {
            self.group = group;
            self.filter();
        }
    }

    fn apply_sizes_and_sort(&mut self) {
        for entry in &mut self.all {
            if let Some(size) = self.folder_sizes.get(&entry.path) {
                entry.size = *size;
            }
        }
        fs::sort_entries(&mut self.all, self.sort);
    }

    pub fn filter_text(&self) -> &str {
        &self.filter_text
    }

    /// Shows only names containing `text` ("" shows everything).
    pub fn set_filter(&mut self, text: &str) {
        self.filter_text = text.to_string();
        self.filter();
    }

    /// How many items there are without the filter (hidden ones aside).
    /// Hides what the filename filters name (`version` changes with them,
    /// so the list is only filtered again when they do).
    pub fn set_exclusions(&mut self, exclusions: Option<crate::fs::filters::Exclusions>, version: u64) {
        if self.exclusions_version != version {
            self.exclusions = exclusions;
            self.exclusions_version = version;
            self.filter();
        }
    }

    /// How many items the filename filters hide here.
    pub fn filtered_out(&self) -> usize {
        self.filtered_out
    }

    pub fn unfiltered_count(&self) -> usize {
        self.all.iter().filter(|e| self.show_hidden || !e.hidden).count()
    }

    /// Records a folder's calculated size (shown, and used for sorting).
    pub fn set_folder_size(&mut self, path: &Path, size: u64) {
        self.folder_sizes.insert(path.to_path_buf(), size);
        self.filter();
        fs::sort_entries(&mut self.all, self.sort);
        self.filter();
    }

    pub fn folder_size(&self, path: &Path) -> Option<u64> {
        self.folder_sizes.get(path).copied()
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
        // Leaving search results for a folder in them: the real one.
        let fs = match self.fs.browse_fs() {
            Some(inner) if path != self.fs.home() => inner,
            // Leaving a mounted share's folder: the Mac.
            _ if !self.fs.contains(&path) => vfs::local(),
            _ => self.fs.clone(),
        };
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
        self.filter_text.clear();
        self.folder_sizes.clear();
        self.load();
    }

    /// Shows another folder of the same file system without recording
    /// history (browsing in column view, where columns show the way).
    pub fn browse(&mut self, path: PathBuf) {
        if path == self.location {
            return;
        }
        self.location = path;
        self.all.clear();
        self.visible.clear();
        self.filter_text.clear();
        self.folder_sizes.clear();
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
        // Search results: up is the folder searched.
        if let Some(inner) = self.fs.browse_fs()
            && self.location == self.fs.home()
        {
            let folder = self.location.clone();
            self.navigate_to(inner, folder);
            return Some(PathBuf::new());
        }
        // At the top of an archive: out to the folder it is in.
        if self.location.parent().is_none()
            && let Some((fs, file)) = self.fs.outside()
        {
            let folder = file.parent().map_or_else(|| fs.home(), Path::to_path_buf);
            self.navigate_to(fs, folder);
            return Some(file);
        }
        let parent = self.location.parent()?.to_path_buf();
        let came_from = self.location.clone();
        self.navigate(parent);
        Some(came_from)
    }

    /// There is somewhere to go up to (".." is shown).
    pub fn can_go_up(&self) -> bool {
        self.location.parent().is_some() || self.fs.outside().is_some()
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
        self.filter_text.clear();
        self.folder_sizes.clear();
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
        // iCloud Drive's folder, as Finder names it.
        Some(name) if name == ICLOUD_FOLDER && path.ends_with(ICLOUD_DRIVE) => "iCloud Drive".to_string(),
        Some(name) => name.to_string_lossy().into_owned(),
        None => "Macintosh HD".to_string(),
    }
}

/// iCloud Drive, relative to the home folder.
pub const ICLOUD_DRIVE: &str = "Library/Mobile Documents/com~apple~CloudDocs";
const ICLOUD_FOLDER: &str = "com~apple~CloudDocs";

/// iCloud Drive's folder, if iCloud Drive is on.
pub fn icloud_drive() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|home| home.join(ICLOUD_DRIVE)).filter(|path| path.is_dir())
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

/// A local calendar date (year, month 1 to 12, day).
type Date = (i32, u32, u32);

fn local_date(time: std::time::SystemTime) -> Date {
    let seconds = match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as libc::time_t,
        Err(_) => 0,
    };
    // SAFETY: localtime_r fills the tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&seconds, &mut tm) };
    (tm.tm_year + 1900, (tm.tm_mon + 1) as u32, tm.tm_mday as u32)
}

/// Days since 1970-01-01 of a calendar date (Howard Hinnant's algorithm).
fn day_number((y, m, d): Date) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(d) - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A date's group, as Finder names them: Today, Yesterday, Previous 7
/// Days, Previous 30 Days, a month of this year, or a year. Newest first.
fn date_group(today: Date, date: Date) -> (i64, String) {
    let ago = day_number(today) - day_number(date);
    match ago {
        i64::MIN..=0 => (0, "Today".into()),
        1 => (1, "Yesterday".into()),
        2..=7 => (2, "Previous 7 Days".into()),
        8..=30 => (3, "Previous 30 Days".into()),
        _ if date.0 == today.0 => (100 + 12 - i64::from(date.1), MONTHS[(date.1 - 1) as usize].into()),
        _ => (1000 + i64::from(today.0 - date.0), date.0.to_string()),
    }
}

/// An entry's group (order, then title) for a grouping.
fn group_key(group: GroupBy, entry: &Entry, today: Date, kind: KindLabel) -> (i64, String) {
    match group {
        GroupBy::None => (0, String::new()),
        GroupBy::Kind => (i64::from(!entry.is_folder_like()), kind(entry)),
        GroupBy::Modified => match entry.modified {
            Some(time) => date_group(today, local_date(time)),
            None => (i64::MAX, "Unknown Date".into()),
        },
        GroupBy::Size => {
            const MB: u64 = 1_000_000;
            match entry.size {
                _ if entry.is_folder_like() => (0, "Folders".into()),
                s if s >= 1000 * MB => (1, "Huge (1 GB or more)".into()),
                s if s >= 100 * MB => (2, "Large (100 MB to 1 GB)".into()),
                s if s >= MB => (3, "Medium (1 to 100 MB)".into()),
                s if s >= 100_000 => (4, "Small (100 KB to 1 MB)".into()),
                0 => (6, "Empty".into()),
                _ => (5, "Tiny (under 100 KB)".into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_groups_like_finder() {
        let today = (2026, 11, 5);
        let group = |date| date_group(today, date).1;
        assert_eq!(group((2026, 11, 5)), "Today");
        assert_eq!(group((2026, 11, 6)), "Today", "the future counts as today");
        assert_eq!(group((2026, 11, 4)), "Yesterday");
        assert_eq!(group((2026, 10, 30)), "Previous 7 Days");
        assert_eq!(group((2026, 10, 10)), "Previous 30 Days");
        assert_eq!(group((2026, 3, 1)), "March");
        assert_eq!(group((2024, 12, 31)), "2024");
        // Across a year end, and newest groups first.
        assert_eq!(date_group((2027, 1, 1), (2026, 12, 31)).1, "Yesterday");
        let order = |date| date_group(today, date).0;
        assert!(order((2026, 11, 4)) < order((2026, 3, 1)) && order((2026, 3, 1)) < order((2025, 6, 1)));
        assert!(order((2026, 9, 1)) < order((2026, 2, 1)));
    }

    #[test]
    fn grouped_rows_have_titles_and_keep_the_sort() {
        let dir = folder();
        std::fs::write(dir.path().join("a.md"), vec![b'x'; 200_000]).unwrap();
        let mut pane = PaneState::new(vfs::local(), dir.path().to_path_buf(), Sort::default(), false);
        assert!(pane.rows().iter().all(|r| !matches!(r, Row::Header(_))));
        pane.set_group_by(GroupBy::Size, plain_kind);
        let titles: Vec<String> =
            pane.rows().iter().filter_map(|r| if let Row::Header(t) = r { Some(t.clone()) } else { None }).collect();
        assert_eq!(titles, ["Folders", "Small (100 KB to 1 MB)", "Tiny (under 100 KB)"]);
        assert_eq!(pane.rows()[0], Row::Parent);
        let names: Vec<&str> =
            (0..pane.rows().len()).filter_map(|r| pane.entry_at_row(r)).map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "a.md", "b.txt"]);
        let row = pane.table_row_of(&dir.path().join("b.txt")).unwrap();
        assert_eq!(pane.entry_at_row(row).unwrap().name, "b.txt");
        pane.set_group_by(GroupBy::Kind, plain_kind);
        let titles: Vec<String> =
            pane.rows().iter().filter_map(|r| if let Row::Header(t) = r { Some(t.clone()) } else { None }).collect();
        assert_eq!(titles, ["Folder", "MD file", "TXT file"]);
        pane.set_group_by(GroupBy::None, plain_kind);
        assert_eq!(pane.rows().len(), 4, "\"..\" and three items");
    }

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
    fn filter_and_folder_sizes() {
        let dir = folder();
        std::fs::write(dir.path().join("Banana.txt"), "").unwrap();
        std::fs::create_dir(dir.path().join("aaa")).unwrap();
        let root = dir.path().to_path_buf();
        let mut pane = PaneState::new(vfs::local(), root.clone(), Sort::default(), false);
        pane.set_filter("an");
        let names: Vec<_> = pane.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Banana.txt"], "case-insensitive, hidden items stay hidden");
        assert_eq!(pane.unfiltered_count(), 4);
        pane.set_filter("");
        // A calculated folder size shows and sorts.
        pane.set_sort(Sort { key: crate::fs::SortKey::Size, ascending: false });
        pane.set_folder_size(&root.join("aaa"), 10);
        pane.set_folder_size(&root.join("sub"), 9_999);
        let order = |p: &PaneState| p.entries().iter().map(|e| e.name.clone()).take(2).collect::<Vec<_>>();
        assert_eq!(order(&pane), ["sub", "aaa"], "largest folder first");
        pane.reload();
        assert_eq!(pane.folder_size(&root.join("sub")), Some(9_999), "kept across reloads");
        assert_eq!(order(&pane), ["sub", "aaa"], "and still sorted by it");
        pane.navigate(root.join("sub"));
        pane.go_back();
        assert_eq!(pane.folder_size(&root.join("sub")), None, "forgotten when leaving");
        pane.set_filter("x");
        pane.navigate(root.join("sub"));
        assert_eq!(pane.filter_text(), "", "the filter is per folder visit");
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
