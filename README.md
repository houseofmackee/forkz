# forkz

A fast, native dual-pane file manager for macOS on Apple silicon, written in Rust with AppKit.

**[Download the latest release](https://github.com/houseofmackee/forkz/releases/latest)** (Apple silicon, macOS 14 or later): signed with a Developer ID, notarized by Apple, and it updates itself. Or install it with Homebrew: `brew install --cask houseofmackee/tap/forkz`.

**Status:** early development. The local dual-pane file manager, SFTP, FTP and FTPS, WebDAV, and S3 work; cloud drives come later. See [PLAN.md](PLAN.md) for what is done and what is planned.

## What it does today

- Two panes side by side, each with tabs, a sidebar with favorites and volumes, and a customizable toolbar
- A sortable list (name, date modified, size, kind) with ".." on top to go up, a clickable path bar, and a status line with the selection size and free space
- Copy, move, duplicate, trash, rename, and new folder, with progress, Stop, and a conflict dialog (Replace, Replace if Older, Keep Both, Skip, Merge, and "Apply to all")
- Fast copies on APFS (clones), drag and drop with Finder, and Copy and Paste of files
- Quick Look, a hidden files toggle, and live updates when folders change
- A filter field on each list, folder sizes, SHA-256 and MD5 checksums, Compare Files in your diff tool, and Open in Terminal with your terminal app
- Rename Multiple Items with rules (replace, regular expressions, insert, remove, case, numbers, dates), a live preview, and presets
- A preview pane with Info, a Quick Look preview (or an archive's contents), the session's transfers, and a log
- Archives (zip, tar, 7z, rar, iso, and more) browsed like folders, read-only: copy items out to extract them; Compress makes zip, tar.gz, 7z, and other archives
- Search by name or contents (Spotlight on the Mac, and on servers and in archives too), with the results in a pane
- Workspaces: saved sets of tabs for both panes, including server folders
- Folder sync with a preview, saved as synclets in the sidebar, and Sync Browsing
- Transfer speed limits, Pause and Resume, and rules for errors
- Finder tags (colors on icons, and in the sidebar to find everything tagged), a command palette, and Select Matching
- Zoom (Command-= and Command-minus) and a sidebar that can be hidden
- A Settings window (Command-comma), with light and dark appearance, text size and row spacing, and your own keyboard shortcuts

## SFTP

- **Connect** with File > Connect to Server (Command-K), the toolbar, a saved site in the sidebar, or `forkz sftp://user@host:port/path`.
- **Log in** with a password (also keyboard-interactive), a private key file (asks for its passphrase), or the SSH agent. With nothing saved, keys from the agent and `~/.ssh` are tried before asking for a password.
- **Host keys** are checked against `~/.ssh/known_hosts` and keys you trusted in forkz. A new server asks you to compare the SHA256 fingerprint; a changed key gets a strong warning.
- **Sites** keep the server, login, a remote folder to open, and a local folder for the other pane. Passwords are kept in your login Keychain, never in a file.
- **Transfers** between the Mac and a server, or between two servers, run on their own connection with progress and Stop, keep modification dates, and replace files only once the new copy is complete. Drag and drop works between panes and from Finder.
- **On the server:** browse, rename, new folder, delete (asks first: servers have no Trash), open a file (saving it uploads it back), Copy Path (an `sftp://` link), and Open in Terminal (an SSH session in that folder).
- **`~/.ssh/config`** hosts work anywhere a server name goes (HostName, User, Port, IdentityFile, IdentitiesOnly, ProxyCommand, ProxyJump, wildcards, Include); the Connect form lists them. ProxyJump uses the system `ssh` for the jump hosts (key or agent login).
- **Permissions, Owner, and Group** columns; Permissions… (Command-I) changes permissions, optionally for everything inside a folder.
- **Columns** are chosen by right-clicking the column titles or with View > Columns, separately for the Mac and for servers.
- **Simultaneous transfers** (Commands menu, default 2): more uploads and downloads wait their turn.
- Dropped connections reconnect by themselves.
- **Interrupted transfers resume** from their partial file, if the original has not changed since.
- **Drag to Finder:** files and folders dragged from a server pane download where you drop them.

## FTP, WebDAV, and S3

Choose the protocol in Connect to Server; sites keep it.

- **FTP and FTPS:** encrypted with TLS when the server offers it (the default), TLS required, or plain FTP. Certificates are checked by macOS; a self-signed one shows its fingerprint to trust once. Passive mode, MLSD listings where available, and resumed transfers.
- **WebDAV** over HTTPS or HTTP, with Basic login. Put the server's WebDAV path in Remote folder (for Nextcloud, `/remote.php/dav/files/USER`).
- **S3 and S3-compatible storage:** the access key ID goes in User and the secret key in Password (kept in the Keychain). The top level lists your buckets. The region comes from an AWS server name (`s3.eu-west-1.amazonaws.com`); for other providers, set it in Region (Cloudflare R2 uses `auto`). Files up to 5 GB; folders can be created and deleted, files renamed.
- Addresses work in Go to Folder and on the command line: `ftp://`, `ftps://`, `dav://`, `davs://`, `s3://`.

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
| Command-Up, or open ".." | Enclosing folder |
| Command-[ and Command-] | Back and forward |
| Space or Command-Y | Quick Look |
| F5 or Shift-Command-C | Copy to the other pane |
| F6 or Shift-Command-M | Move to the other pane |
| F7 or Shift-Command-N | New folder |
| F8 or Command-Delete | Move to Trash |
| Command-D | Duplicate |
| Option-Command-R | Rename multiple items |
| Option-Command-O | Open with the default app |
| Option-Command-N, Control-Command-N | New file, new folder with the selection |
| Option-Command-Delete | Delete immediately |
| Shift-Command-S, Shift-Command-B | Sync folders, sync browsing |
| Option-Command-A | Select matching items |
| Shift-Command-F | Search |
| Option-Command-P | Command palette |
| Control-Shift-Command-S, Control-Command-1 to 9 | Save a workspace, open a workspace |
| Control-Command-S | Show or hide the sidebar |
| Command-=, Command-minus, Command-0 | Zoom in, zoom out, actual size |
| Shift-Command-. | Show or hide hidden files |
| Shift-Command-G | Go to folder |
| Command-U | Swap panes |
| Option-Command-= | Same folder in the other pane |
| Command-K | Connect to a server |
| Command-E | Disconnect |
| Command-I | Permissions |
| Command-F | Filter the list |
| Option-Command-S | Calculate folder sizes |
| Control-Command-C | Compare files |
| Shift-Command-P | Show or hide the preview pane |
| Control-Command-P | Expand the preview pane to fill the window, or shrink it back |
| Option-Command-1, Option-Command-2 | Hide or show the left pane, the right pane |
| Command-comma | Settings |
| Command-T, Command-W | New tab, close tab |
| Control-Tab, Control-Shift-Tab | Next tab, previous tab |

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
