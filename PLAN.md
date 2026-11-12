# forkz plan

A native dual-pane file manager and file transfer client for macOS on Apple silicon, in Rust with AppKit (through `objc2`), in the spirit of ForkLift with extras from similar tools. It has its own name, icon, and look; it borrows workflows, not branding.

## Architecture

| Module | What it does | AppKit? |
| --- | --- | --- |
| `fs` | Listing folders, sorting (folders first, natural order), sizes, free space | No |
| `fs::search`, `fs::glob` | Searching by name, contents, kind, and tag (Spotlight on the Mac, walking folders elsewhere); name patterns for search and Select Matching | Foundation only (Spotlight) |
| `fs::sync` | Comparing two folders on any file systems and planning the copies and deletes that make them match | No |
| `fs::git`, `fs::view`, `fs::filters` | Git status of items; the viewer's text decoding and hex rows; filename filters | No |
| `fs::tags`, `fs::checksum` | Finder tags (read from and written to the file's extended attribute); SHA-256 and MD5 | Foundation only (tags) |
| `fs::ops` | Copy, move, trash, and delete jobs between any two file systems: conflict resolution, progress, cancel, retry. Runs on a worker thread and talks to the UI through the `Observer` trait | Foundation only (Trash) |
| `vfs` | The `Vfs` trait; `LocalFs`; `vfs::archive` (archives as read-only folders, through libarchive); `vfs::sftp` (connecting, auth, browsing, streaming); `vfs::ftp` (FTP and FTPS); `vfs::webdav`; `vfs::s3` (Signature Version 4); `vfs::http` (what WebDAV and S3 share); `vfs::known_hosts` (host keys and trusted certificates); `vfs::ssh_config`; `vfs::mount` (SMB, AFP, and NFS shares mounted with NetFS); `vfs::fxp` (direct copies between FTP servers); `vfs::results` (search results shown as a folder) | Foundation only (`vfs::mount`: NetFS) |
| `sites` | Saved sites (`sites.toml`), Keychain passwords, `sites::filezilla` import | Security framework |
| `pane` | One pane's state: location, entries, hidden items, history | No |
| `rename` | Multi-rename rules, the preview with its problems, and renaming with swaps | No |
| `settings` | `~/Library/Application Support/forkz/settings.toml` | No |
| `theme` | Color themes: the built-in ones and the user's TOML files | No |
| `updates` | Sparkle, loaded at runtime when the app bundle has it | Yes |
| `ui` | Window, menus, toolbar, folder watching (FSEvents through `notify`), and app-wide commands | Yes |
| `ui::pane_view` | The file list (NSTableView), path bar, status line, drag and drop, Quick Look, context menu | Yes |
| `ui::sidebar` | Favorites, standard places, volumes, sites (with connection state), synclets, and tags | Yes |
| `ui::activity` | A row per running job, with progress, Stop, and Pause; the transfer queue (reordering, Start Now, Pause All, Stop All) | Yes |
| `ui::dialogs` | Conflict and error alerts (queued, one at a time), text prompts | Yes |
| `ui::multi_rename` | The Rename Multiple Items window | Yes |
| `ui::preview`, `ui::oplog` | The preview pane, and the session's operation log it shows | Yes |
| `ui::promises` | File promises for dragging server items to Finder | Yes |
| `ui::palette` | The command palette | Yes |
| `ui::column_view`, `ui::icon_view`, `ui::gallery_view`, `ui::thumbnails` | Column view (NSBrowser), icon view (NSCollectionView), and gallery (Quick Look above an icon strip), with the hidden list following their selection; content thumbnails (QuickLookThumbnailing) | Yes |
| `ui::viewer`, `ui::filters_editor` | The text and hex viewer; Settings > Filters | Yes |
| `ui::themed`, `ui::theme_editor` | Drawing lists in theme colors; the Customize Theme editor | Yes |
| `ui::file_viewer` | "Show in Finder" in other apps (NSFileViewer), opened folders, and reveal requests | Yes |
| `ui::search` | The Search dialog, and showing its results in a pane | Yes |
| `ui::sync_window` | The Sync Folders window and synclets | Yes |
| `ui::workspaces` | Saving and opening workspaces | Yes |
| `ui::settings_window`, `ui::site_form`, `ui::permissions` | The Settings window, the Connect and site form, and the Permissions dialog | Yes |
| `ui::remote_edit`, `ui::tools` | Upload on save for server files; the terminal and compare apps forkz hands work to | Yes |

The logic that can be tested without a window (`fs`, `fs::ops`, `fs::search`, `fs::sync`, `fs::tags`, `vfs::archive`, `vfs::mount`, `pane`, `rename`, `settings`, `sites`, `vfs::known_hosts`, `vfs::http`, `vfs::s3` signing, `ui::commands`) has unit tests; a fake remote file system runs the streaming transfer code in them. `tests/sftp_live.rs` runs against a real sshd (`scripts/test_sftp.sh` starts a throwaway one), and `tests/protocols_live.rs` against FTP/FTPS (including FXP between servers), WebDAV, and S3 servers (`scripts/test_protocols.sh` starts throwaway ones).

## Phase 1: local dual-pane MVP (done)

- [x] Two panes, side by side, with a sidebar and a customizable toolbar
- [x] List view: name, date modified, size, kind; sortable; column widths saved
- [x] Path bar (click a part to go there), status line (selection, size, free space)
- [x] Navigation: open, enclosing folder, back and forward, Go to Folder, standard places
- [x] Copy and move to the other pane, duplicate, trash, new folder, rename
- [x] Conflicts: Replace, Replace if Older, Keep Both, Skip, Merge (folders), Stop, with "Apply to all"
- [x] Jobs on worker threads with progress (bytes and items), Stop, and Skip or Try Again on errors
- [x] APFS clones for copies on the same volume (`copyfile` with `COPYFILE_CLONE`)
- [x] Drag and drop between panes, to and from Finder; Option copies, Command moves
- [x] Edit > Copy and Paste of files (works with Finder), Move Items Here, Copy Path
- [x] Quick Look (Space), hidden files toggle, live updates when folders change
- [x] Favorites in the sidebar (add, remove, drop folders), mounted volumes
- [x] Keyboard: Tab switches panes, Return renames, F2/F5/F6/F7/F8, Command-Down opens
- [x] Settings saved between launches; signed app bundle

## Phase 2: remote connections

- [x] `Vfs` trait; local file system moved behind it; panes and jobs work on any pair
- [x] SFTP (libssh2 through `ssh2`, OpenSSL built in): password, keyboard-interactive, key files with passphrases, agent, known hosts, reconnects, keepalive
- [x] Connect form, saved sites in the sidebar (with folders), passwords in the Keychain
- [x] Jobs across file systems (local to remote, remote to local, remote to remote), each on its own connection
- [x] Import from FileZilla: sites, passwords, key files, folders, trusted host keys, settings
- [x] Open remote files (download and open), Open in Terminal over SSH, `forkz sftp://...`
- [x] Remote editing: upload on save (or ask first)
- [x] Drag from a server pane to Finder (file promises)
- [x] `~/.ssh/config` hosts (aliases, HostName, User, Port, IdentityFile, IdentitiesOnly, Include)
- [x] ProxyJump and ProxyCommand
- [x] Permissions column and dialog (chmod, recursive)
- [x] Owner and group columns (names from the server’s user and group lists)
- [x] Choose the columns (separately for the Mac and servers)
- [x] Simultaneous transfer limit (FileZilla's "Number of Transfers")
- [x] Resume of partial transfers
- [x] FTP and FTPS (`suppaftp`, with macOS TLS)
- [x] FXP between FTP servers (direct server-to-server copies when both allow it; SSCN for FTPS)
- [x] S3 and S3-compatible (Backblaze B2, R2, MinIO), with Signature Version 4
- [x] Google Cloud Storage (S3-compatible XML API, HMAC keys) and Azure Blob Storage (Shared Key)
- [x] WebDAV
- [x] SMB, AFP, and NFS shares (mounted through NetFS, as Finder does)
- [x] Server addresses in Go to Folder and the command palette

## Phase 3: parity

- [x] Tabs in each pane (restored at launch)
- [x] Workspaces (saved sets of tabs and locations)
- [x] Column view; Group by
- [x] Icon view
- [x] Gallery view, and thumbnails in icon views
- [x] Stacked panes (one above the other)
- [x] Preview pane (info, preview, activity, log)
- [x] Filter field on each list
- [x] Search (name, content, kind, tag; Spotlight locally, scanning remotely), Select Matching
- [x] Smart folders (saved searches in the sidebar)
- [x] Folder sync with preview, saved as "synclets", and sync browsing
- [x] Multi-rename with rules and presets (presets in the sidebar take dropped items)
- [x] Archives browsed as folders (libarchive), extracting by copying out
- [x] Compressing (zip, tar, tar.gz, tar.bz2, tar.xz, 7z)
- [x] Changing archives in place (zip, tar, tar.gz, tar.bz2, tar.xz, and 7z on the Mac)
- [x] App deleter (leftover files when trashing an app)
- [x] Git status in the list
- [ ] Git add, commit, push, pull
- [x] Filename filters for listings and copies
- [x] Open in Terminal for iTerm, Warp, Ghostty, Kitty (and SSH sessions in them)
- [x] Tags (column and editing)
- [x] Box, through OAuth in the browser
- [ ] Cloud drives (Google Drive, OneDrive, Dropbox) through OAuth
- [x] Settings window
- [x] Compare Files in an external tool (FileMerge, Kaleidoscope, Beyond Compare, BBEdit, VS Code)
- [x] Folder sizes; server folder bookmarks
- [x] Shortcut editor; appearance, text size, row spacing, alternating rows
- [x] Color themes: built-in ones and the user's own (TOML files, a Customize editor)
- [x] forkz for "Show in Finder" in other apps (NSFileViewer); macOS refuses other apps as the folder handler
- [x] Transfer queue management (reorder, start now, pause all, stop all)

## Phase 4: extras

- [x] Built-in diff for text and folders; Compare Panes (selects what differs)
- [x] Terminal pane that follows the current folder
- [x] Command palette (commands, places, sites, synclets, workspaces, rename presets, Open With)
- [x] Transfer speed limits, Pause and Resume, and error rules
- [x] Expandable preview pane; zoom
- [ ] Remote locations in Finder (File Provider)
- [x] Checksums (SHA-256, MD5)
- [x] Viewer for text and hex
- [x] Duplicate finder
- [x] Flat view; recent folders
- [x] Disk usage view
- [x] Shortcuts actions (App Intents) and forkz:// links
- [x] Custom tools (shell commands on the selection, in a menu, the toolbar, and the palette)
- [x] Site import from Cyberduck and WinSCP; site export and import between Macs
- [ ] Scripting or plugins (AppleScript)
- [x] Drop stack

## Release

- [x] Sparkle updates (forkz > Check for Updates; the key is `~/Developer/AppSigning/sparkle/forkz.key`)
- [x] Release workflow: DMG and zip, notarization, signed appcast, GitHub release, and the Homebrew cask (`.github/workflows/release.yml`, `docs/RELEASE_CHECKLIST.md`)
- [x] First release (v0.1.0)
- [x] Local release fallback for when GitHub's macOS runners are busy (`scripts/release_local.sh`)
