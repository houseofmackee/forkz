# forkz

A fast, native dual-pane file manager for macOS on Apple silicon, written in Rust with AppKit.

![forkz with a local folder on the left, an FTP server on the right, and a photo in the preview pane](docs/images/main.png)

**[Download the latest release](https://github.com/houseofmackee/forkz/releases/latest)** (Apple silicon, macOS 14 or later): signed with a Developer ID, notarized by Apple, and it updates itself. Or install it with Homebrew: `brew install --cask houseofmackee/tap/forkz`.

**Status:** early development. The local dual-pane file manager, SFTP, FTP and FTPS, WebDAV, S3, and SMB, AFP, and NFS shares work; cloud drives come later. See [PLAN.md](PLAN.md) for what is done and what is planned, and [CHANGELOG.md](CHANGELOG.md) for what changed in each release.

## Features

### Two panes, on the Mac and on servers

- Two panes side by side, each with tabs, and a sidebar with favorites, volumes, saved sites, synclets, and Finder tags. The sidebar, either pane, and the preview pane can each be hidden.
- A line in your accent color along the top of the active pane shows where commands, new connections, and Go to Folder will open. Tabs show a close button when the pointer is over them.
- A sortable list (name, date modified, size, kind, and more) with ".." on top to go up, a clickable path bar, a filter field, and a status line with the selection size and free space
- Copy, move, duplicate, trash, rename, new file, and new folder, with progress, Stop, Pause, and a conflict dialog (Replace, Replace if Older, Keep Both, Skip, Merge, and "Apply to all")
- Fast copies on APFS (clones), drag and drop with Finder, and Copy and Paste of files
- Quick Look, a hidden files toggle, and live updates when folders change
- Folder sizes, SHA-256 and MD5 checksums, Compare Files in your diff tool, and Open in Terminal with your terminal app
- Workspaces: saved sets of tabs for both panes, including server folders
- A transfer queue: speed limits, Pause and Resume, Pause All and Stop All, and waiting transfers that can be started now or moved up and down (right-click a transfer), plus rules for errors
- Zoom (Command-= and Command-minus) and a Settings window (Command-comma) with color themes, text size, row spacing, and your own keyboard shortcuts
- Optionally used for "Show in Finder" in other apps (Settings > General): the file opens selected in forkz. macOS keeps Finder as the app that opens folders, but forkz can open them with Open With

### Preview pane and archives

![The preview pane listing the contents of a zip file](docs/images/archive.png)

The preview pane (Shift-Command-P) shows Info, a Quick Look preview, the session's transfers, and a log. It can expand to fill the window (Control-Command-P) and shrink back. For an archive it lists what is inside.

Archives (zip, tar, 7z, rar, iso, and more) open like folders, read-only: copy items out to extract them, on the Mac or on a server. Compress makes zip, tar.gz, 7z, and other archives.

### Rename Multiple Items

![Rename Multiple Items replacing a prefix and lowercasing extensions, with a live preview](docs/images/rename.png)

Select several items and press Return (or Option-Command-R). Rules (replace, regular expressions, insert, remove, case, numbers, and dates) stack, the preview shows every new name and any problems before anything changes, and presets keep rule sets for next time. Saved presets also appear in the sidebar: drop files on one, or click it, to rename the selection with it.

### Search

![Search results for images across subfolders, with a Folder column](docs/images/search.png)

Search (Shift-Command-F) finds items by name or contents, by kind (folders, images, movies, audio, documents, archives, source code, and more), and by Finder tag, or any mix of these. It uses Spotlight on the Mac and also works on servers and in archives. The results open in a pane, with a column for the folder each one is in, and work like any other list.

### Sync Folders and synclets

![Sync Folders comparing a local folder with a staging server, showing four copies and one delete](docs/images/sync.png)

Sync Folders (Shift-Command-S) compares the two panes' folders, on the Mac or on servers, and shows what would be copied or deleted and why, before anything happens. Any item can be left out. It syncs one way or both ways, with options for subfolders, hidden items, new and changed items, empty folders, deleting what only the target has (off unless chosen), a time offset for servers whose clocks differ, and names to skip.

A sync can be saved as a synclet in the sidebar: clicking it connects to its sites if needed and compares right away. Sync Browsing (Shift-Command-B) keeps the other pane in the matching folder while you browse.

### Transfer queue

![Five uploads to a server: two running, three waiting with their place in line, and Pause All and Stop All above them](docs/images/queue.png)

Transfers beyond the simultaneous limit (Commands menu, default 2) wait in a queue, each showing its place in line. Right-click a transfer to start it now, move it to the top, up, down, or to the bottom, pause it, or stop it. With several transfers, a line above them shows how many are waiting, with Pause All (or Resume All) and Stop All. Speed limits for uploads and downloads, and what to do on errors, are in Settings > Transfers.

### Command palette

![The command palette showing commands and saved sites that match "on"](docs/images/palette.png)

The command palette (Option-Command-P) runs any command and goes anywhere: places, favorites, devices, sites, server folders, synclets, and workspaces. It also applies rename presets to the selection and opens the selected file with any app that can open it.

### Color themes

![forkz in the Dracula theme, with rename presets in the sidebar](docs/images/themes.png)

Settings > Appearance has a theme for the file lists and the sidebar: macOS's own colors (light, dark, or following macOS), or one of the built-in themes, such as GitHub Light, Solarized, Catppuccin, Dracula, Nord, One Dark, Monokai, Gruvbox, and Tokyo Night. Customize… turns the chosen theme into one of your own, with a color well for each color.

<img src="docs/images/theme-editor.png" alt="The Customize Theme editor with a color well for each of a theme's colors" width="380">

Your themes are TOML files in the themes folder (`~/Library/Application Support/forkz/themes`), so they can also be written by hand or shared:

```toml
name = "Sunrise"
dark = false
background = "#fdf6e3"
alternate = "#eee8d5"
text = "#586e75"
secondary = "#93a1a1"
selection = "#268bd2"
selection_text = "#fdf6e3"
accent = "#b58900"
sidebar = "#eee8d5"
```

### Finder tags and Select Matching

- Tags show as colors on file icons; Tags… edits them, and a tag in the sidebar finds everything with it.
- Select Matching (Option-Command-A) selects items by name pattern, and can add them to the selection or remove them from it.

## SFTP

- **Connect** with File > Connect to Server (Command-K), the toolbar, a saved site in the sidebar, Go to Folder, the command palette, or `forkz sftp://user@host:port/path`.
- **Log in** with a password (also keyboard-interactive), a private key file (asks for its passphrase), or the SSH agent. With nothing saved, keys from the agent and `~/.ssh` are tried before asking for a password.
- **Host keys** are checked against `~/.ssh/known_hosts` and keys you trusted in forkz. A new server asks you to compare the SHA256 fingerprint; a changed key gets a strong warning.
- **Sites** keep the server, login, a remote folder to open, and a local folder for the other pane. Passwords are kept in your login Keychain, never in a file. Connected sites show a green icon and an eject button in the sidebar.
- **Transfers** between the Mac and a server, or between two servers, run on their own connection with progress, Stop, and Pause, keep modification dates, and replace files only once the new copy is complete. Drag and drop works between panes and from Finder.
- **On the server:** browse, rename, new folder, delete (asks first: servers have no Trash), open a file (saving it uploads it back), Copy Server Link (an `sftp://` link), and Open in Terminal (an SSH session in that folder).
- **`~/.ssh/config`** hosts work anywhere a server name goes (HostName, User, Port, IdentityFile, IdentitiesOnly, ProxyCommand, ProxyJump, wildcards, Include); the Connect form lists them. ProxyJump uses the system `ssh` for the jump hosts (key or agent login).
- **Permissions, Owner, and Group** columns; Permissions… (Command-I) changes permissions, optionally for everything inside a folder.
- **Columns** are chosen by right-clicking the column titles or with View > Columns, separately for the Mac and for servers.
- **Simultaneous transfers** (Commands menu, default 2): more uploads and downloads wait their turn. Right-click a transfer to start it now or move it up or down the queue; with several, Pause All and Stop All appear above them.
- Dropped connections reconnect by themselves.
- **Interrupted transfers resume** from their partial file, if the original has not changed since.
- **Drag to Finder:** files and folders dragged from a server pane download where you drop them.

## FTP, WebDAV, S3, and network shares

Choose the protocol in Connect to Server; sites keep it.

- **FTP and FTPS:** encrypted with TLS when the server offers it (the default), TLS required, or plain FTP. Certificates are checked by macOS; a self-signed one shows its fingerprint to trust once. Passive mode, MLSD listings where available, and resumed transfers.
- **FXP:** copies between two FTP servers go straight from one server to the other when both allow it, without passing through your Mac (the activity shows "server to server"). Many servers refuse this; forkz checks once per pair of servers and otherwise copies through the Mac as usual. With FTPS, one of the servers must support SSCN; FXP is never used when only one of the two connections is encrypted.
- **WebDAV** over HTTPS or HTTP, with Basic login. Put the server's WebDAV path in Remote folder (for Nextcloud, `/remote.php/dav/files/USER`).
- **S3 and S3-compatible storage:** the access key ID goes in User and the secret key in Password (kept in the Keychain). The top level lists your buckets. The region comes from an AWS server name (`s3.eu-west-1.amazonaws.com`); for other providers, set it in Region (Cloudflare R2 uses `auto`). Files up to 5 GB; folders can be created and deleted, files renamed.
- **SMB, AFP, and NFS** shares are mounted by macOS, as in Finder (its sign-in and share-picker dialogs included), and browsed like folders on the Mac. Put the share in Remote folder (for example `Public` or `Public/Photos`), or leave it empty to choose. A share that is already mounted is used as it is. Disconnect (Command-E) unmounts the share.
- Addresses work in Go to Folder and on the command line: `sftp://`, `ftp://`, `ftps://`, `dav://`, `davs://`, `s3://`, `smb://`, `afp://`, and `nfs://`.

### Importing from FileZilla

File > Import from FileZilla reads `~/.config/filezilla` and shows what it found before changing anything:

- Sites from the Site Manager (SFTP, FTP, FTPS, S3, and WebDAV), with their folders, saved passwords (into the Keychain), key files, and default remote and local folders
- The host keys FileZilla trusts, so known servers do not ask again
- Settings forkz has an equivalent for: show hidden files, keeping modification dates, what to do when a file exists on download or upload, the number of simultaneous transfers, the connection timeout, and extra key files

Sites using implicit FTPS (port 990) or protocols forkz does not have, and passwords protected by a FileZilla master password, are listed as not imported. FileZilla's files are not changed.

## Keyboard

| Key | Action |
| --- | --- |
| Tab | Switch to the other pane |
| Return or F2 | Rename (several items: Rename Multiple Items) |
| Command-Down or Command-O | Open |
| Option-Command-O | Open with the default app |
| Command-Up, or open ".." | Enclosing folder |
| Command-[ and Command-] | Back and forward |
| Command-R | Refresh |
| Space or Command-Y | Quick Look |
| F5 or Shift-Command-C | Copy to the other pane |
| F6 or Shift-Command-M | Move to the other pane |
| Command-C, Command-V, Option-Command-V | Copy items, paste items, move items here |
| Option-Command-C | Copy Path (on a server: Copy Server Link) |
| F7 or Shift-Command-N | New folder |
| Option-Command-N, Control-Command-N | New file, new folder with the selection |
| Command-D | Duplicate |
| F8 or Command-Delete | Move to Trash |
| Option-Command-Delete | Delete immediately |
| Option-Command-R | Rename multiple items |
| Command-A | Select all items |
| Option-Command-A | Select matching items |
| Command-F | Filter the list |
| Shift-Command-F | Search |
| Option-Command-P | Command palette |
| Shift-Command-S, Shift-Command-B | Sync folders, sync browsing |
| Shift-Command-G | Go to folder |
| Shift-Command-H, Shift-Command-D, Shift-Command-O, Option-Command-L, Shift-Command-A | Home, Desktop, Documents, Downloads, Applications |
| Shift-Command-R | Show in Finder |
| Option-Command-T | Open in Terminal |
| Control-Command-T | Add to the sidebar |
| Command-U | Swap panes |
| Option-Command-= | Same folder in the other pane |
| Command-K, Command-E | Connect to a server, disconnect |
| Command-I | Permissions |
| Option-Command-S | Calculate folder sizes |
| Control-Command-C | Compare files |
| Shift-Command-. | Show or hide hidden files |
| Shift-Command-P | Show or hide the preview pane |
| Control-Command-P | Expand the preview pane to fill the window, or shrink it back |
| Control-Command-S | Show or hide the sidebar |
| Option-Command-1, Option-Command-2 | Hide or show the left pane, the right pane |
| Control-Shift-Command-S, Control-Command-1 to 9 | Save a workspace, open a workspace |
| Command-=, Command-minus, Command-0 | Zoom in, zoom out, actual size |
| Command-T, Command-W | New tab, close tab |
| Control-Tab, Control-Shift-Tab | Next tab, previous tab |
| Command-comma | Settings |

Settings > Shortcuts changes any of the menu shortcuts.

## Requirements

- macOS 14 or later
- Apple silicon

## Building

```sh
cargo run                # run from source
cargo test               # unit tests
scripts/test_sftp.sh     # live SFTP tests against a throwaway local sshd
scripts/test_protocols.sh  # live FTP/FTPS, WebDAV, and S3 tests against throwaway local servers (Python)
scripts/bundle.sh        # dist/forkz.app with Sparkle, signed (Developer ID if available)
scripts/make_icon.sh     # macos/AppIcon.svg to AppIcon.icns (needs librsvg)
```

## Releasing

Pushing a version tag (`git tag v0.1.0 && git push origin v0.1.0`) runs the Release workflow: it builds forkz, signs it with the Developer ID, notarizes it, and publishes it as the latest release with the DMG, zip, checksums, and a signed `appcast.xml` (so Sparkle offers it right away), then updates the Homebrew cask. The steps are in [docs/RELEASE_CHECKLIST.md](docs/RELEASE_CHECKLIST.md).

```sh
scripts/dist.sh          # the release files in dist/ (FORKZ_SPARKLE_KEY signs the appcast)
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
