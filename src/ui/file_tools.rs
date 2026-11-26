//! File operations beyond copy and move: links and aliases, setting dates
//! (touch), flattening folders, quick image actions (convert, resize,
//! rotate, make a PDF), splitting and joining large files, and copying to
//! several folders at once. Slow work runs in the background, with a note
//! in the activity area; what can be undone is recorded for Edit > Undo.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command as Process;
use std::time::SystemTime;

use dispatch2::DispatchQueue;

use super::commands::Command;
use super::pane_view::PaneController;
use super::{app, dialogs, oplog, undo};
use crate::fs::ops::{JobKind, free_name};
use crate::fs::split;
use crate::pane::title_of;
use crate::vfs::{self, FsRef};

/// Images sips reads.
const IMAGES: &[&str] = &[
    "jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "gif", "bmp", "webp", "jp2", "psd", "tga", "dng", "cr2",
    "cr3", "nef", "arw", "raf", "orf", "rw2",
];

fn is_image(path: &Path) -> bool {
    path.extension().is_some_and(|e| IMAGES.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// Whether a command can run on a pane now.
pub fn is_enabled(command: Command, pane: &PaneController, has_selection: bool) -> bool {
    let fs = pane.fs();
    let writable = !fs.read_only();
    match command {
        Command::MakeSymlink | Command::MakeAlias | Command::MakeHardLink => has_selection && pane.is_local(),
        Command::ConvertImages
        | Command::ResizeImages
        | Command::RotateLeft
        | Command::RotateRight
        | Command::MakePdf => has_selection && pane.is_local() && pane.target_paths().iter().any(|p| is_image(p)),
        Command::SetDate | Command::FlattenFolder | Command::SplitFile => has_selection && writable,
        Command::JoinFiles => {
            has_selection && writable && pane.target_paths().iter().any(|p| split::is_part(&title_of(p)))
        }
        Command::CopyToSeveral => has_selection,
        _ => true,
    }
}

pub fn run(command: Command, pane: &PaneController) {
    match command {
        Command::MakeSymlink => make_links(pane, LinkKind::Symbolic),
        Command::MakeAlias => make_links(pane, LinkKind::Alias),
        Command::MakeHardLink => make_links(pane, LinkKind::Hard),
        Command::SetDate => set_date(pane),
        Command::FlattenFolder => flatten(pane),
        Command::ConvertImages => convert_images(pane),
        Command::ResizeImages => resize_images(pane),
        Command::RotateLeft => rotate_images(pane, 270),
        Command::RotateRight => rotate_images(pane, 90),
        Command::MakePdf => make_pdf(pane),
        Command::SplitFile => split_files(pane),
        Command::JoinFiles => join_files(pane),
        Command::CopyToSeveral => copy_to_several(pane),
        _ => {}
    }
}

/// Runs `work` off the main thread with a note in the activity area, then
/// `done` with its result on the main thread.
fn in_background<T: Send + 'static>(
    note: &str,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + Send + 'static,
) {
    let Some(id) = app().map(|a| a.activity.begin_note(note)) else { return };
    std::thread::spawn(move || {
        let result = work();
        DispatchQueue::main().exec_async(move || {
            if let Some(app) = app() {
                app.activity.remove(id);
            }
            done(result);
        });
    });
}

/// The panes showing a folder list it again (selecting `select` in the
/// one it was done in).
fn show_changes(index: usize, folder: &Path, select: &[PathBuf]) {
    let Some(app) = app() else { return };
    for (i, pane) in app.panes.iter().enumerate() {
        if i == index {
            pane.reload_selecting(select);
        } else if pane.location() == folder {
            pane.reload();
        }
    }
}

/// Logs what was made, and shows what failed.
fn report(done: String, failed: &[String], failure: &str) {
    if !done.is_empty() {
        oplog::record(oplog::Kind::Change, done, failed.join("\n"));
    }
    if !failed.is_empty() {
        oplog::record(oplog::Kind::Problem, failure.to_string(), failed.join("\n"));
        dialogs::show_error(failure.to_string(), failed.join("\n"));
    }
}

/// "“name”" or "3 items".
fn what(paths: &[PathBuf]) -> String {
    match paths {
        [one] => format!("\u{201c}{}\u{201d}", title_of(one)),
        many => format!("{} items", many.len()),
    }
}

// MARK: Links

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkKind {
    Symbolic,
    Alias,
    Hard,
}

/// "report link.pdf" (keeping the extension, so it opens the same way),
/// or for an alias "report.pdf alias" (as Finder names them).
fn link_name(name: &str, kind: LinkKind, folder: bool) -> String {
    let suffix = match kind {
        LinkKind::Symbolic => "link",
        LinkKind::Alias => return format!("{name} alias"),
        LinkKind::Hard => "hard link",
    };
    match Path::new(name).extension().filter(|_| !folder) {
        Some(extension) => {
            let extension = extension.to_string_lossy();
            format!("{} {suffix}.{extension}", &name[..name.len() - extension.len() - 1])
        }
        None => format!("{name} {suffix}"),
    }
}

fn make_alias(target: &Path, alias: &Path) -> io::Result<()> {
    use objc2_foundation::{NSURL, NSURLBookmarkCreationOptions};
    let url = super::util::file_url(target);
    let data = url
        .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
            NSURLBookmarkCreationOptions::SuitableForBookmarkFile,
            None,
            None,
        )
        .map_err(|e| io::Error::other(e.localizedDescription().to_string()))?;
    NSURL::writeBookmarkData_toURL_options_error(&data, &super::util::file_url(alias), 0)
        .map_err(|e| io::Error::other(e.localizedDescription().to_string()))
}

/// Links to the selected items: in the other pane's folder (with the same
/// names) when it shows another folder on this Mac, else beside them.
fn make_links(pane: &PaneController, kind: LinkKind) {
    let Some(app) = app() else { return };
    let entries = pane.target_entries();
    if entries.is_empty() {
        return;
    }
    let other = &app.panes[1 - pane.index()];
    let elsewhere =
        (other.is_local() && !other.showing_results() && other.location() != pane.location()).then(|| other.location());
    let local = vfs::local();
    let mut made = Vec::new();
    let mut failed = Vec::new();
    for entry in &entries {
        let folder = entry.is_folder_like();
        if kind == LinkKind::Hard && folder {
            failed.push(format!("{}: folders cannot have hard links", entry.name));
            continue;
        }
        let wanted = match &elsewhere {
            Some(place) => place.join(&entry.name),
            None => entry.path.with_file_name(link_name(&entry.name, kind, folder)),
        };
        let link = free_name(&local, &wanted);
        let result = match kind {
            LinkKind::Symbolic => std::os::unix::fs::symlink(&entry.path, &link),
            LinkKind::Hard => std::fs::hard_link(&entry.path, &link),
            LinkKind::Alias => make_alias(&entry.path, &link),
        };
        match result {
            Ok(()) => made.push(link),
            Err(e) => failed.push(format!("{}: {e}", entry.name)),
        }
    }
    let name = match kind {
        LinkKind::Symbolic => "Make Symbolic Link",
        LinkKind::Alias => "Make Alias",
        LinkKind::Hard => "Make Hard Link",
    };
    undo::record(name, undo::Action::Trash { paths: made.clone() });
    let kind_name = match kind {
        LinkKind::Symbolic => "symbolic links",
        LinkKind::Alias => "aliases",
        LinkKind::Hard => "hard links",
    };
    let done = if made.is_empty() { String::new() } else { format!("Made {kind_name} to {}", what(&made)) };
    report(done, &failed, &format!("Some {kind_name} could not be made."));
    if elsewhere.is_some() { other.reload_selecting(&made) } else { pane.reload_selecting(&made) }
}

// MARK: Dates

/// File > Set Date: the modification date of the selected items (now, or
/// a date typed), as `touch` sets it.
fn set_date(pane: &PaneController) {
    let paths = pane.target_paths();
    if paths.is_empty() {
        return;
    }
    let now = crate::rename::format_date("YYYY-MM-DD hh:mm:ss", SystemTime::now());
    let Some(text) = dialogs::prompt(
        &format!("Set the Modification Date of {}", what(&paths)),
        "The date and time (YYYY-MM-DD hh:mm:ss, in local time). It starts as now.",
        "Set Date",
        &now,
        None,
    ) else {
        return;
    };
    let mut text = text.trim().to_string();
    match text.len() {
        10 => text.push_str(" 00:00:00"),
        16 => text.push_str(":00"),
        _ => {}
    }
    let Some(date) = crate::fs::taken::parse_exif(&text) else {
        dialogs::show_error(
            format!("\u{201c}{text}\u{201d} is not a date."),
            "Write it as 2026-11-25 14:30:00.".into(),
        );
        return;
    };
    let fs = pane.fs();
    let fs = fs.browse_fs().unwrap_or(fs);
    let index = pane.index();
    let folder = pane.location();
    let worker = fs.clone();
    in_background(
        "Setting dates\u{2026}",
        move || set_dates(&worker, paths.iter().map(|p| (p.clone(), date)).collect()),
        move |(before, failed)| {
            let changed: Vec<PathBuf> = before.iter().map(|(p, _)| p.clone()).collect();
            undo::record("Set Date", undo::Action::Dates { fs, items: before });
            let done = if changed.is_empty() { String::new() } else { format!("Set the date of {}", what(&changed)) };
            report(done, &failed, "Some dates could not be set.");
            show_changes(index, &folder, &changed);
        },
    );
}

/// Sets modification dates; returns the dates they had (for undo) and
/// what failed.
pub fn set_dates(fs: &FsRef, items: Vec<(PathBuf, SystemTime)>) -> (Vec<(PathBuf, SystemTime)>, Vec<String>) {
    let mut before = Vec::new();
    let mut failed = Vec::new();
    for (path, date) in items {
        let old = fs.stat(&path).ok().and_then(|e| e.modified);
        match fs.set_attributes(&path, Some(date), None) {
            Ok(()) => {
                if let Some(old) = old {
                    before.push((path, old));
                }
            }
            Err(e) => failed.push(format!("{}: {e}", title_of(&path))),
        }
    }
    (before, failed)
}

// MARK: Flatten

/// The files below a folder, at any depth, and the folders under it
/// (deepest last).
fn walk(fs: &FsRef, folder: &Path, files: &mut Vec<PathBuf>, folders: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs.list(folder)? {
        if entry.is_folder_like() && !entry.package {
            folders.push(entry.path.clone());
            walk(fs, &entry.path, files, folders)?;
        } else {
            files.push(entry.path);
        }
    }
    Ok(())
}

/// What flattening a folder did: (moved to, from) pairs, and problems.
pub fn flatten_folder(fs: &FsRef, folder: &Path) -> (Vec<(PathBuf, PathBuf)>, Vec<String>) {
    let mut moved = Vec::new();
    let mut failed = Vec::new();
    let mut folders = Vec::new();
    let mut files = Vec::new();
    for entry in fs.list(folder).unwrap_or_default() {
        if entry.is_folder_like() && !entry.package {
            folders.push(entry.path.clone());
            if let Err(e) = walk(fs, &entry.path, &mut files, &mut folders) {
                failed.push(format!("{}: {e}", entry.name));
            }
        }
    }
    for file in files {
        let target = free_name(fs, &folder.join(title_of(&file)));
        match fs.rename(&file, &target) {
            Ok(()) => moved.push((target, file)),
            Err(e) => failed.push(format!("{}: {e}", title_of(&file))),
        }
    }
    // The emptied folders go, deepest first.
    for sub in folders.iter().rev() {
        if fs.list(sub).is_ok_and(|items| items.is_empty()) {
            let _ = fs.remove_dir(sub);
        }
    }
    (moved, failed)
}

/// File > Flatten Folder: moves everything in a folder's subfolders up
/// into it, and removes the emptied subfolders.
fn flatten(pane: &PaneController) {
    let folders: Vec<PathBuf> =
        pane.target_entries().into_iter().filter(|e| e.is_folder_like() && !e.package).map(|e| e.path).collect();
    if folders.is_empty() {
        dialogs::show_info("Select a folder to flatten.".into(), String::new());
        return;
    }
    if !dialogs::confirm(
        &format!("Flatten {}?", what(&folders)),
        "The files in its subfolders (at any depth) move up into it; names that are taken get a number. The emptied subfolders are removed.",
        "Flatten",
        false,
    ) {
        return;
    }
    let fs = pane.fs();
    let fs = fs.browse_fs().unwrap_or(fs);
    let index = pane.index();
    let shown = folders.clone();
    let worker = fs.clone();
    in_background(
        "Flattening\u{2026}",
        move || {
            let mut moved = Vec::new();
            let mut failed = Vec::new();
            for folder in &folders {
                let (m, f) = flatten_folder(&worker, folder);
                moved.extend(m);
                failed.extend(f);
            }
            (moved, failed)
        },
        move |(moved, failed)| {
            let count = moved.len();
            undo::record("Flatten Folder", undo::Action::Move { fs, pairs: moved });
            let done = format!("Flattened {} ({count} files moved)", what(&shown));
            report(done, &failed, "Some files could not be moved.");
            if let Some(app) = app() {
                for (i, pane) in app.panes.iter().enumerate() {
                    if i == index || shown.iter().any(|f| pane.location().starts_with(f)) {
                        pane.reload();
                    }
                }
            }
        },
    );
}

// MARK: Images

/// Runs sips; its complaint is the error.
fn sips(args: &[&std::ffi::OsStr]) -> io::Result<()> {
    let output = Process::new("/usr/bin/sips").args(args).output()?;
    if output.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&output.stderr);
    let message = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("sips could not do it.").trim().to_string();
    Err(io::Error::other(message))
}

fn os(text: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(text)
}

/// The size of an image in pixels.
fn pixel_size(path: &Path) -> Option<(u32, u32)> {
    let output =
        Process::new("/usr/bin/sips").args(["-g", "pixelWidth", "-g", "pixelHeight"]).arg(path).output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let value = |key: &str| {
        text.lines()
            .find_map(|l| l.trim().strip_prefix(key).and_then(|v| v.trim_start_matches(':').trim().parse().ok()))
    };
    Some((value("pixelWidth")?, value("pixelHeight")?))
}

fn selected_images(pane: &PaneController) -> Vec<PathBuf> {
    pane.target_entries().into_iter().filter(|e| !e.is_folder_like() && is_image(&e.path)).map(|e| e.path).collect()
}

/// Makes new images beside the selected ones, each with `make(from, to)`,
/// then selects them (and records undo).
fn make_images(
    pane: &PaneController,
    note: &str,
    undo_name: &'static str,
    target: impl Fn(&Path) -> Option<PathBuf> + Send + 'static,
    make: impl Fn(&Path, &Path) -> io::Result<()> + Send + 'static,
) {
    let images = selected_images(pane);
    if images.is_empty() {
        return;
    }
    let index = pane.index();
    let folder = pane.location();
    let local = vfs::local();
    in_background(
        note,
        move || {
            let mut made = Vec::new();
            let mut failed = Vec::new();
            for image in &images {
                let Some(wanted) = target(image) else { continue };
                let out = free_name(&local, &wanted);
                match make(image, &out) {
                    Ok(()) => made.push(out),
                    Err(e) => failed.push(format!("{}: {e}", title_of(image))),
                }
            }
            (made, failed)
        },
        move |(made, failed): (Vec<PathBuf>, Vec<String>)| {
            undo::record(undo_name, undo::Action::Trash { paths: made.clone() });
            let done = if made.is_empty() { String::new() } else { format!("Made {}", what(&made)) };
            report(done, &failed, "Some images could not be made.");
            show_changes(index, &folder, &made);
        },
    );
}

/// The formats images convert to: (menu name, sips format, extension).
const FORMATS: [(&str, &str, &str); 6] = [
    ("JPEG", "jpeg", "jpg"),
    ("PNG", "png", "png"),
    ("HEIC", "heic", "heic"),
    ("TIFF", "tiff", "tiff"),
    ("GIF", "gif", "gif"),
    ("BMP", "bmp", "bmp"),
];

/// File > Images > Convert Images: copies in another format, beside them.
fn convert_images(pane: &PaneController) {
    let names: Vec<&str> = FORMATS.iter().map(|(name, _, _)| *name).collect();
    let Some(choice) = dialogs::choose(
        "Convert Images",
        "Copies of the selected images in another format are made beside them.",
        "Convert",
        &names,
        0,
    ) else {
        return;
    };
    let (name, format, extension) = FORMATS[choice];
    make_images(
        pane,
        &format!("Converting to {name}\u{2026}"),
        "Convert Images",
        move |image| Some(image.with_extension(extension)),
        move |from, to| sips(&[os("-s"), os("format"), os(format), from.as_os_str(), os("--out"), to.as_os_str()]),
    );
}

/// File > Images > Resize Images: smaller copies beside them ("photo
/// 1280px.jpg"); images already that small are left out.
fn resize_images(pane: &PaneController) {
    const SIZES: [u32; 5] = [640, 1024, 1280, 1920, 3840];
    let names: Vec<String> = SIZES.iter().map(|s| format!("{s} pixels")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let Some(choice) = dialogs::choose(
        "Resize Images",
        "Smaller copies are made beside the selected images, at most this wide and high (keeping their shape).",
        "Resize",
        &names,
        2,
    ) else {
        return;
    };
    let size = SIZES[choice];
    make_images(
        pane,
        "Resizing images\u{2026}",
        "Resize Images",
        move |image| {
            if pixel_size(image).is_some_and(|(w, h)| w.max(h) <= size) {
                return None;
            }
            let stem = image.file_stem()?.to_string_lossy().into_owned();
            let name = match image.extension() {
                Some(e) => format!("{stem} {size}px.{}", e.to_string_lossy()),
                None => format!("{stem} {size}px"),
            };
            Some(image.with_file_name(name))
        },
        move |from, to| sips(&[os("-Z"), os(&size.to_string()), from.as_os_str(), os("--out"), to.as_os_str()]),
    );
}

/// Rotates images in place (`degrees` clockwise).
pub fn rotate(paths: &[PathBuf], degrees: u32) -> (Vec<PathBuf>, Vec<String>) {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    for path in paths {
        match sips(&[os("-r"), os(&degrees.to_string()), path.as_os_str()]) {
            Ok(()) => done.push(path.clone()),
            Err(e) => failed.push(format!("{}: {e}", title_of(path))),
        }
    }
    (done, failed)
}

/// File > Images > Rotate Left or Right: in place, as Finder does.
fn rotate_images(pane: &PaneController, degrees: u32) {
    let images = selected_images(pane);
    if images.is_empty() {
        return;
    }
    let index = pane.index();
    let folder = pane.location();
    let name = if degrees == 90 { "Rotate Right" } else { "Rotate Left" };
    in_background(
        "Rotating images\u{2026}",
        move || rotate(&images, degrees),
        move |(done, failed)| {
            undo::record(name, undo::Action::Rotate { paths: done.clone(), degrees: 360 - degrees });
            let text = if done.is_empty() { String::new() } else { format!("Rotated {}", what(&done)) };
            report(text, &failed, "Some images could not be rotated.");
            show_changes(index, &folder, &done);
        },
    );
}

/// File > Images > Make PDF: one PDF of the selected images, a page each
/// (in name order).
fn make_pdf(pane: &PaneController) {
    let mut images = selected_images(pane);
    if images.is_empty() {
        return;
    }
    images.sort_by_key(|p| title_of(p).to_lowercase());
    let initial = match images.as_slice() {
        [one] => format!("{}.pdf", one.file_stem().unwrap_or_default().to_string_lossy()),
        _ => format!("{}.pdf", title_of(&pane.location())),
    };
    let Some(name) = dialogs::prompt(
        "Make PDF",
        &format!("A PDF of {}, a page each, in name order. Its name:", what(&images)),
        "Make PDF",
        &initial,
        Some(initial.len() - 4),
    ) else {
        return;
    };
    let mut name = name.trim().to_string();
    if !name.to_lowercase().ends_with(".pdf") {
        name.push_str(".pdf");
    }
    let target = free_name(&vfs::local(), &pane.location().join(name));
    // PDFKit and NSImage on the main thread (quick for a few images).
    let result = write_pdf(&images, &target);
    match result {
        Ok(()) => {
            undo::record("Make PDF", undo::Action::Trash { paths: vec![target.clone()] });
            report(format!("Made \u{201c}{}\u{201d} of {}", title_of(&target), what(&images)), &[], "");
            show_changes(pane.index(), &pane.location(), std::slice::from_ref(&target));
        }
        Err(e) => dialogs::show_error("The PDF could not be made.".into(), e.to_string()),
    }
}

fn write_pdf(images: &[PathBuf], target: &Path) -> io::Result<()> {
    use objc2::AllocAnyThread;
    use objc2_app_kit::NSImage;
    use objc2_pdf_kit::{PDFDocument, PDFPage};
    let document = unsafe { PDFDocument::new() };
    for image_path in images {
        let image = NSImage::initWithContentsOfURL(NSImage::alloc(), &super::util::file_url(image_path))
            .ok_or_else(|| io::Error::other(format!("{} could not be read.", title_of(image_path))))?;
        let page = unsafe { PDFPage::initWithImage(PDFPage::alloc(), &image) }
            .ok_or_else(|| io::Error::other(format!("{} could not be made a page.", title_of(image_path))))?;
        unsafe { document.insertPage_atIndex(&page, document.pageCount()) };
    }
    if unsafe { document.writeToURL(&super::util::file_url(target)) } {
        Ok(())
    } else {
        Err(io::Error::other("PDFKit could not write the file."))
    }
}

// MARK: Split and join

/// File > Split File: the selected files in parts of a chosen size
/// ("movie.mkv.001", ".002", and so on), beside them.
fn split_files(pane: &PaneController) {
    let files: Vec<crate::fs::Entry> = pane.target_entries().into_iter().filter(|e| !e.is_folder_like()).collect();
    if files.is_empty() {
        return;
    }
    let Some(text) = dialogs::prompt(
        &format!("Split {}", what(&files.iter().map(|e| e.path.clone()).collect::<Vec<_>>())),
        "The size of each part: 25 MB for email, 100 MB, 700 MB for a CD, 4 GB for a FAT32 drive, or any size. Join Files puts them together again (as do cat and 7-Zip).",
        "Split",
        "100 MB",
        None,
    ) else {
        return;
    };
    let Some(part) = split::parse_size(&text) else {
        dialogs::show_error(
            format!("\u{201c}{}\u{201d} is not a size.", text.trim()),
            "Write it as 100 MB or 2 GB.".into(),
        );
        return;
    };
    let fs = pane.fs();
    let fs = fs.browse_fs().unwrap_or(fs);
    let index = pane.index();
    let folder = pane.location();
    let worker = fs.clone();
    in_background(
        "Splitting\u{2026}",
        move || {
            let mut made = Vec::new();
            let mut failed = Vec::new();
            for file in &files {
                match split::split(&worker, &file.path, file.size, part, &mut |_| {}) {
                    Ok(parts) => made.extend(parts),
                    Err(e) => failed.push(format!("{}: {e}", file.name)),
                }
            }
            (made, failed)
        },
        move |(made, failed)| {
            if fs.is_local() {
                undo::record("Split File", undo::Action::Trash { paths: made.clone() });
            }
            let done = if made.is_empty() { String::new() } else { format!("Split into {} parts", made.len()) };
            report(done, &failed, "Some files could not be split.");
            show_changes(index, &folder, &made);
        },
    );
}

/// File > Join Files: the parts a selected part belongs to, joined into
/// the file they were split from (beside them).
fn join_files(pane: &PaneController) {
    let fs = pane.fs();
    let fs = fs.browse_fs().unwrap_or(fs);
    // The first part of each set selected (any of its parts may be).
    let mut firsts: Vec<PathBuf> = Vec::new();
    for path in pane.target_paths() {
        let name = title_of(&path);
        if !split::is_part(&name) {
            continue;
        }
        let Some((stem, number)) = name.rsplit_once('.') else { continue };
        let first = path.with_file_name(format!("{stem}.{:0width$}", 1, width = number.len()));
        if !firsts.contains(&first) {
            firsts.push(first);
        }
    }
    if firsts.is_empty() {
        return;
    }
    let index = pane.index();
    let folder = pane.location();
    let worker = fs.clone();
    in_background(
        "Joining\u{2026}",
        move || {
            let mut made = Vec::new();
            let mut failed = Vec::new();
            for first in &firsts {
                let parts = split::parts_from(&worker, first);
                let Some(joined) = split::joined_name(&title_of(first)) else { continue };
                if parts.is_empty() {
                    failed.push(format!("{}: the first part is missing", title_of(first)));
                    continue;
                }
                let target = first.with_file_name(joined);
                match split::join(&worker, &parts, &target, &mut |_| {}) {
                    Ok(()) => made.push(target),
                    Err(e) => failed.push(format!("{}: {e}", title_of(&target))),
                }
            }
            (made, failed)
        },
        move |(made, failed)| {
            if fs.is_local() {
                undo::record("Join Files", undo::Action::Trash { paths: made.clone() });
            }
            let done = if made.is_empty() { String::new() } else { format!("Joined {}", what(&made)) };
            report(done, &failed, "Some files could not be joined.");
            show_changes(index, &folder, &made);
        },
    );
}

// MARK: Copy to several folders

/// "~/Documents" for a folder in the home folder.
fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// Commands > Copy to Several Folders: the selection copied to each folder
/// chosen (the other pane's, sidebar favorites, recent folders, or others
/// picked), a job each.
fn copy_to_several(pane: &PaneController) {
    use super::dialogs::Choice;
    let Some(app) = app() else { return };
    let sources = pane.target_paths();
    if sources.is_empty() {
        return;
    }
    let here = pane.is_local().then(|| pane.location());
    let mut seen: Vec<PathBuf> = here.iter().cloned().collect();
    let mut take = |path: &Path| {
        if seen.iter().any(|p| p == path) || !path.is_dir() {
            return None;
        }
        seen.push(path.to_path_buf());
        Some(Choice { path: path.to_path_buf(), label: tilde(path), checked: false })
    };
    let other = &app.panes[1 - pane.index()];
    let panes: Vec<Choice> = if other.is_local() && !other.showing_results() {
        take(&other.location()).into_iter().collect()
    } else {
        Vec::new()
    };
    let settings = app.settings.borrow();
    let favorites: Vec<Choice> = settings.favorites.iter().filter_map(|p| take(p)).collect();
    let recent: Vec<Choice> =
        settings.recent_folders.iter().filter(|r| r.site.is_none()).filter_map(|r| take(&r.path)).take(10).collect();
    drop(settings);
    const OTHERS: &str = "\u{0}others";
    let others = vec![Choice {
        path: PathBuf::from(OTHERS),
        label: "Other folders (chosen next)\u{2026}".into(),
        checked: false,
    }];
    let groups = vec![
        ("Other Pane".to_string(), panes),
        ("Favorites".to_string(), favorites),
        ("Recent Folders".to_string(), recent),
        (String::new(), others),
    ];
    let Some(mut chosen) = dialogs::choose_items(
        &format!("Copy {} to Several Folders", what(&sources)),
        "A copy goes to each folder checked.",
        "Copy",
        "Cancel",
        &groups,
    ) else {
        return;
    };
    if let Some(at) = chosen.iter().position(|p| p.as_os_str() == OTHERS) {
        chosen.remove(at);
        let panel = objc2_app_kit::NSOpenPanel::openPanel(super::util::mtm());
        panel.setCanChooseFiles(false);
        panel.setCanChooseDirectories(true);
        panel.setCanCreateDirectories(true);
        panel.setAllowsMultipleSelection(true);
        panel.setMessage(Some(&super::util::ns("Choose folders to copy to (Command-click for several).")));
        panel.setPrompt(Some(&super::util::ns("Copy")));
        if panel.runModal() == objc2_app_kit::NSModalResponseOK {
            chosen.extend(panel.URLs().iter().filter_map(|u| super::util::url_path(&u)));
        }
    }
    chosen.dedup();
    for destination in &chosen {
        super::start_job(super::transfer_job(
            JobKind::Copy,
            pane.fs(),
            sources.clone(),
            vfs::local(),
            destination.clone(),
        ));
    }
    if !chosen.is_empty() {
        oplog::record(oplog::Kind::Change, format!("Copying {} to {} folders", what(&sources), chosen.len()), "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_links() {
        assert_eq!(link_name("report.pdf", LinkKind::Symbolic, false), "report link.pdf");
        assert_eq!(link_name("report.pdf", LinkKind::Hard, false), "report hard link.pdf");
        assert_eq!(link_name("report.pdf", LinkKind::Alias, false), "report.pdf alias");
        assert_eq!(link_name("Photos", LinkKind::Symbolic, true), "Photos link");
        assert_eq!(link_name("v1.2", LinkKind::Symbolic, true), "v1.2 link");
    }

    #[test]
    fn flattens_and_sets_dates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("top");
        std::fs::create_dir_all(root.join("a/deeper")).unwrap();
        std::fs::create_dir_all(root.join("b")).unwrap();
        std::fs::write(root.join("keep.txt"), "k").unwrap();
        std::fs::write(root.join("a/one.txt"), "1").unwrap();
        std::fs::write(root.join("a/deeper/two.txt"), "2").unwrap();
        std::fs::write(root.join("b/one.txt"), "other one").unwrap();
        let fs = vfs::local();
        let (moved, failed) = flatten_folder(&fs, &root);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(moved.len(), 3);
        let mut names: Vec<String> =
            std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["keep.txt", "one 2.txt", "one.txt", "two.txt"]);

        // Dates: set, and what they were is returned (for undo); folders too.
        let date = crate::fs::taken::parse_exif("2001-02-03 04:05:06").unwrap();
        let (before, failed) = set_dates(&fs, vec![(root.join("keep.txt"), date), (root.clone(), date)]);
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(before.len(), 2);
        assert_eq!(std::fs::metadata(root.join("keep.txt")).unwrap().modified().unwrap(), date);
        assert_eq!(std::fs::metadata(&root).unwrap().modified().unwrap(), date);
    }
}
