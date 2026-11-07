//! File > Import from Cyberduck, Import from WinSCP, Import Sites from
//! File, and Export Sites. (Import from FileZilla, which brings settings
//! and host keys too, is in mod.rs.)

use std::path::PathBuf;

use objc2_app_kit::{NSModalResponseOK, NSOpenPanel, NSSavePanel};

use super::dialogs;
use super::util::{file_url, mtm, ns, url_path};
use crate::sites::import::{self, Found};

/// Asks to add the sites found (those forkz has already are left out),
/// and adds them.
fn add_found(source: &str, found: Found) {
    let Some(app) = super::app() else { return };
    let new_sites: Vec<_> = {
        let store = app.sites.borrow();
        found
            .sites
            .iter()
            .filter(|site| !store.sites.iter().any(|s| s.same_account(site) && s.name == site.name))
            .cloned()
            .collect()
    };
    let already = found.sites.len() - new_sites.len();
    let count = |n: usize| if n == 1 { "1 site".to_string() } else { format!("{n} sites") };
    let mut detail = format!("\u{2022} {} to add", count(new_sites.len()));
    if already > 0 {
        detail.push_str(&format!("\n\u{2022} {} already in forkz (left as they are)", count(already)));
    }
    if !found.notes.is_empty() {
        let shown: Vec<&String> = found.notes.iter().take(12).collect();
        detail.push_str(&format!(
            "\n\nNot imported:\n\u{2022} {}",
            shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n\u{2022} ")
        ));
        if found.notes.len() > shown.len() {
            detail.push_str(&format!("\n\u{2022} and {} more", found.notes.len() - shown.len()));
        }
    }
    detail.push_str(
        "\n\nPasswords are not brought over: forkz asks for them when connecting, and can keep them in your Keychain.",
    );
    if new_sites.is_empty() {
        dialogs::show_error(format!("No new sites to import from {source}."), detail);
        return;
    }
    if !dialogs::confirm(&format!("Import sites from {source}?"), &detail, "Import", false) {
        return;
    }
    {
        let mut store = app.sites.borrow_mut();
        for site in new_sites {
            store.upsert(site);
        }
        store.save();
    }
    super::reload_sidebar();
}

/// Chooses files (or folders) to read.
fn choose(message: &str, start: Option<PathBuf>, folders: bool, multiple: bool) -> Vec<PathBuf> {
    let panel = NSOpenPanel::openPanel(mtm());
    panel.setCanChooseFiles(true);
    panel.setCanChooseDirectories(folders);
    panel.setAllowsMultipleSelection(multiple);
    panel.setShowsHiddenFiles(true);
    panel.setMessage(Some(&ns(message)));
    if let Some(start) = start {
        panel.setDirectoryURL(Some(&file_url(&start)));
    }
    if panel.runModal() != NSModalResponseOK {
        return Vec::new();
    }
    panel.URLs().iter().filter_map(|u| url_path(&u)).collect()
}

/// Cyberduck's bookmarks (found where Cyberduck keeps them, or chosen).
pub fn import_cyberduck() {
    let paths = match import::cyberduck_dir() {
        Some(dir) => vec![dir],
        None => {
            choose("Choose Cyberduck bookmarks (.duck files) or a folder of them", dirs::download_dir(), true, true)
        }
    };
    if paths.is_empty() {
        return;
    }
    add_found("Cyberduck", import::cyberduck(&paths));
}

/// WinSCP's sessions, from a WinSCP.ini (WinSCP > Tools > Export/Backup
/// Configuration on Windows).
pub fn import_winscp() {
    let Some(file) = choose(
        "Choose a WinSCP.ini (in WinSCP: Tools > Export/Backup Configuration)",
        dirs::download_dir(),
        false,
        false,
    )
    .into_iter()
    .next() else {
        return;
    };
    match std::fs::read(&file) {
        // WinSCP may write UTF-8 with a byte order mark, or Latin-1.
        Ok(bytes) => add_found("WinSCP", import::winscp(&crate::fs::view::decode(&bytes).0)),
        Err(e) => dialogs::show_error("The file could not be read.".into(), e.to_string()),
    }
}

/// Sites from a forkz export file.
pub fn import_file() {
    let Some(file) =
        choose("Choose a sites file exported from forkz", dirs::download_dir(), false, false).into_iter().next()
    else {
        return;
    };
    match std::fs::read_to_string(&file).and_then(|text| import::read_export(&text)) {
        Ok(found) => add_found("the file", found),
        Err(e) => dialogs::show_error("The file could not be imported.".into(), e.to_string()),
    }
}

/// Saves the sites to a file (without passwords), for another Mac.
pub fn export_sites() {
    let Some(app) = super::app() else { return };
    let sites = app.sites.borrow().sites.clone();
    if sites.is_empty() {
        dialogs::show_error("There are no sites to export.".into(), String::new());
        return;
    }
    let panel = NSSavePanel::savePanel(mtm());
    panel.setNameFieldStringValue(&ns("forkz Sites.toml"));
    panel.setMessage(Some(&ns(&format!(
        "Export {} site{} (without passwords), to import on another Mac with File > Import Sites from File.",
        sites.len(),
        if sites.len() == 1 { "" } else { "s" }
    ))));
    if let Some(dir) = dirs::document_dir() {
        panel.setDirectoryURL(Some(&file_url(&dir)));
    }
    if panel.runModal() != NSModalResponseOK {
        return;
    }
    let Some(path) = panel.URL().and_then(|u| url_path(&u)) else { return };
    if let Err(e) = std::fs::write(&path, import::export(&sites)) {
        dialogs::show_error("The sites could not be exported.".into(), e.to_string());
    }
}
