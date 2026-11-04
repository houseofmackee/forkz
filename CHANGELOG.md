# Changelog

All notable changes to forkz. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). The section for a version is what
the GitHub release and Sparkle's update window show.

## [Unreleased]

## [0.2.0] - 2026-11-04

### Added

- A Settings window (Command-comma) with General, Transfers, and SFTP: hidden files, starting on "..", the terminal and compare apps, automatic update checks, simultaneous transfers, keeping dates, what to do when a file exists on download or upload, the connection timeout, extra key files, and the trusted host keys file.
- A filter field above each list (Command-F): shows only names containing what you type, on the Mac and on servers. Escape clears it.
- Calculate Folder Sizes (Option-Command-S): sizes of the selected folders, or all of them, in the background, locally and on servers; folders then sort by size.
- Copy SHA-256 Checksum and Copy MD5 Checksum, for files on the Mac or on a server.
- Compare Files (Control-Command-C): two files in one pane, or one in each, in FileMerge, Kaleidoscope, Beyond Compare, BBEdit, or Visual Studio Code. Server files are downloaded first.
- Open in Terminal can use iTerm, Warp, Ghostty, or Kitty instead of Terminal.
- Server folders can be added to the sidebar; clicking one connects and opens it.
- An option to select ".." when opening a folder.

## [0.1.0] - 2026-11-04

### Added

- Updates through Sparkle: forkz > Check for Updates…, and an option to check automatically. Releases are built, signed, notarized, and published by a GitHub workflow when a version tag is pushed.
- SFTP: connect with a password, key file, or the SSH agent; host key checks with fingerprints; saved sites with passwords in the Keychain; uploads, downloads, and server-to-server copies with progress; rename, new folder, and delete on servers; open remote files; Open in Terminal over SSH; automatic reconnects.
- Import from FileZilla: SFTP sites (with folders, passwords, key files, and default folders), trusted host keys, and matching settings.
- Context menus, the File and Edit menus, and toolbar tooltips match where the pane is: on a server, Mac-only items (Quick Look, Show in Finder, Add to Sidebar, Copy) are gone or disabled, and items are worded for servers ("Delete…", "Download to Other Pane", "Copy sftp:// Link", "Open SSH Session in Terminal").
- A ".." row at the top of every folder (local and remote) goes up a folder when opened (double-click, Command-Down, or Command-O) and selects the folder you came from. Items dropped on it go to the enclosing folder; commands never act on it.
- `~/.ssh/config` support: host aliases with HostName, User, Port, IdentityFile, and IdentitiesOnly (with wildcards and Include). The Connect form lists configured hosts; hosts behind ProxyJump or ProxyCommand get a clear message.
- ProxyJump and ProxyCommand from `~/.ssh/config`: ProxyCommand runs the command and connects over it; ProxyJump goes through the system’s `ssh -W`, so jump hosts use your usual keys, agent, and known hosts. The final server’s host key is still checked by forkz, and proxy errors are shown.
- Owner and Group columns: local names from macOS, server names from the server’s user and group lists (or their numbers when the server allows only SFTP).
- Choose the columns: right-click the column titles, or View > Columns. The Mac and servers each keep their own choice (by default Kind on the Mac, Permissions on servers).
- Permissions: a Permissions column on servers, and a Permissions dialog (Command-I) with owner, group, and others checkboxes, an octal field, and applying to folder contents (everything, folders only, or files only). Works on the Mac too.
- Simultaneous transfers (Commands menu, default 2): uploads and downloads beyond the limit wait their turn and can be stopped while waiting. Imported from FileZilla’s "Number of Transfers".
- `forkz FOLDER` and `forkz sftp://user@host:port/path` open a folder or server at launch.

- Dual-pane window with a sidebar (favorites, standard places, volumes) and a customizable toolbar.
- File list with name, date modified, size, and kind columns; sorting; a path bar and status line.
- Copy, move, duplicate, trash, rename, and new folder, run as jobs with progress and Stop.
- Conflict dialog: Replace, Replace if Older, Keep Both, Skip, Merge, Stop, and "Apply to all".
- Drag and drop between panes and with Finder; Copy and Paste of files.
- Quick Look, hidden files toggle, live folder updates, and keyboard shortcuts for every command.

[Unreleased]: https://github.com/houseofmackee/forkz/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/houseofmackee/forkz/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/houseofmackee/forkz/releases/tag/v0.1.0
