//! When a photo or video was taken (for renaming by it): a photo's EXIF
//! date (read from the file, so it works where Spotlight does not look),
//! or what Spotlight knows of the item's content (a video's recording
//! date). None for other files and when neither knows.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDate, NSDictionary, NSMetadataItem, NSString, NSURL};

const PHOTOS: &[&str] = &[
    "jpg", "jpeg", "heic", "heif", "png", "tif", "tiff", "dng", "cr2", "cr3", "nef", "arw", "raf", "orf", "rw2", "webp",
];
const VIDEOS: &[&str] = &["mov", "mp4", "m4v", "3gp", "avi", "mts", "m2ts", "mkv"];

pub fn date_taken(path: &Path) -> Option<SystemTime> {
    let extension = path.extension()?.to_string_lossy().to_lowercase();
    let photo = PHOTOS.contains(&extension.as_str());
    if !photo && !VIDEOS.contains(&extension.as_str()) {
        return None;
    }
    let exif = if photo { exif_date(path) } else { None };
    exif.or_else(|| spotlight_date(path))
}

fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

/// The photo's "DateTimeOriginal" (when it was taken, the camera's local
/// time).
fn exif_date(path: &Path) -> Option<SystemTime> {
    use objc2_core_foundation::{CFDictionary, CFURL};
    use objc2_image_io::CGImageSource;
    let url = file_url(path);
    // NSURL and CFURL are the same object.
    let cf_url: &CFURL = unsafe { &*(Retained::as_ptr(&url) as *const CFURL) };
    let source = unsafe { CGImageSource::with_url(cf_url, None) }?;
    let properties = unsafe { source.properties_at_index(0, None) }?;
    // CFDictionary and NSDictionary are the same object too.
    let properties: &NSDictionary<NSString, AnyObject> = unsafe {
        &*(objc2_core_foundation::CFRetained::as_ptr(&properties).as_ptr() as *const CFDictionary as *const _)
    };
    let exif = properties.objectForKey(&NSString::from_str("{Exif}"))?;
    exif.downcast_ref::<NSDictionary>()?;
    // Checked: a dictionary (of the same keys and values).
    let exif: &NSDictionary<NSString, AnyObject> = unsafe { &*(Retained::as_ptr(&exif) as *const _) };
    let original = exif.objectForKey(&NSString::from_str("DateTimeOriginal"))?;
    let text = original.downcast_ref::<NSString>()?.to_string();
    parse_exif(&text)
}

/// "2026:11:04 10:15:30" (local time) as a moment.
pub fn parse_exif(text: &str) -> Option<SystemTime> {
    let number = |range: std::ops::Range<usize>| text.get(range)?.trim().parse::<i32>().ok();
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || year < 1900 {
        return None;
    }
    // SAFETY: a zeroed tm is valid; mktime reads it as local time.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = second;
    tm.tm_isdst = -1;
    let seconds = unsafe { libc::mktime(&mut tm) };
    u64::try_from(seconds).ok().map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

/// What Spotlight knows of when the content was made (a video's
/// recording date), if it has the item.
fn spotlight_date(path: &Path) -> Option<SystemTime> {
    let url = file_url(path);
    let item = NSMetadataItem::initWithURL(NSMetadataItem::alloc(), &url)?;
    let value = item.valueForAttribute(&NSString::from_str("kMDItemContentCreationDate"))?;
    let date = value.downcast_ref::<NSDate>()?;
    let seconds = date.timeIntervalSince1970();
    (seconds > 0.0).then(|| UNIX_EPOCH + Duration::from_secs_f64(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exif_dates_as_local_time() {
        let taken = parse_exif("2026:11:04 10:15:30").unwrap();
        // Shown again in local time, the same digits.
        assert_eq!(crate::rename::format_date("YYYY-MM-DD hh-mm-ss", taken), "2026-11-04 10-15-30");
        assert_eq!(parse_exif("0000:00:00 00:00:00"), None);
        assert_eq!(parse_exif("garbage"), None);
        // Not a photo or video: no date taken.
        assert_eq!(date_taken(Path::new("/tmp/notes.txt")), None);
    }

    /// A real photo's date (FORKZ_TEST_PHOTO: a JPEG taken 2019-07-14
    /// 16:45:10; run with --ignored).
    #[test]
    #[ignore = "needs a photo with EXIF dates"]
    fn reads_a_real_photo() {
        let Some(photo) = std::env::var_os("FORKZ_TEST_PHOTO") else { return };
        let taken = date_taken(Path::new(&photo)).expect("a date taken");
        assert_eq!(crate::rename::format_date("YYYY-MM-DD hh-mm-ss", taken), "2019-07-14 16-45-10");
    }
}
