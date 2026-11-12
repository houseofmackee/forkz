//! App Cleaner (Commands > App Cleaner), as AppCleaner does it: apps and
//! everything they left on the Mac, in one list to move to the Trash. The
//! same list (without the apps) is offered after an app goes to the Trash.
//! Files for all users start unchecked: they take an administrator's
//! password (Finder asks once for all of them).

use std::path::{Path, PathBuf};

use objc2_foundation::NSArray;

use super::dialogs::{self, Choice};
use super::util::{file_url, mtm, ns, url_path};
use crate::fs::apps::{self, AppInfo};
use crate::fs::ops::{Job, JobKind};

/// Where items for all users are (for tests, another folder).
const SYSTEM_ROOT: &str = "/";

/// "~/Library/Caches/com.example.App", or the full path outside the home
/// folder; background services and helper tools say so.
fn label(path: &Path, home: &Path) -> String {
    let shown = match path.strip_prefix(home) {
        Ok(inside) => format!("~/{}", inside.display()),
        Err(_) => path.display().to_string(),
    };
    let kind = path.parent().and_then(Path::file_name).map(|f| f.to_string_lossy().into_owned());
    match kind.as_deref() {
        _ if !apps::is_service(path) => shown,
        Some("PrivilegedHelperTools") => format!("{shown} (helper tool)"),
        _ => format!("{shown} (background service)"),
    }
}

/// The leftovers of apps, in groups: the home folder's (checked) and those
/// for all users (unchecked), leaving out what is inside `except` (the apps
/// themselves).
fn leftover_groups(app_list: &[AppInfo], except: &[PathBuf], home: &Path) -> Vec<(String, Vec<Choice>)> {
    let keep = |path: &PathBuf| !except.iter().any(|e| path.starts_with(e));
    let mut user: Vec<PathBuf> = app_list.iter().flat_map(|app| apps::leftovers(home, app)).filter(keep).collect();
    let mut system: Vec<PathBuf> =
        app_list.iter().flat_map(|app| apps::system_leftovers(Path::new(SYSTEM_ROOT), app)).filter(keep).collect();
    for list in [&mut user, &mut system] {
        list.sort();
        list.dedup();
    }
    let choices = |paths: Vec<PathBuf>, checked: bool| -> Vec<Choice> {
        paths.into_iter().map(|path| Choice { label: label(&path, home), path, checked }).collect()
    };
    vec![
        ("In your Library".to_string(), choices(user, true)),
        ("For all users (needs an administrator\u{2019}s password)".to_string(), choices(system, false)),
    ]
}

/// After apps went to the Trash: offers to move what they left there too.
pub fn offer_leftovers(app_list: &[AppInfo], trashed: &[PathBuf]) {
    let Some(home) = dirs::home_dir() else { return };
    let groups = leftover_groups(app_list, trashed, &home);
    if groups.iter().all(|(_, items)| items.is_empty()) {
        return;
    }
    let names = names_of(app_list);
    let message = format!("Also move the leftover files of {names} to the Trash?");
    let informative = "Settings, caches, and support files the app left on your Mac. Those for all users need an administrator\u{2019}s password; background services keep running until you restart. Uncheck any you want to keep.";
    if let Some(chosen) = dialogs::choose_items(&message, informative, "Move to Trash", "Keep", &groups) {
        super::start_job(Job::local(JobKind::Trash, chosen, home.join("Library")));
    }
}

fn names_of(app_list: &[AppInfo]) -> String {
    match app_list {
        [one] => format!("\u{201c}{}\u{201d}", one.name),
        _ => "these apps".to_string(),
    }
}

/// Commands > App Cleaner: the apps selected in the pane (or chosen), with
/// everything they left, to move to the Trash.
pub fn run(selected: Vec<PathBuf>) {
    let chosen: Vec<PathBuf> = selected.into_iter().filter(|p| apps::app_info(p).is_some()).collect();
    let app_paths = if chosen.is_empty() { choose_apps() } else { chosen };
    if app_paths.is_empty() {
        return;
    }
    let found: Vec<(PathBuf, AppInfo)> =
        app_paths.iter().filter_map(|p| apps::app_info(p).map(|info| (p.clone(), info))).collect();
    if found.is_empty() {
        dialogs::show_info(
            "App Cleaner leaves this app alone.".into(),
            "It is one of Apple\u{2019}s apps, or it has no identifier to find its files by.".into(),
        );
        return;
    }
    let Some(home) = dirs::home_dir() else { return };
    let (paths, infos): (Vec<PathBuf>, Vec<AppInfo>) = found.into_iter().unzip();
    let mut groups = vec![(
        if paths.len() == 1 { "App".to_string() } else { "Apps".to_string() },
        paths.iter().map(|path| Choice { path: path.clone(), label: label(path, &home), checked: true }).collect(),
    )];
    groups.extend(leftover_groups(&infos, &paths, &home));
    let names = names_of(&infos);
    let count: usize = groups.iter().map(|(_, items)| items.len()).sum::<usize>() - paths.len();
    let informative = match count {
        0 => "Nothing else of it was found on this Mac.".to_string(),
        1 => "1 file or folder it left was found. Those for all users need an administrator\u{2019}s password; background services keep running until you restart.".to_string(),
        n => format!("{n} files and folders it left were found. Those for all users need an administrator\u{2019}s password; background services keep running until you restart."),
    };
    if let Some(chosen) =
        dialogs::choose_items(&format!("Remove {names}?"), &informative, "Move to Trash", "Cancel", &groups)
    {
        super::start_job(Job::local(JobKind::Trash, chosen, home.join("Library")));
    }
}

/// Asks for apps (in Applications).
fn choose_apps() -> Vec<PathBuf> {
    let panel = objc2_app_kit::NSOpenPanel::openPanel(mtm());
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(true);
    panel.setTreatsFilePackagesAsDirectories(false);
    panel.setMessage(Some(&ns("Choose apps to remove, with the files they left.")));
    panel.setPrompt(Some(&ns("Choose")));
    if let Some(kind) = objc2_uniform_type_identifiers::UTType::typeWithIdentifier(&ns("com.apple.application-bundle"))
    {
        panel.setAllowedContentTypes(&NSArray::from_retained_slice(&[kind]));
    }
    panel.setDirectoryURL(Some(&file_url(Path::new("/Applications"))));
    if panel.runModal() != objc2_app_kit::NSModalResponseOK {
        return Vec::new();
    }
    panel.URLs().iter().filter_map(|u| url_path(&u)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_say_where_and_what() {
        let home = Path::new("/Users/me");
        assert_eq!(label(Path::new("/Users/me/Library/Caches/x"), home), "~/Library/Caches/x");
        assert_eq!(
            label(Path::new("/Library/LaunchDaemons/com.example.App.plist"), home),
            "/Library/LaunchDaemons/com.example.App.plist (background service)"
        );
        assert_eq!(
            label(Path::new("/Library/PrivilegedHelperTools/com.example.App.helper"), home),
            "/Library/PrivilegedHelperTools/com.example.App.helper (helper tool)"
        );
        assert_eq!(
            label(Path::new("/Library/Preferences/com.example.App.plist"), home),
            "/Library/Preferences/com.example.App.plist"
        );
    }
}
