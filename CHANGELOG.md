# Changelog

All notable changes to forkz. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/). The section for a version is what
the GitHub release and Sparkle's update window show.

## [Unreleased]

## [0.27.0] - 2026-11-27

### Added

- Uploads and downloads are checked against the checksum the server keeps (S3, Google Drive, Dropbox, Box, Azure, and SFTP servers that run `sha256sum` or `shasum`); a copy that differs is made again. Settings > Transfers can turn it off.
- When a connection drops or a server is busy (HTTP 429 or 503, for example), transfers connect again and go on, up to five times, waiting 1 to 16 seconds, before showing the error. Connecting at the start of a transfer tries again the same way.

## [0.26.0] - 2026-11-27

### Added

- Tab and Shift-Tab while renaming in the list or icon view: the item is renamed, and the next or previous one is ready to rename.
- Close Other Tabs (Option-Command-W), Close Tabs to the Left, and Close Tabs to the Right, in the Window menu and when you right-click a tab.
- File > New Folder in Other Pane.
- The filter field takes patterns (`*.pdf`, several separated by `;`) and regular expressions between slashes (`/^IMG_\d+/`), besides text in the name.
- View > Show Item Info: under each name in the icon view, a folder's number of items, an image's size, or a movie's or song's length.
- Tagged folders take their tag's color, as in Finder.

## [0.25.0] - 2026-11-27

### Added

- A Raycast extension (in `raycast/`, for the Raycast Store): connect to saved sites, open recent folders and favorites, run synclets, open smart folders and workspaces, open Finder's folder in forkz, and search files.
- A terms of use page with the home page and privacy policy (`site/forkz`).
- Copying and syncing without a window: `forkz copy`, `forkz sync`, and `forkz synclet` upload and download with your saved sites from Terminal or a script.

## [0.24.2] - 2026-11-27

### Fixed

- A force click in the list did not show Quick Look (or showed it only when the click ended): the list held the trackpad's pressure until then. forkz now catches the force click while you press, in the list and in icon view, also when "Look up & data detectors" is set to a three-finger tap. A force click on a selected name no longer starts renaming it ([#18](https://github.com/houseofmackee/forkz/issues/18)).
- Icon view renames in place too: click the name of the selected icon again, or press Return.
- In icon view, commands (Rename, Move to Trash, Copy, and the rest) could act on nothing after a folder was listed again, though an item still looked selected. Reading the Dimensions, Duration, Date Last Opened, or Comments columns lost the list's selection.

## [0.24.1] - 2026-11-27

### Added

- The right-click menu has Share… near the top, as in Finder (AirDrop, Mail, Messages, and the rest), for items on the Mac. The share sheet points at the right-clicked item.

### Fixed

- Disconnecting from a server (or closing an archive) took the pane to the home folder instead of the folder it was in before connecting. It now goes back to that folder (the latest folder on the Mac in its history that still exists).

## [0.24.0] - 2026-11-26

### Added

- Dropbox and Google Drive: choose them in Connect to Server and sign in in the browser (forkz keeps the sign-in in the Keychain). Google Docs, Sheets, and Slides show as Word, Excel, and PowerPoint files and are exported when copied or opened; copies within each service happen on its servers; share links work on both.
- The viewer (F3) highlights code and settings files (Rust, Python, JavaScript, Swift, C, Go, Java, Ruby, PHP, shell, SQL, CSS, JSON, YAML, TOML, HTML, XML, Markdown, and more), and can edit them: check Edit, and Command-S saves, in the file's encoding, on the Mac or the server. Closing with unsaved changes asks first.
- Commands > Repeat Last Command (Shift-Option-Command-R) runs the last command on items again, such as Compress, Rotate Right, or a custom tool, on what is selected now.
- File > Disconnect All closes every server connection and mounted share.
- The Dock icon's menu lists recent folders and favorites, to open in forkz.
- The Dimensions, Duration, Date Last Opened, and Comments columns can be sorted by.
- A force click on a file (on a trackpad) shows it in Quick Look, as in Finder ([#17](https://github.com/houseofmackee/forkz/issues/17)).

### Fixed

- Files uploaded one by one to Google Drive lost their modification date. Files copied to cloud storage (Google Drive, Dropbox, Box, S3, and Azure) are now written straight to their names, which also saves a request per file.

## [0.23.1] - 2026-11-26

### Added

- Rename in the list, as in Finder: press Return (or F2), or click the name of the selected item again, and type the new name; the part before the extension is selected. Return or a click elsewhere renames, and Escape leaves it as it was ([#12](https://github.com/houseofmackee/forkz/issues/12)). Settings > General > Rename in the list turns it off (Return then asks in a dialog, as before).

### Changed

- Updated notify (folder watching) to 9.0, suppaftp (FTP) to 13.0, regex, and toml. suppaftp no longer uses the unmaintained chrono crate.

## [0.23.0] - 2026-11-26

### Added

- Search by size (larger or smaller than a size you type) and by date modified: today, the last 7 or 30 days, the last 12 months, more than 30 days or a year ago, or between two dates. Smart folders keep dates relative, so "the last 7 days" stays current. `forkz://search` links take `larger=`, `smaller=`, and `days=`.
- S3 sites can use an AWS profile from `~/.aws/config` and `~/.aws/credentials` instead of keys, including IAM Identity Center (SSO): forkz uses the sign-in the AWS CLI made and renews it, or signs in in the browser itself. Roles assumed from another profile (with an MFA code if needed), `credential_process`, and keys with session tokens work too, and expiring keys are renewed while connected. `s3://profile@bucket` opens a bucket with a profile, and `s3://bucket` offers your profiles first.
- New columns for folders on the Mac: Dimensions (of images and videos), Duration (of videos and audio), Date Last Opened, and Comments (Finder comments).
- View > Group By > Tags groups items by their Finder tag.
- Settings > General > Remember view settings for each folder: the view, sort, and grouping chosen in a folder are kept for it; other folders use the default view (View > Use as Default View sets it, and Reset to Default View forgets a folder's own).
- Settings has a search field: it shows the tab with matching settings and marks them, and a command's name finds its shortcut.

## [0.22.0] - 2026-11-26

### Added

- Undo and Redo (Command-Z and Shift-Command-Z) of file changes, as in Finder: renames (also Rename Multiple Items), moves (also between the Mac and servers), Move to Trash (the items are put back), and new files, new folders, and copies on the Mac (they go to the Trash). The Edit menu names what Undo and Redo will do.
- Rename Multiple Items can use the date a photo or video was taken (from the photo's EXIF data, or what Spotlight knows of a video).
- File > Checksums > Create Checksum File writes a checksum file (MD5, SHA-1, SHA-256, SHA-384, or SHA-512, as `shasum` writes them) for the selected files and everything in selected folders. Verify Checksums checks the files a checksum file lists (also BSD-style ones and SHA256SUMS), or a selected file against a checksum on the clipboard, and says which differ or are missing. Both work on servers too.
- Edit > Copy Share Link puts a link on the clipboard that anyone can open to download a file, for 1 hour to 7 days: a presigned URL on S3 and Google Cloud Storage, a shared access signature on Azure, and a shared link on Box (files and folders).
- Pasting an image (a screenshot, or an image copied in a browser or an editor) into a pane saves it as a PNG file there.
- New File can start from a template: files in the templates folder (New File > Open Templates Folder) are offered.
- File > Make Link makes symbolic links, Finder aliases, and hard links: in the other pane's folder when it shows another folder on the Mac, else beside the items.
- File > Set Date sets the modification date of files and folders (to now, or a date you type), as `touch` does.
- File > Flatten Folder moves everything in a folder's subfolders up into it and removes the emptied subfolders (Undo puts it all back).
- File > Images converts images to JPEG, PNG, HEIC, TIFF, GIF, or BMP, makes smaller copies, rotates them (in place, as in Finder), or makes one PDF of them.
- File > Split File splits a large file into parts of any size ("movie.mkv.001", ".002", and so on, as 7-Zip and `split` name them), and Join Files puts them back together.
- Commands > Copy to Several Folders copies the selection to each folder checked: the other pane's, favorites, recent folders, or others you pick.

### Fixed

- Folders copied or downloaded to the Mac did not keep their dates (with Keep modification dates on): macOS refused the way forkz set them.

## [0.21.1] - 2026-11-25

### Added

- forkz > Install Command Line Tool: a `forkz` command in Terminal, so `forkz .` opens the current folder in forkz (Homebrew installs it already). The command now hands folders and addresses to the running forkz instead of starting a second copy ([#11](https://github.com/houseofmackee/forkz/issues/11)).

### Fixed

- A slow or unresponsive drive (an external disk on a slow connection, a stalled network share) froze forkz. Folders on the Mac are now read in the background: they show at once as before, and a slow one shows "Loading" while forkz stays usable. Tags and free space are read with the listing, folders on other volumes get their icons by type, and looking for a Git repository happens in the background too ([#9](https://github.com/houseofmackee/forkz/issues/9)).
- The bundled zlib (used by SFTP) is updated for a heap buffer overflow (RUSTSEC-2026-0544), and ureq to 3.5.2.

## [0.21.0] - 2026-11-14

### Added

- Hovering over a name in the list shows the full file or folder name, for names too long for the Name column.
- `s3://bucket/folder` addresses (in Go to Folder and on the command line) open a bucket on AWS: forkz asks for the access key ID and secret key, and finds the bucket's region itself. With the server `s3.amazonaws.com` and no region set, a site finds its bucket's region the same way.

- Folders copied to or from a server (or on a server) have their files copied four at a time, each with its own connection: many small files go several times faster. Files that fail are tried again one by one afterwards, the usual way (asking what to do, and resuming partial files).
- Uploads to S3, Google Cloud Storage, Azure, and Box are written straight to their names (those services only show a file once it is complete), saving the copy and delete a rename takes there.

### Fixed

- Copies and moves of folders with many items on a server took very long to start ("Preparing...") and to run: forkz looked up every item again before counting and copying it. It now uses what folder listings already say, and on S3 and Azure counts a whole folder tree from a single listing (1,000 items a request). Thousands of items now start in seconds.
- S3 keys limited to one bucket were not accepted ("The keys were not accepted"): forkz checked them by listing all buckets. It now checks them with the bucket being opened.

## [0.20.2] - 2026-11-13

### Added

- In a Git repository, the end of a pane's status line shows the branch (and how far it is ahead of or behind its upstream) as a menu of the repository's branches: choose one to switch to it.

## [0.20.1] - 2026-11-12

### Added

- Copies and moves within one connection to S3, Google Cloud Storage, Azure Blob Storage, or Box happen on the server: nothing is downloaded and uploaded again, so large files copy in a fraction of the time. On S3, files over 5 GB are copied in parts, still on the server.

### Fixed

- Moving a folder within one S3 or Azure connection (to another bucket or container, say) stopped with "Folders cannot be renamed". It is now copied on the server, then deleted.

## [0.20.0] - 2026-11-12

### Added

- Git: a Git menu (and a Git submenu in the context menu, in a repository) to stage and unstage the selected items, commit (with a message, optionally staging everything first), pull, push (the first push of a branch sets its upstream), and switch branches or make a new one. git never waits for a password or an editor: if something is needed, its message says so. The status line shows the branch, and how far it is ahead of or behind its upstream.
- Type-to-select in icon view and the gallery: typing a name selects the item, as in the list.
- App Cleaner (Commands > App Cleaner): choose apps (or select them first) to see everything they left on the Mac, as the AppCleaner app shows it, and move the app and the files you keep checked to the Trash in one go. Besides your Library (now with crash reports too), it finds what installers put in /Library for all users: support files, settings, background services, and helper tools. Those start unchecked, and Finder asks for an administrator's password once for all of them. Moving an app to the Trash offers the same list.

### Fixed

- Moving an app installed for all users (from the App Store, or with an installer) to the Trash failed with "you don't have permission". forkz now asks Finder to do it, as Finder itself would: macOS asks for an administrator's password (once for all such items), and the first time, whether forkz may control Finder.
- The offer to remove an app's leftover files could fail to appear when moving the app to the Trash took more than 30 seconds (answering a password prompt, say).

## [0.19.0] - 2026-11-12

### Added

- Open With in the context menu: the default app, the other apps that can open the items, and Other to choose one. Items on servers and in archives are downloaded first.
- Gallery view (View > as Gallery, Command-4): a large preview of the selected item with its name and details, above a strip of the folder's items.
- Icon view and the gallery show thumbnails of files' contents, as Finder does.
- Folders on servers and cloud services (SFTP, FTP, WebDAV, S3, Box, and the rest) show at once when you open them again: forkz shows their last listing ("updating" in the status line) while it lists them again in the background, then swaps in the fresh one, keeping the selection. Changes made in forkz still show immediately. Settings > Transfers > Server folders turns it off ([#8](https://github.com/houseofmackee/forkz/issues/8)).
- Refresh buttons: one in each pane's bar (next to Filter) refreshes that pane, and one in the toolbar (also View > Refresh All Panes, Control-Command-R) refreshes both. Refreshing lists a server folder again, skipping its last listing.

## [0.18.3] - 2026-11-11

### Added

- Settings > General > Dragging: "Always move items dragged between panes" moves them anywhere on this Mac (also to another disk) or within one server. Hold Option to copy. When off, dragging works as in Finder: a move on the same disk, a copy to another.

### Fixed

- Dragging to or from search results, a tag, a Smart Folder, or Flat View copied items even within one disk (it counted as another location). It now moves on the same disk, as from a folder. Dropping on the empty part of such a list, which is no folder, is no longer possible.

## [0.18.2] - 2026-11-10

### Added

- Drag files and folders into the terminal pane (from the panes or Finder) to type their paths, quoted for the shell. Items from a server pane give their paths on the server. Dropped text is pasted.

## [0.18.1] - 2026-11-10

### Fixed

- The terminal pane lined up columns wrongly (as in `ls`): text after a tab, and after characters such as icons or accented letters, drifted. Each character now sits in its cell.
- Shell prompt icons (Powerlevel10k, Oh My Posh, and similar) showed as boxes with question marks. The terminal now draws them with an installed Nerd Font, such as MesloLGS NF.

## [0.18.0] - 2026-11-10

### Added

- Box: choose it in Connect to Server and sign in in the browser. Browse, copy both ways (large files in parts), rename, move, and delete (into Box's trash); forkz remembers the sign-in in the Keychain.

### Fixed

- `forkz` on the command line accepted only `sftp://` addresses; it now takes all the kinds Go to Folder does (`ftp://`, `s3://`, `box://`, and so on).

## [0.17.0] - 2026-11-10

### Added

- A terminal pane below the panes (View > Show Terminal, Control-`): your shell, starting in the active pane's folder and following it to each folder you go to while it waits for a command. View > Terminal Follows Active Pane turns that off.
- Google Cloud Storage (with HMAC keys) and Azure Blob Storage (with the storage account's access key) in Connect to Server, as `gs://` and `azure://` addresses too.

## [0.16.4] - 2026-11-08

### Fixed

- In iCloud Drive, the path bar showed "iCloud Drive" twice, and the first led to the folder of apps' containers. It now starts at iCloud Drive (then the app's folder, if in one, and the folders below).

## [0.16.3] - 2026-11-08

### Fixed

- iCloud Drive works as in Finder: it also shows the folders of apps that keep documents in iCloud (Keynote, Numbers, Preview, and so on) under the apps' names, and going up no longer leads into the folder of apps' containers (with their private data).
- Locations listed Time Machine's snapshot and backup mounts, and folders they leave behind in /Volumes, which then stayed there. It now lists only the mounted volumes Finder shows.

## [0.16.2] - 2026-11-07

### Added

- A Help menu: macOS's search field for finding any command, forkz Help, Keyboard Shortcuts, What's New in forkz, All Release Notes, Report an Issue, forkz on GitHub, and Customize Keyboard Shortcuts.

## [0.16.1] - 2026-11-07

### Added

- Right-click Copy To and Move To: to the other pane, any other open tab, or a folder you choose.
- Right-click Extract To for archives: into this folder, the other pane, another tab, or a folder you choose. An archive with one folder at its top gives that folder; otherwise its items go into a new folder named after it.
- Compress in the right-click menu.
- Icons in the menus (the menu bar and the right-click menu).

### Changed

- The import commands are in File > Import Sites.

## [0.16.0] - 2026-11-07

### Added

- Disk Usage (Commands menu): the pane's folder and everything in it as a tree, largest first, with each item's size, share of its folder, and number of files. Items can be shown in the pane or, on the Mac, moved to the Trash. Works on servers too.
- Drop Stack (Window menu, and a toolbar button): a floating shelf to gather files from several folders or Finder, then drag them out together or copy or move them to the active pane. It keeps its files between launches.

### Changed

- Export Folder Tree lists the pane's folder (it used the selected folder, and a pane nearly always has one selected).

## [0.15.2] - 2026-11-07

### Fixed

- After moving (or deleting) several selected items, the source pane selected other items that had taken their rows; it now selects just the item that took the first one's place.

## [0.15.1] - 2026-11-07

### Added

- iCloud Drive in the sidebar's Locations (when iCloud Drive is on), named as in Finder.
- Sidebar items can be put in any order by dragging them within their section (Favorites, Locations, Sites, Synclets, Rename Presets, Smart Folders, and Tags); the order is kept.
- Network in the sidebar's Locations: the file servers on your network that announce themselves (SMB, AFP, SFTP, and WebDAV); open one to connect to it. macOS may ask once to let forkz look on the local network.

## [0.15.0] - 2026-11-07

### Added

- Custom tools (Settings > Tools): your own shell commands, run on the selection from Commands > Tools, the toolbar, or the command palette, with placeholders for the selection, the folder, and the other pane's. A tool can show its output; errors are shown. A few examples come with forkz.
- Import from Cyberduck and Import from WinSCP (File menu), and Export Sites and Import Sites from File, for moving your sites to another Mac (without passwords).
- Reopen Closed Tab (Shift-Command-T), Move Tab to Other Pane (Window menu), and Select Same Type (Edit menu).
- The mouse's Back and Forward buttons go back and forward in the pane under the pointer.
- SHA-1, SHA-384, and SHA-512 checksums, in File > Copy Checksum with MD5 and SHA-256.
- Export Folder Tree (File menu): a text file listing a folder and everything in it.
- Sound cues: Finder's sounds for moving items to the Trash and for deleting them for good, and a chime when a transfer that took 5 seconds or more finishes (another sound if some items failed). They follow macOS's "Play user interface sound effects" setting, and Settings > General can turn each off.

## [0.14.0] - 2026-11-07

### Added

- Shortcuts actions: Open in forkz (a folder, or a file selected in its folder, in either pane or a new tab), Connect to Site in forkz, Search in forkz, Open Synclet, Open Smart Folder, Open Workspace, and Run forkz Command. Saved sites, synclets, smart folders, and workspaces can be picked from a list; commands that delete or move items are not offered.
- `forkz://` links for scripts, launchers, and web pages: open, connect, search, synclet, smart-folder, workspace, and command (see the README). A link can also be given on the command line.

## [0.13.0] - 2026-11-06

### Added

- Flat View (View menu, Option-Command-F): every file in the pane's folder and the folders in it, in one list with a column for the folder each is in. Again goes back to the folder.
- Go > Recent Folders: the last 15 folders the panes showed, on the Mac and on saved sites, with Clear Menu. They are in the command palette too.
- Find Duplicates (Commands menu): files with the same contents in a folder on the Mac and everything in it, in groups, the largest waste of space first. Mark copies by hand, or all but the oldest or newest of each group, and move them to the Trash; one copy of each file is always kept.
- Moving an app to the Trash offers to move its leftover files there too: settings, caches, containers, saved state, and support files in your Library folder, each with a check box (Settings > General can turn this off). Apple's apps are left alone.
- Zip, tar (plain, gzip, bzip2, or xz), and 7z archives on the Mac can be changed in place: copy or drag items in, rename and delete items, and make folders. A job's changes are written into the archive once, when it is done.

## [0.12.2] - 2026-11-06

### Changed

- The preview pane can be made wider: up to half the window (it was at most 520 points).

## [0.12.1] - 2026-11-05

### Fixed

- In icon view (and column view), changing or clearing the filter did not update the items shown while the selected item still matched; the clear button of the filter field also clears the filter right away.

## [0.12.0] - 2026-11-05

### Added

- A built-in compare tool for Compare Files (Control-Command-C), the default for new installs (choose "forkz (built in)" in Settings > General to switch from another tool): two files side by side, line by line, with changed lines and the words that changed marked, removed and added lines, Next and Previous Change (Command-Down and Command-Up Arrow), and options to ignore whitespace and case; binary files are compared byte by byte. Two folders are compared with everything in them, showing what only one side has, what differs in size, date, or contents, and what is the same; double-click two files to compare them. Works on the Mac, on servers, and in archives.
- Compare Panes (Commands menu): selects, in each pane, the items that differ from the other pane (items the other does not have, newer files, and files of the same date but a different size).
- Smart folders: a search on the Mac can be saved in the sidebar, and clicking it searches again. Right-click one to change its search, rename it, or delete it. They are in the command palette too.

### Fixed

- Icon view jumped back to the top after a drop onto a folder that was not selected (or any other change in the folder); it now stays where it was scrolled.

## [0.11.1] - 2026-11-05

### Changed

- Column view columns start wider (300 points instead of 220), and the widths you set are kept between launches.

### Fixed

- Dropping items onto a folder icon (or into the empty space) in icon view did nothing: the icon pictures took the drag for themselves and refused it.
- Column view jumped back to the top of the list after a drop or any other change in the folder; it now keeps each column where it was scrolled.
- Names too long for a narrow column view column were not shown at all; they are now shortened in the middle.
- In a narrow pane, column view scrolled the current folder's column mostly out of sight to show the next one; the current column now stays in view.

## [0.11.0] - 2026-11-05

### Added

- Icon view (View > as Icons, Command-2) and column view (View > as Columns, Command-3; as List is Command-1), as in Finder: large icons that grow with zoom, or a pane's folders side by side, browsed with the mouse or the arrow keys. Commands, Quick Look, the preview pane, and drag and drop (out to other panes and Finder, and onto folders) work as in the list.
- Group By (View > Group By): the list grouped by kind, date modified (Today, Yesterday, Previous 7 Days, Previous 30 Days, months, and years), or size, keeping the sort within each group.
- Stack Panes (View menu): the two panes one above the other instead of side by side.
- A Git column (View > Columns > Git): in a repository, whether each item is modified, added, untracked, renamed, ignored, or in conflict; folders show the most important change inside them.
- A viewer (View, F3): a file as text (UTF-8, UTF-16, or Latin-1, with search and optional wrapping) or as hex, in its own window; on the Mac, on servers, and in archives.
- Filename filters (Settings > Filters, View > Use Filename Filters): named sets of patterns that hide items in the lists, leave them out of copies, or both, with a few to start from.

### Fixed

- Command-W in another window (a viewer, Settings) closed the main window; it closes that window.
- The Git column and other optional columns now show from launch (they needed a refresh before).
- Removing a folder from the sidebar (Remove from Sidebar) no longer makes forkz quit.

## [0.10.1] - 2026-11-05

### Fixed

- The setting in Settings > General could not be turned on: macOS does not let apps replace Finder as the app that opens folders. It is now "Use forkz for “Show in Finder” in other apps", which works: files that other apps show in Finder open selected in forkz.

### Changed

- Updated the checksum and signing libraries (sha2 0.11, hmac 0.13, and md-5 0.11).

## [0.10.0] - 2026-11-05

### Added

- Color themes (Settings > Appearance): built-in themes in the manner of a code editor (GitHub Light, Solarized Light and Dark, Catppuccin Latte and Mocha, Dracula, Nord, One Dark, Monokai, Gruvbox Dark, and Tokyo Night) for the file lists and the sidebar, alongside macOS's own colors. Customize… makes a theme of your own with a color well for each color; themes are TOML files in the themes folder, which Themes Folder opens.
- The transfer queue can be managed: right-click a transfer to start it now, move it to the top, up, down, or to the bottom, pause it, or stop it. With several transfers, a line above them shows how many are waiting, with Pause All (or Resume All) and Stop All. Waiting transfers show their place in line.
- forkz can be the default folder viewer instead of Finder (Settings > General): folders opened from other apps, and "Show in Finder", open in forkz, with the file selected.
- Rename presets appear in the sidebar: drop items on a preset to rename them with it, or click it to rename the selection (Rename Multiple Items opens with its rules, to check first).
- FXP: copies and moves between two FTP servers go directly from one server to the other when both allow it, instead of through your Mac. forkz checks once per pair of servers and copies through the Mac as before when they refuse; Stop works as usual. Encrypted connections need a server with SSCN, and FXP is not used when only one connection is encrypted.

## [0.9.2] - 2026-11-05

### Added

- Tabs show a close button when the mouse is over them.

### Changed

- The active pane (where commands, connections, and Go to Folder open) has a thin line in your accent color along its top.

## [0.9.1] - 2026-11-05

### Fixed

- Select All (Command-A) no longer selects "..", and Shift-click or Command-click leaves it out too, so a selection is only items. Clicking ".." on its own still selects it.

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

[Unreleased]: https://github.com/houseofmackee/forkz/compare/v0.27.0...HEAD
[0.27.0]: https://github.com/houseofmackee/forkz/compare/v0.26.0...v0.27.0
[0.26.0]: https://github.com/houseofmackee/forkz/compare/v0.25.0...v0.26.0
[0.25.0]: https://github.com/houseofmackee/forkz/compare/v0.24.2...v0.25.0
[0.24.2]: https://github.com/houseofmackee/forkz/compare/v0.24.1...v0.24.2
[0.24.1]: https://github.com/houseofmackee/forkz/compare/v0.24.0...v0.24.1
[0.24.0]: https://github.com/houseofmackee/forkz/compare/v0.23.1...v0.24.0
[0.23.1]: https://github.com/houseofmackee/forkz/compare/v0.23.0...v0.23.1
[0.23.0]: https://github.com/houseofmackee/forkz/compare/v0.22.0...v0.23.0
[0.22.0]: https://github.com/houseofmackee/forkz/compare/v0.21.1...v0.22.0
[0.21.1]: https://github.com/houseofmackee/forkz/compare/v0.21.0...v0.21.1
[0.21.0]: https://github.com/houseofmackee/forkz/compare/v0.20.2...v0.21.0
[0.20.2]: https://github.com/houseofmackee/forkz/compare/v0.20.1...v0.20.2
[0.20.1]: https://github.com/houseofmackee/forkz/compare/v0.20.0...v0.20.1
[0.20.0]: https://github.com/houseofmackee/forkz/compare/v0.19.0...v0.20.0
[0.19.0]: https://github.com/houseofmackee/forkz/compare/v0.18.3...v0.19.0
[0.18.3]: https://github.com/houseofmackee/forkz/compare/v0.18.2...v0.18.3
[0.18.2]: https://github.com/houseofmackee/forkz/compare/v0.18.1...v0.18.2
[0.18.1]: https://github.com/houseofmackee/forkz/compare/v0.18.0...v0.18.1
[0.18.0]: https://github.com/houseofmackee/forkz/compare/v0.17.0...v0.18.0
[0.17.0]: https://github.com/houseofmackee/forkz/compare/v0.16.4...v0.17.0
[0.16.4]: https://github.com/houseofmackee/forkz/compare/v0.16.3...v0.16.4
[0.16.3]: https://github.com/houseofmackee/forkz/compare/v0.16.2...v0.16.3
[0.16.2]: https://github.com/houseofmackee/forkz/compare/v0.16.1...v0.16.2
[0.16.1]: https://github.com/houseofmackee/forkz/compare/v0.16.0...v0.16.1
[0.16.0]: https://github.com/houseofmackee/forkz/compare/v0.15.2...v0.16.0
[0.15.2]: https://github.com/houseofmackee/forkz/compare/v0.15.1...v0.15.2
[0.15.1]: https://github.com/houseofmackee/forkz/compare/v0.15.0...v0.15.1
[0.15.0]: https://github.com/houseofmackee/forkz/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/houseofmackee/forkz/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/houseofmackee/forkz/compare/v0.12.2...v0.13.0
[0.12.2]: https://github.com/houseofmackee/forkz/compare/v0.12.1...v0.12.2
[0.12.1]: https://github.com/houseofmackee/forkz/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/houseofmackee/forkz/compare/v0.11.1...v0.12.0
[0.11.1]: https://github.com/houseofmackee/forkz/compare/v0.11.0...v0.11.1
[0.11.0]: https://github.com/houseofmackee/forkz/compare/v0.10.1...v0.11.0
[0.10.1]: https://github.com/houseofmackee/forkz/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/houseofmackee/forkz/compare/v0.9.2...v0.10.0
[0.9.2]: https://github.com/houseofmackee/forkz/compare/v0.9.1...v0.9.2
[0.9.1]: https://github.com/houseofmackee/forkz/compare/v0.9.0...v0.9.1
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
