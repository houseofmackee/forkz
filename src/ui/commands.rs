//! Every command the app has, used as the tag of menu items and toolbar
//! buttons. One place for titles and shortcuts.

use std::collections::BTreeMap;

use objc2_app_kit::NSEventModifierFlags;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(isize)]
pub enum Command {
    Open = 1,
    NewFolder,
    Rename,
    Duplicate,
    Trash,
    CopyToOther,
    MoveToOther,
    CopyFiles,
    PasteFiles,
    MoveFilesHere,
    CopyPath,
    RevealInFinder,
    OpenInTerminal,
    QuickLook,
    AddToSidebar,
    RemoveFromSidebar,
    ToggleHidden,
    Refresh,
    RefreshAll,
    Back,
    Forward,
    Up,
    Home,
    Desktop,
    Documents,
    Downloads,
    Applications,
    GoToFolder,
    SwitchPane,
    SwapPanes,
    SameFolderInOther,
    Connect,
    Disconnect,
    ImportFileZilla,
    Permissions,
    CompareFiles,
    CalculateSizes,
    CopySha256,
    CopyMd5,
    Filter,
    Settings,
    NewTab,
    CloseTab,
    NextTab,
    PreviousTab,
    OpenInNewTab,
    MultiRename,
    TogglePreview,
    ToggleLeftPane,
    ToggleRightPane,
    OpenWithApp,
    Compress,
    EditTags,
    CommandPalette,
    SelectMatching,
    Search,
    SaveWorkspace,
    ToggleSidebar,
    ZoomIn,
    ZoomOut,
    ActualSize,
    NewFile,
    NewFolderWithSelection,
    DeleteImmediately,
    Share,
    SyncFolders,
    SyncBrowsing,
    ExpandPreview,
    GroupNone,
    GroupKind,
    GroupModified,
    GroupSize,
    ViewAsList,
    ViewAsIcons,
    ViewAsColumns,
    ViewAsGallery,
    StackPanes,
    ViewFile,
    ToggleFilters,
    ComparePanes,
    FlatView,
    FindDuplicates,
    ImportCyberduck,
    ImportWinScp,
    ImportSitesFile,
    ExportSites,
    ReopenClosedTab,
    MoveTabToOther,
    SelectSameType,
    CopySha1,
    CopySha384,
    CopySha512,
    ExportTree,
    DiskUsage,
    ShowDropStack,
    ToggleTerminal,
    TerminalFollows,
}

impl Command {
    /// The view an "as List", "as Icons", or "as Columns" command picks.
    pub fn view_mode(self) -> Option<crate::pane::ViewMode> {
        use crate::pane::ViewMode;
        match self {
            Command::ViewAsList => Some(ViewMode::List),
            Command::ViewAsIcons => Some(ViewMode::Icons),
            Command::ViewAsColumns => Some(ViewMode::Columns),
            Command::ViewAsGallery => Some(ViewMode::Gallery),
            _ => None,
        }
    }

    /// The grouping a Group By command picks.
    pub fn grouping(self) -> Option<crate::pane::GroupBy> {
        use crate::pane::GroupBy;
        match self {
            Command::GroupNone => Some(GroupBy::None),
            Command::GroupKind => Some(GroupBy::Kind),
            Command::GroupModified => Some(GroupBy::Modified),
            Command::GroupSize => Some(GroupBy::Size),
            _ => None,
        }
    }

    pub const ALL: &[Command] = &[
        Command::Open,
        Command::NewFolder,
        Command::Rename,
        Command::Duplicate,
        Command::Trash,
        Command::CopyToOther,
        Command::MoveToOther,
        Command::CopyFiles,
        Command::PasteFiles,
        Command::MoveFilesHere,
        Command::CopyPath,
        Command::RevealInFinder,
        Command::OpenInTerminal,
        Command::QuickLook,
        Command::AddToSidebar,
        Command::RemoveFromSidebar,
        Command::ToggleHidden,
        Command::Refresh,
        Command::RefreshAll,
        Command::Back,
        Command::Forward,
        Command::Up,
        Command::Home,
        Command::Desktop,
        Command::Documents,
        Command::Downloads,
        Command::Applications,
        Command::GoToFolder,
        Command::SwitchPane,
        Command::SwapPanes,
        Command::SameFolderInOther,
        Command::Connect,
        Command::Disconnect,
        Command::ImportFileZilla,
        Command::Permissions,
        Command::CompareFiles,
        Command::CalculateSizes,
        Command::CopySha256,
        Command::CopyMd5,
        Command::Filter,
        Command::Settings,
        Command::NewTab,
        Command::CloseTab,
        Command::NextTab,
        Command::PreviousTab,
        Command::OpenInNewTab,
        Command::MultiRename,
        Command::TogglePreview,
        Command::ToggleLeftPane,
        Command::ToggleRightPane,
        Command::OpenWithApp,
        Command::Compress,
        Command::EditTags,
        Command::CommandPalette,
        Command::SelectMatching,
        Command::Search,
        Command::SaveWorkspace,
        Command::ToggleSidebar,
        Command::ZoomIn,
        Command::ZoomOut,
        Command::ActualSize,
        Command::NewFile,
        Command::NewFolderWithSelection,
        Command::DeleteImmediately,
        Command::Share,
        Command::SyncFolders,
        Command::SyncBrowsing,
        Command::ExpandPreview,
        Command::GroupNone,
        Command::GroupKind,
        Command::GroupModified,
        Command::GroupSize,
        Command::ViewAsList,
        Command::ViewAsIcons,
        Command::ViewAsColumns,
        Command::ViewAsGallery,
        Command::StackPanes,
        Command::ViewFile,
        Command::ToggleFilters,
        Command::ComparePanes,
        Command::FlatView,
        Command::FindDuplicates,
        Command::ImportCyberduck,
        Command::ImportWinScp,
        Command::ImportSitesFile,
        Command::ExportSites,
        Command::ReopenClosedTab,
        Command::MoveTabToOther,
        Command::SelectSameType,
        Command::CopySha1,
        Command::CopySha384,
        Command::CopySha512,
        Command::ExportTree,
        Command::DiskUsage,
        Command::ShowDropStack,
        Command::ToggleTerminal,
        Command::TerminalFollows,
    ];

    pub fn tag(self) -> isize {
        self as isize
    }

    /// A stable name, used in settings ("MultiRename").
    pub fn id(self) -> String {
        format!("{self:?}")
    }

    pub fn from_id(id: &str) -> Option<Command> {
        Command::ALL.iter().copied().find(|c| c.id() == id)
    }

    /// The shortcut in use: the user's (from settings) or the default.
    pub fn effective_shortcut(self, overrides: &BTreeMap<String, String>) -> (String, NSEventModifierFlags) {
        let (key, modifiers) = self.shortcut();
        match overrides.get(&self.id()) {
            Some(spec) if spec.is_empty() => (String::new(), NSEventModifierFlags::Command),
            Some(spec) => parse_spec(spec).unwrap_or_else(|| (key.to_string(), modifiers)),
            None => (key.to_string(), modifiers),
        }
    }

    pub fn from_tag(tag: isize) -> Option<Command> {
        Command::ALL.iter().copied().find(|c| c.tag() == tag)
    }

    pub fn title(self) -> &'static str {
        match self {
            Command::Open => "Open",
            Command::NewFolder => "New Folder",
            Command::Rename => "Rename\u{2026}",
            Command::Duplicate => "Duplicate",
            Command::Trash => "Move to Trash",
            Command::CopyToOther => "Copy to Other Pane",
            Command::MoveToOther => "Move to Other Pane",
            Command::CopyFiles => "Copy",
            Command::PasteFiles => "Paste Items",
            Command::MoveFilesHere => "Move Items Here",
            Command::CopyPath => "Copy Path",
            Command::RevealInFinder => "Show in Finder",
            Command::OpenInTerminal => "Open in Terminal",
            Command::QuickLook => "Quick Look",
            Command::AddToSidebar => "Add to Sidebar",
            Command::RemoveFromSidebar => "Remove from Sidebar",
            Command::ToggleHidden => "Show Hidden Files",
            Command::Refresh => "Refresh",
            Command::RefreshAll => "Refresh All Panes",
            Command::Back => "Back",
            Command::Forward => "Forward",
            Command::Up => "Enclosing Folder",
            Command::Home => "Home",
            Command::Desktop => "Desktop",
            Command::Documents => "Documents",
            Command::Downloads => "Downloads",
            Command::Applications => "Applications",
            Command::GoToFolder => "Go to Folder\u{2026}",
            Command::SwitchPane => "Switch to Other Pane",
            Command::SwapPanes => "Swap Panes",
            Command::SameFolderInOther => "Same Folder in Other Pane",
            Command::Connect => "Connect to Server\u{2026}",
            Command::Disconnect => "Disconnect",
            Command::ImportFileZilla => "Import from FileZilla\u{2026}",
            Command::Permissions => "Permissions\u{2026}",
            Command::CompareFiles => "Compare Files",
            Command::CalculateSizes => "Calculate Folder Sizes",
            Command::CopySha256 => "Copy SHA-256 Checksum",
            Command::CopyMd5 => "Copy MD5 Checksum",
            Command::Filter => "Filter\u{2026}",
            Command::Settings => "Settings\u{2026}",
            Command::NewTab => "New Tab",
            Command::CloseTab => "Close Tab",
            Command::NextTab => "Show Next Tab",
            Command::PreviousTab => "Show Previous Tab",
            Command::OpenInNewTab => "Open in New Tab",
            Command::MultiRename => "Rename Multiple Items\u{2026}",
            Command::TogglePreview => "Show Preview Pane",
            Command::ToggleLeftPane => "Hide Left Pane",
            Command::ToggleRightPane => "Hide Right Pane",
            Command::OpenWithApp => "Open with Default App",
            Command::Compress => "Compress\u{2026}",
            Command::EditTags => "Tags\u{2026}",
            Command::CommandPalette => "Command Palette\u{2026}",
            Command::SelectMatching => "Select Matching\u{2026}",
            Command::Search => "Search\u{2026}",
            Command::SaveWorkspace => "Save Workspace\u{2026}",
            Command::ToggleSidebar => "Hide Sidebar",
            Command::ZoomIn => "Zoom In",
            Command::ZoomOut => "Zoom Out",
            Command::ActualSize => "Actual Size",
            Command::NewFile => "New File\u{2026}",
            Command::NewFolderWithSelection => "New Folder with Selection\u{2026}",
            Command::DeleteImmediately => "Delete Immediately\u{2026}",
            Command::Share => "Share\u{2026}",
            Command::SyncFolders => "Sync Folders\u{2026}",
            Command::SyncBrowsing => "Sync Browsing",
            Command::ExpandPreview => "Expand Preview Pane",
            Command::GroupNone => "No Grouping",
            Command::GroupKind => "Group by Kind",
            Command::GroupModified => "Group by Date Modified",
            Command::GroupSize => "Group by Size",
            Command::ViewAsList => "as List",
            Command::ViewAsIcons => "as Icons",
            Command::ViewAsColumns => "as Columns",
            Command::ViewAsGallery => "as Gallery",
            Command::StackPanes => "Stack Panes",
            Command::ViewFile => "View",
            Command::ToggleFilters => "Use Filename Filters",
            Command::ComparePanes => "Compare Panes",
            Command::FlatView => "Flat View",
            Command::FindDuplicates => "Find Duplicates",
            Command::ImportCyberduck => "Import from Cyberduck\u{2026}",
            Command::ImportWinScp => "Import from WinSCP\u{2026}",
            Command::ImportSitesFile => "Import Sites from File\u{2026}",
            Command::ExportSites => "Export Sites\u{2026}",
            Command::ReopenClosedTab => "Reopen Closed Tab",
            Command::MoveTabToOther => "Move Tab to Other Pane",
            Command::SelectSameType => "Select Same Type",
            Command::CopySha1 => "Copy SHA-1 Checksum",
            Command::CopySha384 => "Copy SHA-384 Checksum",
            Command::CopySha512 => "Copy SHA-512 Checksum",
            Command::ExportTree => "Export Folder Tree\u{2026}",
            Command::DiskUsage => "Disk Usage",
            Command::ShowDropStack => "Show Drop Stack",
            Command::ToggleTerminal => "Show Terminal",
            Command::TerminalFollows => "Terminal Follows Active Pane",
        }
    }

    /// The key equivalent and its modifiers ("" for none).
    pub fn shortcut(self) -> (&'static str, NSEventModifierFlags) {
        let cmd = NSEventModifierFlags::Command;
        let shift_cmd = cmd | NSEventModifierFlags::Shift;
        let opt_cmd = cmd | NSEventModifierFlags::Option;
        match self {
            Command::Open => ("o", cmd),
            Command::NewFolder => ("n", shift_cmd),
            Command::Rename => ("", cmd),
            Command::Duplicate => ("d", cmd),
            Command::Trash => ("\u{8}", cmd),
            Command::CopyToOther => ("c", shift_cmd),
            Command::MoveToOther => ("m", shift_cmd),
            Command::CopyFiles => ("c", cmd),
            Command::PasteFiles => ("v", cmd),
            Command::MoveFilesHere => ("v", opt_cmd),
            Command::CopyPath => ("c", opt_cmd),
            Command::RevealInFinder => ("r", shift_cmd),
            Command::OpenInTerminal => ("t", opt_cmd),
            Command::QuickLook => ("y", cmd),
            Command::AddToSidebar => ("t", cmd | NSEventModifierFlags::Control),
            Command::RemoveFromSidebar => ("", cmd),
            Command::ToggleHidden => (".", shift_cmd),
            Command::Refresh => ("r", cmd),
            Command::RefreshAll => ("r", cmd | NSEventModifierFlags::Control),
            Command::Back => ("[", cmd),
            Command::Forward => ("]", cmd),
            Command::Up => ("\u{f700}", cmd),
            Command::Home => ("h", shift_cmd),
            Command::Desktop => ("d", shift_cmd),
            Command::Documents => ("o", shift_cmd),
            Command::Downloads => ("l", opt_cmd),
            Command::Applications => ("a", shift_cmd),
            Command::GoToFolder => ("g", shift_cmd),
            Command::SwitchPane => ("", cmd),
            Command::SwapPanes => ("u", cmd),
            Command::SameFolderInOther => ("=", opt_cmd),
            Command::Connect => ("k", cmd),
            Command::Disconnect => ("e", cmd),
            Command::ImportFileZilla => ("", cmd),
            Command::Permissions => ("i", cmd),
            // Not Option-Command-D: macOS uses it to hide the Dock.
            Command::CompareFiles => ("c", cmd | NSEventModifierFlags::Control),
            Command::CalculateSizes => ("s", opt_cmd),
            Command::CopySha256 => ("", cmd),
            Command::CopyMd5 => ("", cmd),
            Command::Filter => ("f", cmd),
            Command::Settings => (",", cmd),
            Command::NewTab => ("t", cmd),
            Command::CloseTab => ("w", cmd),
            // Menu shortcuts, as AppKit takes Control-Tab for keyboard
            // focus before views see it (Shift-Command-] and [ also work
            // in the file list).
            Command::NextTab => ("\t", NSEventModifierFlags::Control),
            Command::PreviousTab => ("\t", NSEventModifierFlags::Control | NSEventModifierFlags::Shift),
            Command::OpenInNewTab => ("", cmd),
            Command::MultiRename => ("r", opt_cmd),
            Command::TogglePreview => ("p", shift_cmd),
            Command::ToggleLeftPane => ("1", opt_cmd),
            Command::ToggleRightPane => ("2", opt_cmd),
            Command::OpenWithApp => ("o", opt_cmd),
            Command::Compress => ("", cmd),
            Command::EditTags => ("", cmd),
            Command::CommandPalette => ("p", opt_cmd),
            Command::SelectMatching => ("a", opt_cmd),
            Command::Search => ("f", shift_cmd),
            Command::SaveWorkspace => ("s", shift_cmd | NSEventModifierFlags::Control),
            // The macOS standard for sidebars.
            Command::ToggleSidebar => ("s", cmd | NSEventModifierFlags::Control),
            Command::ZoomIn => ("=", cmd),
            Command::ZoomOut => ("-", cmd),
            Command::ActualSize => ("0", cmd),
            Command::NewFile => ("n", opt_cmd),
            Command::NewFolderWithSelection => ("n", cmd | NSEventModifierFlags::Control),
            Command::DeleteImmediately => ("\u{8}", opt_cmd),
            Command::Share => ("", cmd),
            Command::SyncFolders => ("s", shift_cmd),
            Command::SyncBrowsing => ("b", shift_cmd),
            Command::ExpandPreview => ("p", cmd | NSEventModifierFlags::Control),
            // Control-Command-1 to 9 open workspaces.
            Command::GroupNone | Command::GroupKind | Command::GroupModified | Command::GroupSize => ("", cmd),
            // As in Finder.
            Command::ViewAsList => ("1", cmd),
            Command::ViewAsIcons => ("2", cmd),
            Command::ViewAsColumns => ("3", cmd),
            Command::ViewAsGallery => ("4", cmd),
            Command::StackPanes => ("", cmd),
            Command::ToggleFilters => ("", cmd),
            Command::ComparePanes => ("", cmd),
            Command::FlatView => ("f", opt_cmd),
            Command::FindDuplicates => ("", cmd),
            Command::ImportCyberduck => ("", cmd),
            Command::ImportWinScp => ("", cmd),
            Command::ImportSitesFile => ("", cmd),
            Command::ExportSites => ("", cmd),
            Command::ReopenClosedTab => ("t", shift_cmd),
            Command::MoveTabToOther => ("", cmd),
            Command::SelectSameType => ("", cmd),
            Command::CopySha1 => ("", cmd),
            Command::CopySha384 => ("", cmd),
            Command::CopySha512 => ("", cmd),
            Command::ExportTree => ("", cmd),
            Command::DiskUsage => ("", cmd),
            Command::ShowDropStack => ("", cmd),
            // As in Visual Studio Code.
            Command::ToggleTerminal => ("`", NSEventModifierFlags::Control),
            Command::TerminalFollows => ("", cmd),
            // F3, as in Total Commander (also handled in the lists).
            Command::ViewFile => ("\u{f706}", NSEventModifierFlags::empty()),
        }
    }

    /// An SF Symbol for menus (None: no icon).
    pub fn menu_symbol(self) -> Option<&'static str> {
        use Command::*;
        Some(match self {
            Open => "arrow.up.right.square",
            OpenInNewTab | NewTab => "plus.rectangle.on.rectangle",
            QuickLook => "eye",
            ViewFile => "doc.text.magnifyingglass",
            OpenWithApp => "arrow.up.forward.app",
            CopyToOther => "doc.on.doc",
            MoveToOther => "arrow.right.doc.on.clipboard",
            Rename => "pencil",
            MultiRename => "pencil.and.list.clipboard",
            Duplicate => "plus.square.on.square",
            Trash => "trash",
            DeleteImmediately => "trash.slash",
            NewFolder => "folder.badge.plus",
            NewFolderWithSelection => "folder.badge.plus",
            NewFile => "doc.badge.plus",
            Compress => "archivebox",
            Permissions => "lock",
            EditTags => "tag",
            CompareFiles | ComparePanes => "rectangle.split.2x1",
            CalculateSizes => "scalemass",
            CopySha256 | CopyMd5 | CopySha1 | CopySha384 | CopySha512 => "number",
            ExportTree => "list.bullet.indent",
            CopyFiles => "doc.on.clipboard",
            PasteFiles => "doc.on.clipboard.fill",
            MoveFilesHere => "arrow.down.doc",
            CopyPath => "link",
            RevealInFinder => "macwindow",
            OpenInTerminal => "terminal",
            Share => "square.and.arrow.up",
            AddToSidebar => "sidebar.left",
            ToggleHidden => "eye.slash",
            ToggleFilters => "line.3.horizontal.decrease",
            Refresh => "arrow.clockwise",
            RefreshAll => "arrow.clockwise",
            Back => "chevron.left",
            Forward => "chevron.right",
            Up => "arrow.up",
            Home => "house",
            Desktop => "menubar.dock.rectangle",
            Documents => "doc",
            Downloads => "arrow.down.circle",
            Applications => "square.grid.2x2",
            GoToFolder => "arrow.right.circle",
            SwapPanes => "arrow.left.arrow.right",
            SameFolderInOther => "rectangle.split.2x1",
            Connect => "network",
            Disconnect => "eject",
            ImportFileZilla | ImportCyberduck | ImportWinScp | ImportSitesFile => "square.and.arrow.down",
            ExportSites => "square.and.arrow.up",
            Filter => "line.3.horizontal.decrease.circle",
            Search => "magnifyingglass",
            SelectMatching => "checklist",
            SelectSameType => "checklist.checked",
            Settings => "gearshape",
            CloseTab => "xmark.square",
            ReopenClosedTab => "arrow.uturn.backward",
            MoveTabToOther => "arrow.right.square",
            CommandPalette => "command",
            SyncFolders => "arrow.triangle.2.circlepath",
            FindDuplicates => "doc.on.doc.fill",
            DiskUsage => "chart.pie",
            ShowDropStack => "tray.and.arrow.down",
            ToggleTerminal => "apple.terminal",
            FlatView => "list.bullet.rectangle",
            ViewAsList => "list.bullet",
            ViewAsIcons => "square.grid.2x2",
            ViewAsColumns => "rectangle.split.3x1",
            ViewAsGallery => "square.grid.3x1.below.line.grid.1x2",
            TogglePreview => "sidebar.right",
            ToggleSidebar => "sidebar.left",
            ZoomIn => "plus.magnifyingglass",
            ZoomOut => "minus.magnifyingglass",
            SaveWorkspace => "rectangle.stack",
            _ => return None,
        })
    }

    /// An SF Symbol for the toolbar.
    pub fn symbol(self) -> &'static str {
        match self {
            Command::Back => "chevron.left",
            Command::Forward => "chevron.right",
            Command::CopyToOther => "doc.on.doc",
            Command::MoveToOther => "arrow.right.doc.on.clipboard",
            Command::Trash => "trash",
            Command::NewFolder => "folder.badge.plus",
            Command::ToggleHidden => "eye",
            Command::OpenInTerminal => "terminal",
            Command::SwapPanes => "arrow.left.arrow.right",
            Command::QuickLook => "eye.circle",
            Command::Refresh | Command::RefreshAll => "arrow.clockwise",
            Command::Connect => "network",
            Command::ToggleLeftPane => "rectangle.lefthalf.inset.filled",
            Command::ToggleRightPane => "rectangle.righthalf.inset.filled",
            Command::TogglePreview => "sidebar.right",
            Command::ToggleSidebar => "sidebar.left",
            Command::ShowDropStack => "tray.and.arrow.down",
            Command::ToggleTerminal => "apple.terminal",
            _ => "questionmark",
        }
    }

    /// Short labels for the toolbar.
    pub fn toolbar_label(self) -> &'static str {
        match self {
            Command::CopyToOther => "Copy to",
            Command::ShowDropStack => "Drop Stack",
            Command::ToggleTerminal => "Terminal",
            Command::MoveToOther => "Move to",
            Command::Trash => "Delete",
            Command::ToggleHidden => "Hidden Files",
            Command::OpenInTerminal => "Terminal",
            Command::SwapPanes => "Swap",
            Command::Connect => "Connect",
            Command::ToggleLeftPane => "Left Pane",
            Command::ToggleRightPane => "Right Pane",
            Command::TogglePreview => "Preview",
            Command::ToggleSidebar => "Sidebar",
            Command::RefreshAll => "Refresh",
            other => other.title(),
        }
    }
}

/// Keys with names in shortcut specs: (name, key equivalent, symbol).
const NAMED_KEYS: &[(&str, &str, &str)] = &[
    ("delete", "\u{8}", "\u{232b}"),
    ("forward-delete", "\u{f728}", "\u{2326}"),
    ("return", "\r", "\u{21a9}"),
    ("tab", "\t", "\u{21e5}"),
    ("space", " ", "Space"),
    ("escape", "\u{1b}", "\u{238b}"),
    ("up", "\u{f700}", "\u{2191}"),
    ("down", "\u{f701}", "\u{2193}"),
    ("left", "\u{f702}", "\u{2190}"),
    ("right", "\u{f703}", "\u{2192}"),
    ("home", "\u{f729}", "\u{2196}"),
    ("end", "\u{f72b}", "\u{2198}"),
    ("page-up", "\u{f72c}", "\u{21de}"),
    ("page-down", "\u{f72d}", "\u{21df}"),
];

const MODIFIER_NAMES: [(&str, NSEventModifierFlags, &str); 4] = [
    ("ctrl", NSEventModifierFlags::Control, "\u{2303}"),
    ("opt", NSEventModifierFlags::Option, "\u{2325}"),
    ("shift", NSEventModifierFlags::Shift, "\u{21e7}"),
    ("cmd", NSEventModifierFlags::Command, "\u{2318}"),
];

/// F1 to F20 are U+F704 to U+F717.
fn function_key(key: &str) -> Option<u32> {
    let c = key.chars().next()?;
    (key.chars().count() == 1 && ('\u{f704}'..='\u{f717}').contains(&c)).then(|| c as u32 - 0xf704 + 1)
}

pub fn is_function_key(key: &str) -> bool {
    function_key(key).is_some()
}

/// Only the modifiers that matter for shortcuts.
pub fn shortcut_modifiers(flags: NSEventModifierFlags) -> NSEventModifierFlags {
    MODIFIER_NAMES
        .iter()
        .fold(NSEventModifierFlags(0), |all, (_, flag, _)| if flags.contains(*flag) { all | *flag } else { all })
}

/// "cmd+shift+r", "ctrl+tab", "f5" (as settings keep them).
pub fn spec(key: &str, modifiers: NSEventModifierFlags) -> String {
    let mut parts: Vec<String> = MODIFIER_NAMES
        .iter()
        .filter(|(_, flag, _)| modifiers.contains(*flag))
        .map(|(name, _, _)| name.to_string())
        .collect();
    let name = if let Some((name, _, _)) = NAMED_KEYS.iter().find(|(_, k, _)| *k == key) {
        name.to_string()
    } else if let Some(n) = function_key(key) {
        format!("f{n}")
    } else {
        key.to_lowercase()
    };
    parts.push(name);
    parts.join("+")
}

pub fn parse_spec(text: &str) -> Option<(String, NSEventModifierFlags)> {
    let mut modifiers = NSEventModifierFlags(0);
    // "cmd++" is Command-plus: the key is what follows the last
    // separating "+".
    let text = text.trim();
    let (prefix, key) = match text.strip_suffix("++") {
        Some(prefix) => (prefix, "+"),
        None => match text.rsplit_once('+') {
            Some((prefix, key)) => (prefix, key),
            None => ("", text),
        },
    };
    for part in prefix.split('+').filter(|p| !p.is_empty()) {
        let (_, flag, _) = MODIFIER_NAMES.iter().find(|(name, _, _)| *name == part.to_lowercase())?;
        modifiers |= *flag;
    }
    let key = key.to_lowercase();
    let key = if let Some((_, k, _)) = NAMED_KEYS.iter().find(|(name, _, _)| *name == key) {
        k.to_string()
    } else if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=20).contains(n)) {
        char::from_u32(0xf704 + n - 1)?.to_string()
    } else if key.chars().count() == 1 {
        key
    } else {
        return None;
    };
    Some((key, modifiers))
}

/// "\u{21e7}\u{2318}R" as menus show it ("" for none).
pub fn display(key: &str, modifiers: NSEventModifierFlags) -> String {
    if key.is_empty() {
        return String::new();
    }
    let mut text: String =
        MODIFIER_NAMES.iter().filter(|(_, flag, _)| modifiers.contains(*flag)).map(|(_, _, symbol)| *symbol).collect();
    if let Some((_, _, symbol)) = NAMED_KEYS.iter().find(|(_, k, _)| *k == key) {
        text.push_str(symbol);
    } else if let Some(n) = function_key(key) {
        text.push_str(&format!("F{n}"));
    } else {
        text.push_str(&key.to_uppercase());
    }
    text
}

/// Shortcuts the app menu and Edit menu use (not for commands).
pub const RESERVED: &[(&str, NSEventModifierFlags)] = &[
    ("q", NSEventModifierFlags::Command),
    ("h", NSEventModifierFlags::Command),
    ("x", NSEventModifierFlags::Command),
    ("a", NSEventModifierFlags::Command),
    ("m", NSEventModifierFlags::Command),
];

/// Keys the file list handles itself (see `FileTable::keyDown`), for
/// conflict checks. A menu shortcut on one of them takes it over.
pub const LIST_KEYS: &[(&str, Command)] = &[
    ("\u{f705}", Command::Rename),
    ("\u{f708}", Command::CopyToOther),
    ("\u{f709}", Command::MoveToOther),
    ("\u{f70a}", Command::NewFolder),
    ("\u{f70b}", Command::Trash),
];

/// The command that already uses this shortcut, if any (besides `except`):
/// in the menus, or one of the file list's function keys.
pub fn command_using(
    key: &str,
    modifiers: NSEventModifierFlags,
    overrides: &BTreeMap<String, String>,
    except: Command,
) -> Option<Command> {
    if key.is_empty() {
        return None;
    }
    let in_menu = Command::ALL.iter().copied().filter(|c| *c != except).find(|c| {
        let (k, m) = c.effective_shortcut(overrides);
        k == key && shortcut_modifiers(m) == shortcut_modifiers(modifiers)
    });
    in_menu.or_else(|| {
        let plain = shortcut_modifiers(modifiers).0 == 0;
        LIST_KEYS.iter().find(|(k, c)| plain && *k == key && *c != except).map(|(_, c)| *c)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_round_trip_and_display() {
        let cmd = NSEventModifierFlags::Command;
        let shift_cmd = cmd | NSEventModifierFlags::Shift;
        assert_eq!(spec("r", shift_cmd), "shift+cmd+r");
        assert_eq!(parse_spec("shift+cmd+r"), Some(("r".to_string(), shift_cmd)));
        assert_eq!(parse_spec("CMD+Shift+R"), Some(("r".to_string(), shift_cmd)));
        assert_eq!(parse_spec("cmd++"), Some(("+".to_string(), cmd)));
        assert_eq!(spec("\u{8}", cmd), "cmd+delete");
        assert_eq!(parse_spec("cmd+delete"), Some(("\u{8}".to_string(), cmd)));
        assert_eq!(spec("\u{f708}", NSEventModifierFlags(0)), "f5");
        assert_eq!(parse_spec("f5"), Some(("\u{f708}".to_string(), NSEventModifierFlags(0))));
        assert_eq!(parse_spec("cmd+banana"), None);
        assert_eq!(parse_spec("hyper+r"), None);
        assert_eq!(display("r", shift_cmd), "\u{21e7}\u{2318}R");
        assert_eq!(display("\t", NSEventModifierFlags::Control), "\u{2303}\u{21e5}");
        assert_eq!(display("\u{f708}", NSEventModifierFlags(0)), "F5");
        assert_eq!(display("", cmd), "");
        for &command in Command::ALL {
            let (key, modifiers) = command.shortcut();
            if !key.is_empty() {
                assert_eq!(parse_spec(&spec(key, modifiers)), Some((key.to_string(), modifiers)), "{command:?}");
            }
        }
    }

    #[test]
    fn overrides_and_conflicts() {
        let mut overrides = BTreeMap::new();
        let cmd = NSEventModifierFlags::Command;
        assert_eq!(Command::Refresh.effective_shortcut(&overrides), ("r".to_string(), cmd));
        assert_eq!(command_using("r", cmd, &overrides, Command::Open), Some(Command::Refresh));
        overrides.insert("Refresh".to_string(), "f5".to_string());
        overrides.insert("Duplicate".to_string(), String::new());
        assert_eq!(Command::Refresh.effective_shortcut(&overrides).0, "\u{f708}");
        assert_eq!(Command::Duplicate.effective_shortcut(&overrides).0, "");
        assert_eq!(command_using("r", cmd, &overrides, Command::Open), None);
        assert_eq!(command_using("d", cmd, &overrides, Command::Open), None);
        assert_eq!(Command::from_id("MultiRename"), Some(Command::MultiRename));
        // F5 copies in the file list.
        let f5 = "\u{f708}";
        assert_eq!(
            command_using(f5, NSEventModifierFlags(0), &BTreeMap::new(), Command::Refresh),
            Some(Command::CopyToOther)
        );
        assert_eq!(
            command_using(f5, NSEventModifierFlags(0), &overrides, Command::CopyToOther),
            Some(Command::Refresh)
        );
    }

    #[test]
    fn tags_round_trip_and_shortcuts_unique() {
        let mut seen = std::collections::HashSet::new();
        for &command in Command::ALL {
            assert_eq!(Command::from_tag(command.tag()), Some(command));
            let (key, modifiers) = command.shortcut();
            if !key.is_empty() {
                assert!(seen.insert((key, modifiers.0)), "{command:?} reuses a shortcut");
            }
        }
    }
}
