# Release checklist

Work through this for every release, on an Apple silicon Mac.

## 1. Prepare

- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` are clean; `scripts/test_sftp.sh` passes.
- [ ] CI is green on main (it also runs `cargo deny check`).
- [ ] `CHANGELOG.md`: the Unreleased notes moved into a new `## [X.Y.Z] - YYYY-MM-DD` section.
- [ ] Version bumped in `Cargo.toml`, and `CFBundleVersion` in `macos/Info.plist` raised by one (Sparkle compares this build number).
- [ ] README is current.

## 2. Release

- [ ] Commit, then tag and push: `git tag vX.Y.Z && git push origin main vX.Y.Z`.
- [ ] The Release workflow passes, and the release (published as the latest, regular release) has the DMG, zip, `SHA256SUMS.txt`, and `appcast.xml`.
- [ ] `Casks/forkz.rb` in houseofmackee/homebrew-tap has the new version and the DMG's checksum.
- [ ] https://github.com/houseofmackee/forkz/releases/latest/download/appcast.xml offers the new version.

## 3. Install

- [ ] The DMG opens and shows forkz, the Applications link, and "Read Me First.txt".
- [ ] After dragging to Applications, forkz opens without a Gatekeeper warning.
- [ ] `spctl -a -vv /Applications/forkz.app` says "accepted, source=Notarized Developer ID", and `codesign -dr - /Applications/forkz.app` shows `certificate leaf[subject.OU] = ZD5VPNGP3T`.
- [ ] Upgrading over the previous release keeps the settings, sites, and the passwords in the Keychain (no Keychain prompt).
- [ ] The previous version offers it with Check for Updates…, shows the changelog notes, installs it, and relaunches.
- [ ] `brew install --cask houseofmackee/tap/forkz` installs the new version.

## 4. Use

- [ ] Two panes, sorting, "..", hidden files, Quick Look, and the context menus on the Mac and on a server.
- [ ] Copy, move, rename, new folder, trash, and a conflict, locally.
- [ ] Connect to a saved site (password from the Keychain), a key-file site, and an `~/.ssh/config` alias; upload, download, rename, delete, and change permissions on the server.
- [ ] Import from FileZilla shows the right summary.
