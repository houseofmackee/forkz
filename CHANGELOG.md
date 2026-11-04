# Changelog

All notable changes to forkz. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). The section for a version is what
the GitHub release and Sparkle's update window show.

## [Unreleased]

## [0.3.0] - 2026-11-04

### Added

- Tabs in each pane: New Tab (Command-T), Open in New Tab for folders, Close Tab (Command-W, or the window with one tab), and Show Next or Previous Tab (Control-Tab and Control-Shift-Tab, or Shift-Command-] and [). Each tab keeps its own folder, history, selection, filter, and server. The tab bar shows with two or more tabs; right-click a tab to close it. Tabs on the Mac are restored at launch.
- Upload on save: a server file opened from forkz is uploaded back over the original each time you save it, with progress. Settings > Transfers can ask first instead.
- FTP and FTPS: explicit TLS when the server offers it (or required, or plain FTP if you choose), with macOS's certificate checks and a fingerprint prompt for self-signed certificates, which forkz then remembers. Listings use MLSD where the server has it; permissions and dates are set where the server allows.
- WebDAV over HTTP or HTTPS (Nextcloud, ownCloud, Apache, and others), with the server's WebDAV path as the site's remote folder.
- Amazon S3 and S3-compatible storage (Backblaze B2, Cloudflare R2, MinIO, Wasabi): buckets at the top, folders inside, uploads up to 5 GB, renaming files, and new buckets and folders. The region comes from the server name unless the site sets one.
- Connect to Server has a protocol menu, and Go to Folder takes `ftp://`, `ftps://`, `dav://`, `davs://`, and `s3://` addresses.
- Interrupted transfers resume: a partial download continues from where it stopped, and so does an upload to SFTP or FTP. Partial files are kept when a transfer fails or is stopped, and used only if the original has not changed since.
- Drag files and folders from a server pane to Finder (or any app that takes files): they download where you drop them, with progress in the activity area.
- Rename Multiple Items (Option-Command-R, or Rename with several items selected): replace text (or a regular expression), insert text, remove characters, change case, add a number, or add the date, each on the name, the extension, or the whole name. A live preview marks empty names, duplicates, and names already taken; rules can be saved as presets. Names that swap places are handled.
- A preview pane (Shift-Command-P) with Info about the selection, a Quick Look preview (server files up to 25 MB are fetched for it), the transfers of this session, and a log of transfers, connections, and changes.
- A shortcut editor in Settings > Shortcuts: change or remove the shortcut of any command, with a warning when a shortcut is already taken.
- Settings > Appearance: light, dark, or the macOS setting; the text size and row spacing of the file lists; and alternating row colors.
- Import from FileZilla also brings FTP, FTPS, S3, and WebDAV sites.

### Changed

- Copy Path on a server is now Copy Server Link, and gives the right kind of address for each protocol.
- Copies within one server use a second connection, as FTP needs one for each direction.

## [0.2.1] - 2026-11-04

### Fixed

- Open in Terminal on a server folder now uses the chosen terminal. With Warp it used Terminal instead; it now opens a Warp window running the SSH session (through a tab config forkz keeps in `~/.warp/tab_configs/forkz_ssh_session.toml`).

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

[Unreleased]: https://github.com/houseofmackee/forkz/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/houseofmackee/forkz/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/houseofmackee/forkz/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/houseofmackee/forkz/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/houseofmackee/forkz/releases/tag/v0.1.0
