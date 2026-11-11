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
    /// Toolbar items added since the toolbar was first saved, so they are
    /// added to saved toolbars once (1: pane and preview buttons).
    pub toolbar_version: u32,
    /// Saved sets of tabs for both panes.
    pub workspaces: Vec<Workspace>,
    /// Hidden with Hide Sidebar, and its width when shown.
    pub sidebar_hidden: bool,
    pub sidebar_width: f64,
    /// View > Stack Panes: one above the other instead of side by side.
    pub stacked_panes: bool,
    /// Settings > Filters, and View > Use Filename Filters.
    pub filters: Vec<crate::fs::filters::NameFilter>,
    pub filters_on: bool,
    /// The Sync window's options, as last used.
    pub sync: crate::fs::sync::Options,
    /// Saved syncs, in the sidebar.
    pub synclets: Vec<Synclet>,
    /// Saved searches, in the sidebar.
    pub smart_folders: Vec<SmartFolder>,
    /// Folders the panes showed, the latest first (Go > Recent Folders).
    pub recent_folders: Vec<RecentFolder>,
    /// Your own commands (Commands > Tools, the toolbar, the palette).
    pub tools: Vec<CustomTool>,
    /// The sidebar's items in the order dragged into, by section title.
    pub sidebar_order: std::collections::BTreeMap<String, Vec<String>>,
    /// The drop stack's files (where they are).
    pub drop_stack: Vec<PathBuf>,
    /// The terminal pane goes where the active pane goes.
    pub terminal_follows: bool,
}

/// A shell command run on the selection (Settings > Tools).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomTool {
    pub name: String,
    /// Run by zsh in the pane's folder; {selection}, {file}, {names},
    /// {folder}, {other-folder}, and {other-selection} are filled in.
    pub command: String,
    /// An SF Symbol for the toolbar.
    #[serde(default = "tool_symbol")]
    pub symbol: String,
    /// Show what the command prints when it is done.
    #[serde(default)]
    pub show_output: bool,
}

fn tool_symbol() -> String {
    "hammer".into()
}

/// Tools to start with.
pub fn example_tools() -> Vec<CustomTool> {
    let tool = |name: &str, command: &str, symbol: &str, show_output: bool| CustomTool {
        name: name.into(),
        command: command.into(),
        symbol: symbol.into(),
        show_output,
    };
    vec![
        tool("Make Executable", "chmod +x {selection}", "terminal", false),
        tool("Count Lines", "wc -l {selection}", "text.alignleft", true),
        tool("Disk Usage", "du -sh {selection}", "chart.pie", true),
    ]
}

/// A folder a pane showed: on the Mac, or on a saved site.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecentFolder {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    pub path: PathBuf,
}

/// How many recent folders are kept.
pub const RECENT_FOLDERS: usize = 15;

/// A saved search on the Mac: clicking it in the sidebar searches again.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmartFolder {
    pub name: String,
    /// Where it searches (and everything in it).
    pub folder: PathBuf,
    #[serde(default)]
    pub query: crate::fs::search::Query,
    #[serde(default = "yes")]
    pub spotlight: bool,
}

fn yes() -> bool {
    true
}

/// A saved sync: two folders and how to sync them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Synclet {
    pub name: String,
    pub left: WorkspaceTab,
    pub right: WorkspaceTab,
    #[serde(default)]
    pub options: crate::fs::sync::Options,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workspace {
    pub name: String,
    pub left: WorkspacePane,
    pub right: WorkspacePane,
    /// The pane that was active (0 or 1).
    #[serde(default)]
    pub active: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WorkspacePane {
    pub tabs: Vec<WorkspaceTab>,
    pub active_tab: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

/// A folder on the Mac, or on a saved site.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkspaceTab {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    pub path: PathBuf,
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

fn is_list_view(view: &crate::pane::ViewMode) -> bool {
    *view == crate::pane::ViewMode::List
}

fn is_default_group(group: &crate::pane::GroupBy) -> bool {
    *group == crate::pane::GroupBy::None
}

pub const THEMES: &[(&str, &str)] = &[("system", "Same as macOS"), ("light", "Light"), ("dark", "Dark")];
pub const ROW_SPACINGS: &[(&str, &str, f64)] =
    &[("compact", "Compact", 18.0), ("normal", "Normal", 22.0), ("roomy", "Roomy", 28.0)];
pub const TEXT_SIZES: &[f64] = &[10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 18.0, 20.0, 22.0, 24.0];

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
    /// Moving an app to the Trash offers to remove its leftover files.
    pub app_leftovers: bool,
    /// Sounds for moving to the Trash and deleting.
    pub sound_effects: bool,
    /// A sound when a long transfer finishes.
    pub transfer_sounds: bool,
    /// Dragging between panes moves items when both are on this Mac or on
    /// the same server (any disk), instead of following Finder (move on
    /// the same disk, copy across disks). Option still copies.
    pub drag_moves: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        GeneralSettings {
            start_on_parent: false,
            terminal: "terminal".into(),
            compare_tool: "forkz".into(),
            app_leftovers: true,
            sound_effects: true,
            transfer_sounds: true,
            drag_moves: false,
        }
    }
}

/// Terminal apps: (key, name, bundle identifier).
pub const TERMINALS: &[(&str, &str, &str)] = &[
    ("terminal", "Terminal", "com.apple.Terminal"),
    ("iterm", "iTerm", "com.googlecode.iterm2"),
    ("warp", "Warp", "dev.warp.Warp-Stable"),
    ("ghostty", "Ghostty", "com.mitchellh.ghostty"),
    ("kitty", "Kitty", "net.kovidgoyal.kitty"),
    ("hyper", "Hyper", "co.zeit.hyper"),
];

/// Compare tools: (key, name, bundle identifier of the app it needs).
pub const COMPARE_TOOLS: &[(&str, &str, &str)] = &[
    // The built-in diff (no app needed).
    ("forkz", "forkz (built in)", ""),
    ("filemerge", "FileMerge", "com.apple.FileMerge"),
    ("kaleidoscope", "Kaleidoscope", "app.kaleidoscope.v4,com.kaleidoscopeapp.v3,com.blackpixel.kaleidoscope"),
    ("beyondcompare", "Beyond Compare", "com.ScooterSoftware.BeyondCompare"),
    ("bbedit", "BBEdit", "com.barebones.bbedit"),
    ("vscode", "Visual Studio Code", "com.microsoft.VSCode"),
    ("araxis", "Araxis Merge", "com.araxis.merge"),
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
    ("tags", "Tags"),
    ("permissions", "Permissions"),
    ("owner", "Owner"),
    ("group", "Group"),
    ("git", "Git"),
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
    /// Speed limits in KB a second for all uploads and all downloads
    /// together (0: none).
    pub upload_limit_kbps: u64,
    pub download_limit_kbps: u64,
    /// When a transfer hits an error: "ask", "skip", or "retry" (three
    /// tries, then skip).
    pub on_error: String,
}

impl Default for TransferSettings {
    fn default() -> Self {
        TransferSettings {
            preserve_timestamps: true,
            download_conflict: None,
            upload_conflict: None,
            max_transfers: 2,
            ask_before_upload_on_save: false,
            upload_limit_kbps: 0,
            download_limit_kbps: 0,
            on_error: "ask".into(),
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
    /// View > Group By.
    #[serde(skip_serializing_if = "crate::settings::is_default_group")]
    pub group_by: crate::pane::GroupBy,
    /// View > as List, as Icons, or as Columns.
    #[serde(skip_serializing_if = "crate::settings::is_list_view")]
    pub view: crate::pane::ViewMode,
    /// Every tab's folder (on the Mac), in order; empty for one tab.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<PathBuf>,
    /// Which of `tabs` was shown.
    pub active_tab: usize,
    /// Hidden with Hide Left Pane or Hide Right Pane.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

impl Default for PaneSettings {
    fn default() -> Self {
        PaneSettings {
            location: dirs::home_dir().unwrap_or_else(|| PathBuf::from("/")),
            sort: Sort::default(),
            group_by: crate::pane::GroupBy::None,
            view: crate::pane::ViewMode::List,
            tabs: Vec::new(),
            active_tab: 0,
            hidden: false,
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
            toolbar_version: 0,
            workspaces: Vec::new(),
            sidebar_hidden: false,
            stacked_panes: false,
            filters: crate::fs::filters::defaults(),
            filters_on: true,
            sidebar_width: 180.0,
            sync: crate::fs::sync::Options::default(),
            synclets: Vec::new(),
            smart_folders: Vec::new(),
            recent_folders: Vec::new(),
            tools: example_tools(),
            sidebar_order: std::collections::BTreeMap::new(),
            drop_stack: Vec::new(),
            terminal_follows: true,
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
