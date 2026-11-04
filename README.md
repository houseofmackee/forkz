# forkz

A fast, native dual-pane file manager for macOS on Apple silicon, written in Rust with AppKit.

**Status:** early development. The local dual-pane file manager and SFTP work; FTP, S3, WebDAV, and cloud drives come later. See [PLAN.md](PLAN.md) for what is done and what is planned.

## What it does today

- Two panes side by side, a sidebar with favorites and volumes, and a customizable toolbar
- A sortable list (name, date modified, size, kind) with ".." on top to go up, a clickable path bar, and a status line with the selection size and free space
- Copy, move, duplicate, trash, rename, and new folder, with progress, Stop, and a conflict dialog (Replace, Replace if Older, Keep Both, Skip, Merge, and "Apply to all")
- Fast copies on APFS (clones), drag and drop with Finder, and Copy and Paste of files
- Quick Look, a hidden files toggle, and live updates when folders change

## SFTP

- **Connect** with File > Connect to Server (Command-K), the toolbar, a saved site in the sidebar, or `forkz sftp://user@host:port/path`.
- **Log in** with a password (also keyboard-interactive), a private key file (asks for its passphrase), or the SSH agent. With nothing saved, keys from the agent and `~/.ssh` are tried before asking for a password.
- **Host keys** are checked against `~/.ssh/known_hosts` and keys you trusted in forkz. A new server asks you to compare the SHA256 fingerprint; a changed key gets a strong warning.
- **Sites** keep the server, login, a remote folder to open, and a local folder for the other pane. Passwords are kept in your login Keychain, never in a file.
- **Transfers** between the Mac and a server, or between two servers, run on their own connection with progress and Stop, keep modification dates, and replace files only once the new copy is complete. Drag and drop works between panes and from Finder.
- **On the server:** browse, rename, new folder, delete (asks first: servers have no Trash), open a file (downloads it and opens it), Copy Path (an `sftp://` link), and Open in Terminal (an SSH session in that folder).
- **`~/.ssh/config`** hosts work anywhere a server name goes (HostName, User, Port, IdentityFile, IdentitiesOnly, ProxyCommand, ProxyJump, wildcards, Include); the Connect form lists them. ProxyJump uses the system `ssh` for the jump hosts (key or agent login).
- **Permissions, Owner, and Group** columns; Permissions… (Command-I) changes permissions, optionally for everything inside a folder.
- **Columns** are chosen by right-clicking the column titles or with View > Columns, separately for the Mac and for servers.
- **Simultaneous transfers** (Commands menu, default 2): more uploads and downloads wait their turn.
- Dropped connections reconnect by themselves.

### Importing from FileZilla

File > Import from FileZilla reads `~/.config/filezilla` and shows what it found before changing anything:

- SFTP sites from the Site Manager, with their folders, saved passwords (into the Keychain), key files, and default remote and local folders
- The host keys FileZilla trusts, so known servers do not ask again
- Settings forkz has an equivalent for: show hidden files, keeping modification dates, what to do when a file exists on download or upload, the number of simultaneous transfers, the connection timeout, and extra key files

FTP sites and passwords protected by a FileZilla master password are listed as not imported. FileZilla's files are not changed.

## Keyboard

| Key | Action |
| --- | --- |
| Tab | Switch to the other pane |
| Return or F2 | Rename |
| Command-Down or Command-O | Open |
| Command-Up, or open ".." | Enclosing folder |
| Command-[ and Command-] | Back and forward |
| Space or Command-Y | Quick Look |
| F5 or Shift-Command-C | Copy to the other pane |
| F6 or Shift-Command-M | Move to the other pane |
| F7 or Shift-Command-N | New folder |
| F8 or Command-Delete | Move to Trash |
| Command-D | Duplicate |
| Shift-Command-. | Show or hide hidden files |
| Shift-Command-G | Go to folder |
| Command-U | Swap panes |
| Command-= | Same folder in the other pane |
| Command-K | Connect to a server |
| Command-E | Disconnect |
| Command-I | Permissions |

## Requirements

- macOS 14 or later
- Apple silicon

## Building

```sh
cargo run                # run from source
cargo test               # unit tests
scripts/test_sftp.sh     # live SFTP tests against a throwaway local sshd
scripts/bundle.sh        # dist/forkz.app with Sparkle, signed (Developer ID if available)
scripts/make_icon.sh     # macos/AppIcon.svg to AppIcon.icns (needs librsvg)
```

## Releasing

Pushing a version tag (`git tag v0.1.0 && git push origin v0.1.0`) runs the Release workflow: it builds forkz, signs it with the Developer ID, notarizes it, and publishes a pre-release with the DMG, zip, checksums, and a signed `appcast.xml`, then updates the Homebrew cask. Marking the release as regular makes Sparkle offer it. The steps are in [docs/RELEASE_CHECKLIST.md](docs/RELEASE_CHECKLIST.md).

```sh
scripts/dist.sh          # the release files in dist/ (FORKZ_SPARKLE_KEY signs the appcast)
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
