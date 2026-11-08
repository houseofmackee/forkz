//! iCloud Drive as Finder shows it: your files (in iCloud's own container,
//! `com~apple~CloudDocs`) together with the folders of apps that keep their
//! documents in iCloud (Keynote, Numbers, Preview, and so on), named after
//! the apps. The containers macOS hides (apps' private data) stay out, and
//! iCloud Drive is a top: there is no going up from it to the folder of
//! containers.

use std::path::{Path, PathBuf};

use super::Entry;

/// iCloud Drive, relative to the home folder.
pub const DRIVE: &str = "Library/Mobile Documents/com~apple~CloudDocs";
const DRIVE_FOLDER: &str = "com~apple~CloudDocs";

/// iCloud Drive's folder, if iCloud Drive is on.
pub fn drive() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(DRIVE)).filter(|path| path.is_dir())
}

/// Whether `path` is iCloud Drive's folder.
pub fn is_drive(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == DRIVE_FOLDER) && path.ends_with(DRIVE)
}

/// An app's container (for its Documents folder), if `path` is one.
fn container_of(documents: &Path) -> Option<&Path> {
    if documents.file_name()? != "Documents" {
        return None;
    }
    let container = documents.parent()?;
    let containers = container.parent()?;
    (containers.file_name()? == "Mobile Documents" && container.file_name()? != DRIVE_FOLDER).then_some(container)
}

/// An app's iCloud folder's name, as Finder shows it ("Numbers"), if
/// `path` is one (an app container's Documents folder).
pub fn app_folder_name(path: &Path) -> Option<String> {
    container_of(path).map(display_name)
}

/// Where going up from `path` leads, if iCloud Drive changes it: from an
/// app's folder, to iCloud Drive. (iCloud Drive itself has nothing above.)
pub fn parent_of(path: &Path) -> Option<PathBuf> {
    let container = container_of(path)?;
    Some(container.parent()?.join(DRIVE_FOLDER))
}

/// The path bar inside iCloud Drive (from iCloud Drive down, never the
/// folder of containers above it): each folder with its title. None
/// outside iCloud Drive.
pub fn path_chain(location: &Path) -> Option<Vec<(PathBuf, String)>> {
    // iCloud Drive's folder, from the location itself.
    let (drive, rest, app): (PathBuf, &Path, Option<(PathBuf, String)>) = match location
        .ancestors()
        .find(|a| is_drive(a))
    {
        Some(drive) => (drive.to_path_buf(), location.strip_prefix(drive).ok()?, None),
        None => {
            // In an app's folder: its Documents folder and what is below.
            let documents = location.ancestors().find(|a| container_of(a).is_some())?;
            let container = container_of(documents)?;
            let drive = container.parent()?.join(DRIVE_FOLDER);
            (drive, location.strip_prefix(documents).ok()?, Some((documents.to_path_buf(), display_name(container))))
        }
    };
    let mut chain = vec![(drive, "iCloud Drive".to_string())];
    let mut folder = match app {
        Some((documents, name)) => {
            chain.push((documents.clone(), name));
            documents
        }
        None => chain[0].0.clone(),
    };
    for part in rest.components() {
        folder = folder.join(part);
        chain.push((folder.clone(), part.as_os_str().to_string_lossy().into_owned()));
    }
    Some(chain)
}

/// The name macOS gives a container ("com~apple~Numbers" is "Numbers").
fn display_name(container: &Path) -> String {
    use objc2_foundation::{NSFileManager, NSString};
    let raw = container.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let shown = NSFileManager::defaultManager()
        .displayNameAtPath(&NSString::from_str(&container.to_string_lossy()))
        .to_string();
    if !shown.is_empty() && shown != raw && !shown.contains('~') {
        return shown;
    }
    // "TEAMID~com~company~App": the last part.
    raw.rsplit('~').next().unwrap_or(&raw).to_string()
}

/// The folders of apps that show in iCloud Drive: containers macOS does
/// not hide, that have a Documents folder.
pub fn app_folders(drive: &Path) -> Vec<Entry> {
    let Some(containers) = drive.parent() else { return Vec::new() };
    let Ok(items) = std::fs::read_dir(containers) else { return Vec::new() };
    items
        .filter_map(Result::ok)
        .map(|item| item.path())
        .filter(|container| container.file_name().is_some_and(|n| n != DRIVE_FOLDER))
        .filter(|container| Entry::from_path(container).is_ok_and(|e| !e.hidden))
        .filter_map(|container| {
            let documents = container.join("Documents");
            let mut entry = Entry::from_path(&documents).ok().filter(Entry::is_folder_like)?;
            entry.name = display_name(&container);
            Some(entry)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knows_icloud_paths() {
        let home = Path::new("/Users/me");
        let drive = home.join(DRIVE);
        assert!(is_drive(&drive));
        assert!(!is_drive(home));
        let numbers = home.join("Library/Mobile Documents/com~apple~Numbers/Documents");
        assert_eq!(parent_of(&numbers), Some(drive.clone()));
        assert!(parent_of(&drive).is_none());
        assert!(parent_of(&drive.join("Documents")).is_none(), "a folder named Documents in iCloud Drive");
        assert_eq!(display_name(Path::new("/nowhere/57T9237FN3~net~whatsapp~WhatsApp")), "WhatsApp");
    }

    #[test]
    fn path_bar_starts_at_icloud_drive() {
        let home = Path::new("/Users/me");
        let drive = home.join(DRIVE);
        let titles = |path: &Path| path_chain(path).map(|c| c.into_iter().map(|(_, t)| t).collect::<Vec<_>>());
        assert_eq!(titles(&drive), Some(vec!["iCloud Drive".to_string()]));
        assert_eq!(titles(&drive.join("Work/2026")), Some(vec!["iCloud Drive".into(), "Work".into(), "2026".into()]));
        let numbers = home.join("Library/Mobile Documents/com~apple~Numbers/Documents/Budgets");
        let chain = path_chain(&numbers).unwrap();
        assert_eq!(chain[0].0, drive);
        assert_eq!(chain.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(), ["iCloud Drive", "Numbers", "Budgets"]);
        assert_eq!(chain[2].0, numbers);
        assert_eq!(titles(home), None);
        assert_eq!(titles(&home.join("Library/Mobile Documents")), None);
    }

    #[test]
    fn lists_visible_app_folders() {
        let home = tempfile::tempdir().unwrap();
        let containers = home.path().join("Library/Mobile Documents");
        let drive = containers.join(DRIVE_FOLDER);
        std::fs::create_dir_all(&drive).unwrap();
        std::fs::create_dir_all(containers.join("com~apple~Numbers/Documents")).unwrap();
        std::fs::create_dir_all(containers.join("com~apple~mail")).unwrap();
        // Hidden (as macOS hides apps' private containers).
        let private = containers.join("com~apple~notes");
        std::fs::create_dir_all(private.join("Documents")).unwrap();
        let path = std::ffi::CString::new(private.to_string_lossy().as_bytes()).unwrap();
        unsafe { libc::chflags(path.as_ptr(), libc::UF_HIDDEN as _) };
        let names: Vec<String> = app_folders(&drive).into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["Numbers"]);
    }
}
