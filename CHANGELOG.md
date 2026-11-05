# Changelog

All notable changes to forkz. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). The section for a version is what
the GitHub release and Sparkle's update window show.

## [Unreleased]

## [0.9.0] - 2026-11-05

### Added

- SMB, AFP, and NFS: connect to network shares from Connect to Server, a site, or Go to Folder (`smb://server/share`). They are mounted the way Finder mounts them (with macOS's sign-in and share-picker dialogs, and shares already mounted are used), then browsed like folders on the Mac; Disconnect unmounts them. The Remote folder of a site is the share, optionally with a folder in it.
- The command palette finds places, favorites, devices, sites, server folders, synclets, and workspaces; applies rename presets to the selection; and opens the selected file with any app that can open it.
- Search can look for a kind (folders, files, images, movies, audio, documents, archives, source code) and a Finder tag, with or without a name or text.
- Select Matching can add the matching items to the selection or remove them from it.
- Go to Folder takes server addresses (`sftp://`, `ftp://`, `smb://`, and the others) as well as paths.

### Fixed

- Folders under /tmp (and other places reached through a link) now refresh when files change in them.

## [0.8.1] - 2026-11-05

### Added

- Expand Preview Pane (Control-Command-P, or the button at the top left of the preview pane): the preview fills the window, for a closer look; the same button shrinks it back, with the sidebar and panes as they were.

## [0.8.0] - 2026-11-05

### Added

- Sync Folders (Shift-Command-S): compares the two panes' folders, on the Mac or on servers, and shows what would be copied or deleted, with the reason and both sides' sizes and dates. Every item can be left out. One way (left to right or right to left) or both ways; with or without subfolders, hidden items, new items, changed items, empty folders, and deleting what only the target has; a time offset for servers whose clocks differ; and names to leave out. Deleting is off unless chosen for that sync.
- Synclets: a sync saved in the sidebar with its folders and options; clicking one connects to its sites if needed and compares right away.
- Sync Browsing (Shift-Command-B): opening a folder in one pane opens the matching folder in the other.
- Speed limits for uploads and downloads (Settings > Transfers), shared by all transfers.
- Pause and Resume for transfers, and Start Next for one that waits its turn.
- What to do when a transfer hits an error: ask, skip the item, or try three more times and then skip (Settings > Transfers).
- Sites and server folders in the sidebar show when they are connected (a green icon and an eject button); eject, or Disconnect in their menu, closes the connection.
- New File (Option-Command-N), New Folder with Selection (Control-Command-N), and Delete Immediately (Option-Command-Delete), which deletes without the Trash after asking.
- Rename Multiple Items can add the creation date or today's date, as well as the modification date.
- Share (File > Share…) for files on the Mac.
- Araxis Merge for Compare Files, and Hyper for Open in Terminal.

## [0.7.0] - 2026-11-05

### Added

- Stop buttons for searches, finding tagged items, and compressing. A stopped search shows what it found so far; a stopped archive is not kept.
- Compressing shows how much is done ("120 MB of 600 MB").
- Compress works on servers too: the items are downloaded, compressed, and the archive is uploaded beside them, with progress for each step.
- The preview pane follows Zoom In and Zoom Out.

### Fixed

- Rows in the activity area use the full width, with Stop at the right.

- A tag named after one of Finder's colors (Red, Blue, and so on) shows that color even when no color was stored with it, as happens with tags set by other tools.

## [0.6.0] - 2026-11-05

### Added

- Tags in the sidebar: Finder's color tags, each showing everything with that tag (through Spotlight) in the active pane.
- Tag colors on file icons, as dots in the corner (custom tags with a color too).
- Hide Sidebar (Control-Command-S, the macOS standard) and a toolbar button for it.
- Zoom In, Zoom Out, and Actual Size (Command-=, Command-minus, Command-0): the file lists and the sidebar get bigger or smaller.

### Changed

- The hidden files button shows the state: an eye when hidden files show, a crossed-out eye when they do not.
- Save Workspace is now Control-Shift-Command-S, and Same Folder in Other Pane is Option-Command-= (their shortcuts went to Hide Sidebar and Zoom In).

## [0.5.0] - 2026-11-04

### Added

- Search (Shift-Command-F): find items by name (with * and ? patterns) or by what files contain, in the active pane's folder and everything in it. On the Mac it uses Spotlight where the folder is indexed and searches the folders itself elsewhere; on servers and in archives it searches the folders. Results show in the pane like a folder, with a Folder column, and can be opened, copied, moved, renamed, or deleted. ".." returns to the folder searched.
- Workspaces (Window > Workspaces): save the tabs of both panes, including folders on saved sites and hidden panes, and open them again with Control-Command-1 to 9. Server tabs connect one after another.
- Compress (File > Compress…): makes a zip, tar, tar.gz, tar.bz2, tar.xz, or 7z archive of the selection; the name chosen picks the format.
- Open with Default App (Option-Command-O): opens items in their usual app, archives too (instead of browsing them).
- Finder tags: a Tags column, and File > Tags… to set or remove tags on the selection.
- A command palette (Option-Command-P): type part of a command's name, choose it with the arrow keys, and press Return. Saved workspaces are in it too.
- Select Matching (Option-Command-A): selects the items whose names match a pattern such as `*.jpg`.

## [0.4.0] - 2026-11-04

### Added

- Hide Left Pane and Hide Right Pane (Option-Command-1 and Option-Command-2, in the View menu and the toolbar): the other pane fills the window. Hidden panes stay hidden at the next launch.
- Toolbar buttons for the left pane, the right pane, and the preview pane. Toolbars saved by earlier versions get them once.
- A close button in the preview pane.
- Archives open like folders: zip, tar (plain, gzip, bzip2, xz, zstd), 7z, rar, iso, cpio, xar, and more, through the libarchive built into macOS. Copy items out to extract them ("Extract to Other Pane", drag and drop, or drag to Finder), preview files inside, and open archives within archives. Archives on servers are fetched first. ".." or Close Archive (Command-E) leaves the archive. Archives are read-only.
- The preview pane lists an archive's contents with a summary; double-click an item to open the archive there.

### Fixed

- The preview pane no longer reappears at launch after it was hidden.
- Moving the mouse from the divider into a text file preview no longer leaves the resize pointer showing.
- The View menu no longer shows Show Tab Bar and Show All Tabs (macOS window tabs, which forkz does not use: it has tabs in each pane).

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

[Unreleased]: https://github.com/houseofmackee/forkz/compare/v0.9.0...HEAD
[0.9.0]: https://github.com/houseofmackee/forkz/compare/v0.8.1...v0.9.0
[0.8.1]: https://github.com/houseofmackee/forkz/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/houseofmackee/forkz/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/houseofmackee/forkz/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/houseofmackee/forkz/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/houseofmackee/forkz/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/houseofmackee/forkz/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/houseofmackee/forkz/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/houseofmackee/forkz/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/houseofmackee/forkz/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/houseofmackee/forkz/releases/tag/v0.1.0
