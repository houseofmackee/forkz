# forkz plan

A native dual-pane file manager and file transfer client for macOS on Apple silicon, in Rust with AppKit (through `objc2`), in the spirit of ForkLift with extras from similar tools. It has its own name, icon, and look; it borrows workflows, not branding.

## Architecture

| Module | What it does | AppKit? |
| --- | --- | --- |
| `fs` | Listing folders, sorting (folders first, natural order), sizes, free space | No |
| `fs::ops` | Copy, move, trash, and delete jobs between any two file systems: conflict resolution, progress, cancel, retry. Runs on a worker thread and talks to the UI through the `Observer` trait | Foundation only (Trash) |
| `vfs` | The `Vfs` trait; `LocalFs`; `vfs::archive` (archives as read-only folders, through libarchive); `vfs::sftp` (connecting, auth, browsing, streaming); `vfs::ftp` (FTP and FTPS); `vfs::webdav`; `vfs::s3` (Signature Version 4); `vfs::http` (what WebDAV and S3 share); `vfs::known_hosts` (host keys and trusted certificates) | No |
| `sites` | Saved sites (`sites.toml`), Keychain passwords, `sites::filezilla` import | Security framework |
| `pane` | One pane's state: location, entries, hidden items, history | No |
| `rename` | Multi-rename rules, the preview with its problems, and renaming with swaps | No |
| `settings` | `~/Library/Application Support/forkz/settings.toml` | No |
| `ui` | Window, menus, toolbar, folder watching (FSEvents through `notify`), and app-wide commands | Yes |
| `ui::pane_view` | The file list (NSTableView), path bar, status line, drag and drop, Quick Look, context menu | Yes |
| `ui::sidebar` | Favorites, standard places, volumes | Yes |
| `ui::activity` | A row per running job, with progress and Stop | Yes |
| `ui::dialogs` | Conflict and error alerts (queued, one at a time), text prompts | Yes |
| `ui::multi_rename` | The Rename Multiple Items window | Yes |
| `ui::preview`, `ui::oplog` | The preview pane, and the session's operation log it shows | Yes |
| `ui::promises` | File promises for dragging server items to Finder | Yes |

The logic that can be tested without a window (`fs`, `fs::ops`, `pane`, `rename`, `settings`, `sites`, `vfs::known_hosts`, `vfs::http`, `vfs::s3` signing, `ui::commands`) has unit tests; a fake remote file system runs the streaming transfer code in them. `tests/sftp_live.rs` runs against a real sshd (`scripts/test_sftp.sh` starts a throwaway one), and `tests/protocols_live.rs` against FTP/FTPS, WebDAV, and S3 servers (`scripts/test_protocols.sh` starts throwaway ones).

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
- [x] S3 and S3-compatible (Backblaze B2, R2, MinIO), with Signature Version 4
- [x] WebDAV

## Phase 3: parity

- [x] Tabs in each pane (restored at launch)
- [ ] Workspaces (saved sets of tabs and locations)
- [ ] Column and icon views; Group by
- [x] Preview pane (info, preview, activity, log)
- [x] Filter field on each list
- [ ] Search (name, content, Spotlight locally, scanning remotely), Quick Select
- [ ] Folder sync with preview, saved as "synclets"
- [x] Multi-rename with rules and presets
- [x] Archives browsed as folders (libarchive), extracting by copying out
- [ ] Compressing, and changing archives
- [ ] App deleter (leftover files when trashing an app)
- [ ] Git status in the list; add, commit, push, pull
- [x] Open in Terminal for iTerm, Warp, Ghostty, Kitty (and SSH sessions in them)
- [ ] Tags
- [ ] Cloud drives (Google Drive, OneDrive, Dropbox) through OAuth
- [x] Settings window
- [x] Compare Files in an external tool (FileMerge, Kaleidoscope, Beyond Compare, BBEdit, VS Code)
- [x] Folder sizes; server folder bookmarks
- [x] Shortcut editor; themes (appearance, text size, row spacing, alternating rows)

## Phase 4: extras

- [ ] Built-in diff for text and folders
- [ ] Terminal pane that follows the current folder
- [ ] Command palette
- [ ] Remote locations in Finder (File Provider)
- [x] Checksums (SHA-256, MD5)
- [ ] Disk usage view, duplicate finder, hex viewer
- [ ] Scripting or plugins
- [ ] Drop stack

## Release

- [x] Sparkle updates (forkz > Check for Updates; the key is `~/Developer/AppSigning/sparkle/forkz.key`)
- [x] Release workflow: DMG and zip, notarization, signed appcast, GitHub release, and the Homebrew cask (`.github/workflows/release.yml`, `docs/RELEASE_CHECKLIST.md`)
- [x] First release (v0.1.0)
- [x] Local release fallback for when GitHub's macOS runners are busy (`scripts/release_local.sh`)
