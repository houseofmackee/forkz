//! Settings saved between launches, in
//! `~/Library/Application Support/forkz/settings.toml`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::fs::Sort;
use crate::fs::ops::Resolution;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub show_hidden: bool,
    pub left: PaneSettings,
    pub right: PaneSettings,
    /// Folders added to the sidebar, in order.
    pub favorites: Vec<PathBuf>,
    pub transfers: TransferSettings,
    pub sftp: SftpSettings,
    pub columns: ColumnSettings,
    pub general: GeneralSettings,
    /// Server folders added to the sidebar.
    pub remote_favorites: Vec<RemoteFavorite>,
    /// Saved multi-rename rules.
    pub rename_presets: Vec<crate::rename::Preset>,
    pub appearance: AppearanceSettings,
    /// Keyboard shortcuts changed from the defaults: command id to a
    /// shortcut ("cmd+shift+r"), or "" for none.
    pub shortcuts: std::collections::BTreeMap<String, String>,
    /// The preview pane on the right, and its tab.
    pub preview: PreviewSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AppearanceSettings {
    /// "system", "light", or "dark".
    pub theme: String,
    /// The file list's text size in points.
    pub text_size: f64,
    /// "compact", "normal", or "roomy".
    pub row_spacing: String,
    pub alternating_rows: bool,
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        AppearanceSettings {
            theme: "system".into(),
            text_size: 13.0,
            row_spacing: "normal".into(),
            alternating_rows: true,
        }
    }
}

pub const THEMES: &[(&str, &str)] = &[("system", "Same as macOS"), ("light", "Light"), ("dark", "Dark")];
pub const ROW_SPACINGS: &[(&str, &str, f64)] =
    &[("compact", "Compact", 18.0), ("normal", "Normal", 22.0), ("roomy", "Roomy", 28.0)];
pub const TEXT_SIZES: &[f64] = &[11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PreviewSettings {
    pub visible: bool,
    /// "info", "preview", "activity", or "log".
    pub tab: String,
    pub width: f64,
}

impl Default for PreviewSettings {
    fn default() -> Self {
        PreviewSettings { visible: false, tab: "info".into(), width: 280.0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct GeneralSettings {
    /// Entering a folder selects ".." instead of the first item.
    pub start_on_parent: bool,
    /// The terminal app for Open in Terminal (a key of `TERMINALS`).
    pub terminal: String,
    /// The tool for Compare Files (a key of `COMPARE_TOOLS`).
    pub compare_tool: String,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        GeneralSettings { start_on_parent: false, terminal: "terminal".into(), compare_tool: "filemerge".into() }
    }
}

/// Terminal apps: (key, name, bundle identifier).
pub const TERMINALS: &[(&str, &str, &str)] = &[
    ("terminal", "Terminal", "com.apple.Terminal"),
    ("iterm", "iTerm", "com.googlecode.iterm2"),
    ("warp", "Warp", "dev.warp.Warp-Stable"),
    ("ghostty", "Ghostty", "com.mitchellh.ghostty"),
    ("kitty", "Kitty", "net.kovidgoyal.kitty"),
];

/// Compare tools: (key, name, bundle identifier of the app it needs).
pub const COMPARE_TOOLS: &[(&str, &str, &str)] = &[
    ("filemerge", "FileMerge", "com.apple.FileMerge"),
    ("kaleidoscope", "Kaleidoscope", "app.kaleidoscope.v4,com.kaleidoscopeapp.v3,com.blackpixel.kaleidoscope"),
    ("beyondcompare", "Beyond Compare", "com.ScooterSoftware.BeyondCompare"),
    ("bbedit", "BBEdit", "com.barebones.bbedit"),
    ("vscode", "Visual Studio Code", "com.microsoft.VSCode"),
];

/// A folder on a saved site, shown in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemoteFavorite {
    pub site_id: String,
    pub path: PathBuf,
}

/// The optional list columns, in order: (identifier, title).
pub const COLUMNS: &[(&str, &str)] = &[
    ("modified", "Date Modified"),
    ("size", "Size"),
    ("kind", "Kind"),
    ("permissions", "Permissions"),
    ("owner", "Owner"),
    ("group", "Group"),
];

/// Which optional columns show (Name always does), for folders on the Mac
/// and on servers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ColumnSettings {
    pub local: Vec<String>,
    pub remote: Vec<String>,
}

impl Default for ColumnSettings {
    fn default() -> Self {
        let list = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect();
        ColumnSettings {
            local: list(&["modified", "size", "kind"]),
            remote: list(&["modified", "size", "permissions"]),
        }
    }
}

impl ColumnSettings {
    pub fn visible(&self, local: bool) -> &[String] {
        if local { &self.local } else { &self.remote }
    }

    /// Shows or hides a column, keeping the standard order.
    pub fn toggle(&mut self, local: bool, id: &str) {
        let list = if local { &mut self.local } else { &mut self.remote };
        if list.iter().any(|c| c == id) {
            list.retain(|c| c != id);
        } else {
            list.push(id.to_string());
            list.sort_by_key(|c| COLUMNS.iter().position(|(known, _)| known == c).unwrap_or(usize::MAX));
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct TransferSettings {
    /// Keep modification dates on uploads and downloads.
    pub preserve_timestamps: bool,
    /// What to do when a downloaded file exists (ask if None).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_conflict: Option<Resolution>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_conflict: Option<Resolution>,
    /// Uploads and downloads that run at once; more wait their turn.
    pub max_transfers: usize,
    /// Ask before uploading an opened server file that was saved (instead
    /// of uploading it right away).
    pub ask_before_upload_on_save: bool,
}

impl Default for TransferSettings {
    fn default() -> Self {
        TransferSettings {
            preserve_timestamps: true,
            download_conflict: None,
            upload_conflict: None,
            max_transfers: 2,
            ask_before_upload_on_save: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SftpSettings {
    pub timeout_seconds: u64,
    /// More private keys to try when connecting.
    pub key_files: Vec<PathBuf>,
}

impl Default for SftpSettings {
    fn default() -> Self {
        SftpSettings { timeout_seconds: 20, key_files: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneSettings {
    pub location: PathBuf,
    pub sort: Sort,
    /// Every tab's folder (on the Mac), in order; empty for one tab.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<PathBuf>,
    /// Which of `tabs` was shown.
    pub active_tab: usize,
}

impl Default for PaneSettings {
    fn default() -> Self {
        PaneSettings {
            location: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            sort: Sort::default(),
            tabs: Vec::new(),
            active_tab: 0,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Settings {
            show_hidden: false,
            left: PaneSettings::default(),
            right: PaneSettings { location: dirs::download_dir().unwrap_or(home), ..PaneSettings::default() },
            favorites: Vec::new(),
            transfers: TransferSettings::default(),
            sftp: SftpSettings::default(),
            columns: ColumnSettings::default(),
            general: GeneralSettings::default(),
            remote_favorites: Vec::new(),
            rename_presets: Vec::new(),
            appearance: AppearanceSettings::default(),
            shortcuts: Default::default(),
            preview: PreviewSettings::default(),
        }
    }
}

impl Settings {
    pub fn path() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("forkz").join("settings.toml"))
    }

    /// Loads the settings, or the defaults if there are none or they cannot
    /// be read.
    pub fn load() -> Settings {
        let Some(path) = Settings::path() else { return Settings::default() };
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("ignoring unreadable settings {}: {e}", path.display());
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = Settings::path() else { return };
        let result = (|| -> anyhow::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let temporary = path.with_extension("toml.tmp");
            std::fs::write(&temporary, toml::to_string_pretty(self)?)?;
            std::fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if let Err(e) = result {
            tracing::warn!("could not save settings: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_choices() {
        let mut columns = ColumnSettings::default();
        assert_eq!(columns.visible(false), ["modified", "size", "permissions"]);
        columns.toggle(false, "owner");
        columns.toggle(false, "kind");
        assert_eq!(columns.visible(false), ["modified", "size", "kind", "permissions", "owner"]);
        columns.toggle(false, "size");
        assert_eq!(columns.visible(false), ["modified", "kind", "permissions", "owner"]);
        assert_eq!(columns.visible(true), ["modified", "size", "kind"], "the Mac's columns are separate");
    }

    #[test]
    fn round_trip_and_partial() {
        let settings = Settings { favorites: vec!["/tmp".into()], ..Settings::default() };
        let text = toml::to_string_pretty(&settings).unwrap();
        let back: Settings = toml::from_str(&text).unwrap();
        assert_eq!(back.favorites, settings.favorites);
        // Missing keys fall back to defaults.
        let partial: Settings = toml::from_str("show_hidden = true").unwrap();
        assert!(partial.show_hidden);
        assert_eq!(partial.left.sort, Sort::default());
    }
}
