//! What Spotlight knows of an item on the Mac, for the list's optional
//! columns: an image's or video's size in pixels, a video's or song's
//! length, when the item was last opened, and its Finder comment. Images
//! Spotlight has not indexed get their size from the file (ImageIO).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDate, NSDictionary, NSMetadataItem, NSNumber, NSString, NSURL};

/// The column identifiers these fill.
pub const COLUMNS: [&str; 4] = ["dimensions", "duration", "opened", "comment"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    /// Width and height in pixels.
    pub pixels: Option<(u32, u32)>,
    /// Length in seconds.
    pub duration: Option<f64>,
    pub last_opened: Option<SystemTime>,
    pub comment: Option<String>,
}

impl Meta {
    pub fn is_empty(&self) -> bool {
        *self == Meta::default()
    }

    /// The text for a column.
    pub fn text(&self, column: &str) -> String {
        match column {
            "dimensions" => self.pixels.map(|(w, h)| format!("{w} \u{d7} {h}")).unwrap_or_default(),
            "duration" => self.duration.map(format_duration).unwrap_or_default(),
            "comment" => self.comment.clone().unwrap_or_default(),
            _ => String::new(),
        }
    }
}

/// "0:07", "3:45", "1:02:03".
pub fn format_duration(seconds: f64) -> String {
    let total = seconds.round().max(0.0) as u64;
    let (hours, minutes, seconds) = (total / 3600, total % 3600 / 60, total % 60);
    if hours > 0 { format!("{hours}:{minutes:02}:{seconds:02}") } else { format!("{minutes}:{seconds:02}") }
}

fn number(item: &NSMetadataItem, key: &str) -> Option<f64> {
    let value = item.valueForAttribute(&NSString::from_str(key))?;
    value.downcast_ref::<NSNumber>().map(|n| n.as_f64())
}

/// Reads an item's metadata (touches the disk: not on the main thread).
pub fn read(path: &Path) -> Meta {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let mut meta = Meta::default();
    if let Some(item) = NSMetadataItem::initWithURL(NSMetadataItem::alloc(), &url) {
        let width = number(&item, "kMDItemPixelWidth");
        let height = number(&item, "kMDItemPixelHeight");
        if let (Some(w), Some(h)) = (width, height)
            && w > 0.0
            && h > 0.0
        {
            meta.pixels = Some((w as u32, h as u32));
        }
        meta.duration = number(&item, "kMDItemDurationSeconds").filter(|d| *d > 0.0);
        meta.last_opened = item
            .valueForAttribute(&NSString::from_str("kMDItemLastUsedDate"))
            .and_then(|v| v.downcast_ref::<NSDate>().map(|d| d.timeIntervalSince1970()))
            .filter(|s| *s > 0.0)
            .map(|s| UNIX_EPOCH + Duration::from_secs_f64(s));
        meta.comment = item
            .valueForAttribute(&NSString::from_str("kMDItemFinderComment"))
            .and_then(|v| v.downcast_ref::<NSString>().map(|s| s.to_string()))
            .filter(|s| !s.trim().is_empty());
    }
    if meta.pixels.is_none() && is_image(path) {
        meta.pixels = image_pixels(&url);
    }
    meta
}

fn is_image(path: &Path) -> bool {
    const IMAGES: &[&str] = &[
        "jpg", "jpeg", "png", "heic", "heif", "gif", "tif", "tiff", "bmp", "webp", "psd", "dng", "cr2", "cr3", "nef",
        "arw",
    ];
    path.extension().is_some_and(|e| IMAGES.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// An image's size from its header.
fn image_pixels(url: &NSURL) -> Option<(u32, u32)> {
    use objc2_core_foundation::{CFDictionary, CFURL};
    use objc2_image_io::CGImageSource;
    // NSURL and CFURL are the same object.
    let cf_url: &CFURL = unsafe { &*(url as *const NSURL as *const CFURL) };
    let source = unsafe { CGImageSource::with_url(cf_url, None) }?;
    let properties = unsafe { source.properties_at_index(0, None) }?;
    // CFDictionary and NSDictionary are the same object too.
    let properties: &NSDictionary<NSString, AnyObject> = unsafe {
        &*(objc2_core_foundation::CFRetained::as_ptr(&properties).as_ptr() as *const CFDictionary as *const _)
    };
    let value = |key: &str| -> Option<u32> {
        let v: Retained<AnyObject> = properties.objectForKey(&NSString::from_str(key))?;
        v.downcast_ref::<NSNumber>().map(|n| n.as_u32())
    };
    Some((value("PixelWidth")?, value("PixelHeight")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_reads() {
        assert_eq!(format_duration(7.4), "0:07");
        assert_eq!(format_duration(225.0), "3:45");
        assert_eq!(format_duration(3723.0), "1:02:03");
        let meta = Meta { pixels: Some((4032, 3024)), ..Meta::default() };
        assert_eq!(meta.text("dimensions"), "4032 \u{d7} 3024");
        assert_eq!(meta.text("duration"), "");

        // An image's size from the file, also where Spotlight does not look.
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("dot.png");
        // A 3 x 2 PNG.
        let data: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00,
            0x00, 0x03, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00, 0x00, 0x12, 0x16, 0xf1, 0x4d, 0x00, 0x00, 0x00,
            0x10, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x41, 0x0c, 0x70, 0x16, 0x00, 0x41,
            0xd2, 0x05, 0xfb, 0x87, 0xf0, 0xb9, 0x48, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60,
            0x82,
        ];
        std::fs::write(&png, data).unwrap();
        assert_eq!(read(&png).pixels, Some((3, 2)));
        assert!(read(&dir.path().join("missing.txt")).is_empty());
    }
}
