//! Small helpers between Rust and AppKit types.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2::{MainThreadMarker, Message};
use objc2_app_kit::{NSFont, NSImage, NSLineBreakMode, NSTextField, NSWorkspace};
use objc2_foundation::{NSDate, NSDateFormatter, NSDateFormatterStyle, NSString, NSURL};
use objc2_uniform_type_identifiers::UTType;

use crate::fs::{Entry, EntryKind};

/// A file's Finder tag names (on the Mac).
pub fn tags_of(path: &Path) -> Vec<String> {
    crate::fs::tags::read(path).into_iter().map(|t| t.name).collect()
}

/// Replaces a file's Finder tags.
pub fn set_tags(path: &Path, tags: &[String]) -> Result<(), String> {
    crate::fs::tags::set(path, tags)
}

/// The color of a Finder tag color number (None for no color).
pub fn tag_color(color: u8) -> Option<Retained<objc2_app_kit::NSColor>> {
    use objc2_app_kit::NSColor;
    Some(match color {
        1 => NSColor::systemGrayColor(),
        2 => NSColor::systemGreenColor(),
        3 => NSColor::systemPurpleColor(),
        4 => NSColor::systemBlueColor(),
        5 => NSColor::systemYellowColor(),
        6 => NSColor::systemRedColor(),
        7 => NSColor::systemOrangeColor(),
        _ => return None,
    })
}

/// An icon with dots in its tags' colors at the bottom right (up to three).
pub fn icon_with_tags(icon: &NSImage, colors: &[u8]) -> Retained<NSImage> {
    let colors: Vec<Retained<objc2_app_kit::NSColor>> = colors.iter().filter_map(|c| tag_color(*c)).take(3).collect();
    if colors.is_empty() {
        return icon.retain();
    }
    let base = icon.retain();
    let size = objc2_foundation::NSSize::new(32.0, 32.0);
    let draw = block2::RcBlock::new(move |rect: objc2_foundation::NSRect| -> objc2::runtime::Bool {
        base.drawInRect(rect);
        let dot = rect.size.width * 0.46;
        let step = rect.size.width * 0.28;
        // The last color on top, at the corner.
        for (i, color) in colors.iter().enumerate() {
            let offset = (colors.len() - 1 - i) as f64 * step;
            let oval = objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(rect.size.width - dot - offset, 0.0),
                objc2_foundation::NSSize::new(dot, dot),
            );
            let path = objc2_app_kit::NSBezierPath::bezierPathWithOvalInRect(oval);
            color.setFill();
            path.fill();
            objc2_app_kit::NSColor::windowBackgroundColor().setStroke();
            path.setLineWidth(dot * 0.14);
            path.stroke();
        }
        objc2::runtime::Bool::YES
    });
    NSImage::imageWithSize_flipped_drawingHandler(size, false, &draw)
}

pub fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("on the main thread")
}

pub fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

pub fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&ns(&path.to_string_lossy()))
}

pub fn url_path(url: &NSURL) -> Option<PathBuf> {
    url.path().map(|p| PathBuf::from(p.to_string()))
}

/// A non-editable, borderless label.
pub fn label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = NSTextField::labelWithString(&ns(text), mtm);
    field.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
    field
}

pub fn small_label(text: &str, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let field = label(text, mtm);
    field.setFont(Some(&NSFont::systemFontOfSize(NSFont::smallSystemFontSize())));
    field
}

/// An SF Symbol, or None if the system does not have it.
pub fn symbol(name: &str, description: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(name), Some(&ns(description)))
}

thread_local! {
    static DATE_FORMATTER: Retained<NSDateFormatter> = {
        let formatter = NSDateFormatter::new();
        formatter.setDateStyle(NSDateFormatterStyle::MediumStyle);
        formatter.setTimeStyle(NSDateFormatterStyle::ShortStyle);
        formatter.setDoesRelativeDateFormatting(true);
        formatter
    };
    /// Kind descriptions by extension ("PDF document").
    static KINDS: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
    /// Icons of plain files by extension; folders and packages can have
    /// their own icons, so they are not cached.
    static ICONS: RefCell<HashMap<String, Retained<NSImage>>> = RefCell::new(HashMap::new());
}

/// "Today at 09:41", "12 Mar 2026 at 14:02".
pub fn format_date(time: Option<SystemTime>) -> String {
    let Some(seconds) = time.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs_f64()) else {
        return "--".to_string();
    };
    let date = NSDate::dateWithTimeIntervalSince1970(seconds);
    DATE_FORMATTER.with(|f| f.stringFromDate(&date).to_string())
}

/// The kind Finder shows: "Folder", "PDF document", "Application".
pub fn kind_of(entry: &Entry) -> String {
    if entry.is_folder_like() {
        return if matches!(entry.kind, EntryKind::Symlink { .. }) { "Alias".into() } else { "Folder".into() };
    }
    if matches!(entry.kind, EntryKind::Symlink { .. }) {
        return "Alias".into();
    }
    let extension = entry.extension();
    if extension.is_empty() {
        return "Document".into();
    }
    KINDS.with(|kinds| {
        kinds
            .borrow_mut()
            .entry(extension.clone())
            .or_insert_with(|| {
                UTType::typeWithFilenameExtension(&ns(&extension))
                    .and_then(|t| t.localizedDescription())
                    .map(|d| capitalize_first(&d.to_string()))
                    .unwrap_or_else(|| format!("{} file", extension.to_uppercase()))
            })
            .clone()
    })
}

fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

pub fn icon_of(entry: &Entry, local: bool) -> Retained<NSImage> {
    let workspace = NSWorkspace::sharedWorkspace();
    if !local {
        // Remote items: by type only (there is no file to look at).
        let key = if entry.is_folder_like() { "/folder".to_string() } else { entry.extension() };
        if let Some(icon) = ICONS.with(|icons| icons.borrow().get(&key).cloned()) {
            return icon;
        }
        let content_type = if entry.is_folder_like() {
            Some(unsafe { objc2_uniform_type_identifiers::UTTypeFolder }.retain())
        } else {
            UTType::typeWithFilenameExtension(&ns(&key))
        };
        let icon = match content_type {
            Some(t) => workspace.iconForContentType(&t),
            None => workspace.iconForContentType(&unsafe { objc2_uniform_type_identifiers::UTTypeData }.retain()),
        };
        ICONS.with(|icons| icons.borrow_mut().insert(key, icon.clone()));
        return icon;
    }
    let cacheable = entry.kind == EntryKind::File && !entry.package;
    let extension = entry.extension();
    if cacheable && let Some(icon) = ICONS.with(|icons| icons.borrow().get(&extension).cloned()) {
        return icon;
    }
    let icon = workspace.iconForFile(&ns(&entry.path.to_string_lossy()));
    // Some file types (like images and icns) show their content; not cached.
    if cacheable && !matches!(extension.as_str(), "icns" | "ico" | "webloc" | "app" | "folder") {
        ICONS.with(|icons| icons.borrow_mut().insert(extension, icon.clone()));
    }
    icon
}

/// The standard folder icon.
pub fn folder_icon() -> Retained<NSImage> {
    NSWorkspace::sharedWorkspace().iconForContentType(&unsafe { objc2_uniform_type_identifiers::UTTypeFolder }.retain())
}

pub fn icon_for_path(path: &Path) -> Retained<NSImage> {
    NSWorkspace::sharedWorkspace().iconForFile(&ns(&path.to_string_lossy()))
}
