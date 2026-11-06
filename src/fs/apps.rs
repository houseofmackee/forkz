//! Leftovers of apps: the settings, caches, and support files an app keeps
//! in `~/Library`, found by its bundle identifier (and, in a few places,
//! its name), so they can go to the Trash with it. Apple's own apps are
//! left alone.

use std::path::{Path, PathBuf};
use std::process::Command;

/// An app, as far as finding its leftovers goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    /// "Visual Studio Code".
    pub name: String,
    /// "com.microsoft.VSCode".
    pub id: String,
}

/// The name and bundle identifier of an app bundle (None for other items,
/// apps without an identifier, and Apple's apps).
pub fn app_info(path: &Path) -> Option<AppInfo> {
    let is_app = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("app"));
    let plist = path.join("Contents/Info.plist");
    if !is_app || !plist.is_file() {
        return None;
    }
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleIdentifier", "raw", "-o", "-"])
        .arg(&plist)
        .output()
        .ok()?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let name = path.file_stem()?.to_string_lossy().into_owned();
    (output.status.success() && is_safe_id(&id)).then_some(AppInfo { name, id })
}

/// A real identifier ("com.example.App") that is not Apple's.
fn is_safe_id(id: &str) -> bool {
    id.contains('.') && !id.starts_with("com.apple.") && !id.contains('/') && !id.contains("..")
}

/// The leftovers of an app in a home folder: what exists of the usual
/// places, in path order.
pub fn leftovers(home: &Path, app: &AppInfo) -> Vec<PathBuf> {
    if !is_safe_id(&app.id) {
        return Vec::new();
    }
    let library = home.join("Library");
    let id = &app.id;
    let mut found: Vec<PathBuf> = [
        format!("Application Support/{id}"),
        format!("Caches/{id}"),
        format!("Containers/{id}"),
        format!("Preferences/{id}.plist"),
        format!("Saved Application State/{id}.savedState"),
        format!("HTTPStorages/{id}"),
        format!("HTTPStorages/{id}.binarycookies"),
        format!("WebKit/{id}"),
        format!("Cookies/{id}.binarycookies"),
        format!("Logs/{id}"),
        format!("Application Scripts/{id}"),
    ]
    .iter()
    .map(|relative| library.join(relative))
    .collect();
    // By name too, where apps often use it (long enough to be specific,
    // and never a folder macOS itself uses).
    let name = app.name.trim();
    let reserved = ["Apple", "Caches", "Logs", "Preferences", "CrashReporter", "com.apple"];
    if name.chars().count() >= 4 && !name.contains('/') && !reserved.contains(&name) {
        for folder in ["Application Support", "Caches", "Logs"] {
            found.push(library.join(folder).join(name));
        }
    }
    // Names that only start or end with the identifier.
    let scan = |folder: &str, matches: &dyn Fn(&str) -> bool| -> Vec<PathBuf> {
        std::fs::read_dir(library.join(folder))
            .map(|items| {
                items
                    .filter_map(Result::ok)
                    .filter(|item| matches(&item.file_name().to_string_lossy()))
                    .map(|item| item.path())
                    .collect()
            })
            .unwrap_or_default()
    };
    found.extend(scan("Preferences/ByHost", &|n| n.starts_with(&format!("{id}.")) && n.ends_with(".plist")));
    found.extend(scan("LaunchAgents", &|n| n == format!("{id}.plist") || n.starts_with(&format!("{id}."))));
    found.extend(scan("Group Containers", &|n| n == format!("group.{id}") || n.ends_with(&format!(".{id}"))));
    found.retain(|path| path.symlink_metadata().is_ok());
    found.sort();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_an_apps_leftovers() {
        let home = tempfile::tempdir().unwrap();
        let lib = home.path().join("Library");
        for dir in [
            "Application Support/com.example.Writer",
            "Application Support/Writer Pro",
            "Caches/com.example.Writer",
            "Containers/com.example.Writer",
            "Saved Application State/com.example.Writer.savedState",
            "Group Containers/ABCDE12345.com.example.Writer",
            "Application Support/com.example.WriterHelper",
            "Caches/com.other.App",
        ] {
            std::fs::create_dir_all(lib.join(dir)).unwrap();
        }
        std::fs::create_dir_all(lib.join("Preferences/ByHost")).unwrap();
        std::fs::create_dir_all(lib.join("LaunchAgents")).unwrap();
        for file in [
            "Preferences/com.example.Writer.plist",
            "Preferences/ByHost/com.example.Writer.0A1B2C.plist",
            "Preferences/com.example.WriterHelper.plist",
            "LaunchAgents/com.example.Writer.updater.plist",
        ] {
            std::fs::write(lib.join(file), "").unwrap();
        }
        let app = AppInfo { name: "Writer Pro".into(), id: "com.example.Writer".into() };
        let found: Vec<String> = leftovers(home.path(), &app)
            .iter()
            .map(|p| p.strip_prefix(&lib).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            found,
            [
                "Application Support/Writer Pro",
                "Application Support/com.example.Writer",
                "Caches/com.example.Writer",
                "Containers/com.example.Writer",
                "Group Containers/ABCDE12345.com.example.Writer",
                "LaunchAgents/com.example.Writer.updater.plist",
                "Preferences/ByHost/com.example.Writer.0A1B2C.plist",
                "Preferences/com.example.Writer.plist",
                "Saved Application State/com.example.Writer.savedState",
            ]
        );
    }

    #[test]
    fn leaves_apple_and_odd_identifiers_alone() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("Library/Caches/com.apple.Safari")).unwrap();
        let apple = AppInfo { name: "Safari".into(), id: "com.apple.Safari".into() };
        assert!(leftovers(home.path(), &apple).is_empty());
        assert!(!is_safe_id("../../etc"));
        assert!(!is_safe_id("nodots"));
        assert!(is_safe_id("com.example.App"));
    }

    #[test]
    fn reads_an_apps_identifier() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Writer.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>CFBundleIdentifier</key><string>com.example.Writer</string></dict></plist>"#,
        )
        .unwrap();
        assert_eq!(app_info(&app), Some(AppInfo { name: "Writer".into(), id: "com.example.Writer".into() }));
        assert_eq!(app_info(dir.path()), None);
    }
}
