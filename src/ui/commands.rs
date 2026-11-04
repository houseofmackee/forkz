//! Every command the app has, used as the tag of menu items and toolbar
//! buttons. One place for titles and shortcuts.

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
}

impl Command {
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
    ];

    pub fn tag(self) -> isize {
        self as isize
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
            Command::SameFolderInOther => ("=", cmd),
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
        }
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
            Command::Refresh => "arrow.clockwise",
            Command::Connect => "network",
            _ => "questionmark",
        }
    }

    /// Short labels for the toolbar.
    pub fn toolbar_label(self) -> &'static str {
        match self {
            Command::CopyToOther => "Copy to",
            Command::MoveToOther => "Move to",
            Command::Trash => "Delete",
            Command::ToggleHidden => "Hidden Files",
            Command::OpenInTerminal => "Terminal",
            Command::SwapPanes => "Swap",
            Command::Connect => "Connect",
            other => other.title(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
