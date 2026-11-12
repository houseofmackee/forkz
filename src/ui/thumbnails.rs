//! Thumbnails of files' contents (as Finder shows them), for the icon view
//! and the gallery's strip. Quick Look makes them in the background; they
//! are kept for the session, by path, size, and date (a changed file gets
//! a new one). Until one is ready, and for files without one, the icon
//! shows.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::NSImage;
use objc2_core_foundation::CGSize;
use objc2_foundation::NSError;
use objc2_quick_look_thumbnailing::{
    QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes, QLThumbnailGenerator,
    QLThumbnailRepresentation,
};

use super::util::file_url;
use crate::fs::Entry;

/// The size asked for (in points; the icon views scale it down).
const SIZE: f64 = 128.0;

/// Kept thumbnails before the oldest are let go.
const LIMIT: usize = 3000;

/// A file as it is now: its path, size, and modification time.
type Key = (PathBuf, u64, u64);

thread_local! {
    static MADE: RefCell<HashMap<Key, Retained<NSImage>>> = RefCell::new(HashMap::new());
    /// Asked for, or without a thumbnail (not asked again).
    static ASKED: RefCell<HashSet<Key>> = RefCell::new(HashSet::new());
}

/// A thumbnail on its way from Quick Look's thread to the main thread.
struct Made(Option<Retained<NSImage>>);
// SAFETY: the image is only handed over, then used on the main thread.
unsafe impl Send for Made {}

fn key(entry: &Entry) -> Key {
    let modified = entry.modified.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
    (entry.path.clone(), entry.size, modified)
}

/// The thumbnail of a file on this Mac if it is ready; otherwise it is
/// asked for (once), and `ready` follows when it is.
pub fn get(entry: &Entry) -> Option<Retained<NSImage>> {
    let key = key(entry);
    if let Some(image) = MADE.with(|made| made.borrow().get(&key).cloned()) {
        return Some(image);
    }
    if ASKED.with(|asked| asked.borrow_mut().insert(key.clone())) {
        request(key);
    }
    None
}

fn request(key: Key) {
    let request = unsafe {
        QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
            QLThumbnailGenerationRequest::alloc(),
            &file_url(&key.0),
            CGSize::new(SIZE, SIZE),
            2.0,
            // A picture of the contents, not an icon.
            QLThumbnailGenerationRequestRepresentationTypes::Thumbnail,
        )
    };
    // Styled as Finder shows them: a page with a border and shadow, a
    // picture with a frame (not a white page on a white background).
    unsafe { request.setIconMode(true) };
    let handler = RcBlock::new(move |representation: *mut QLThumbnailRepresentation, _error: *mut NSError| {
        // SAFETY: Quick Look passes a valid representation, or null.
        let image = unsafe { representation.as_ref() }.map(|r| unsafe { r.NSImage() });
        let made = Made(image);
        let key = key.clone();
        DispatchQueue::main().exec_async(move || ready(key, made));
    });
    unsafe {
        QLThumbnailGenerator::sharedGenerator()
            .generateBestRepresentationForRequest_completionHandler(&request, &handler)
    };
}

/// A thumbnail was made (or could not be): the views showing the file
/// get it.
fn ready(key: Key, made: Made) {
    let Some(image) = made.0 else { return };
    let path = key.0.clone();
    MADE.with(|made| {
        let mut made = made.borrow_mut();
        if made.len() >= LIMIT {
            made.clear();
            ASKED.with(|asked| asked.borrow_mut().clear());
        }
        made.insert(key, image);
    });
    super::thumbnail_ready(&path);
}

/// An image's size to fit in a square of `side` points, keeping its shape.
pub fn fitted(image: &NSImage, side: f64) -> CGSize {
    let size = image.size();
    if size.width <= 0.0 || size.height <= 0.0 {
        return CGSize::new(side, side);
    }
    let scale = side / size.width.max(size.height);
    CGSize::new((size.width * scale).round(), (size.height * scale).round())
}

/// Whether a file can have a thumbnail here (a file on this Mac, not a
/// folder or app).
pub fn applies(entry: &Entry, local: bool) -> bool {
    local && !entry.is_folder_like() && Path::new(&entry.path).is_absolute()
}
