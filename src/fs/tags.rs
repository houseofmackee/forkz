//! Finder tags: names and their colors. macOS keeps them in a file's
//! `com.apple.metadata:_kMDItemUserTags` extended attribute, as a property
//! list of strings such as "Work\n4" (the number is the color).

use std::ffi::CString;
use std::path::Path;

use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSData, NSPropertyListFormat, NSPropertyListSerialization, NSString, NSURL};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// Finder's color number: 0 none, 1 gray, 2 green, 3 purple, 4 blue,
    /// 5 yellow, 6 red, 7 orange.
    pub color: u8,
}

/// Finder's color tags, in its order.
pub const COLORS: [(&str, u8); 7] =
    [("Red", 6), ("Orange", 7), ("Yellow", 5), ("Green", 2), ("Blue", 4), ("Purple", 3), ("Gray", 1)];

const ATTRIBUTE: &str = "com.apple.metadata:_kMDItemUserTags";

/// The tags of a file (none if it has none or they cannot be read).
pub fn read(path: &Path) -> Vec<Tag> {
    let Ok(file) = CString::new(path.as_os_str().as_encoded_bytes()) else { return Vec::new() };
    let Ok(name) = CString::new(ATTRIBUTE) else { return Vec::new() };
    // SAFETY: getxattr with a null buffer returns the size; then fills ours.
    let size =
        unsafe { libc::getxattr(file.as_ptr(), name.as_ptr(), std::ptr::null_mut(), 0, 0, libc::XATTR_NOFOLLOW) };
    if size <= 0 {
        return Vec::new();
    }
    let mut buffer = vec![0u8; size as usize];
    let read = unsafe {
        libc::getxattr(file.as_ptr(), name.as_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), 0, libc::XATTR_NOFOLLOW)
    };
    if read <= 0 {
        return Vec::new();
    }
    buffer.truncate(read as usize);
    parse(&buffer)
}

/// The tags in the attribute's property list.
fn parse(bytes: &[u8]) -> Vec<Tag> {
    let data = NSData::with_bytes(bytes);
    let mut format = NSPropertyListFormat::BinaryFormat_v1_0;
    let Ok(list) = (unsafe {
        NSPropertyListSerialization::propertyListWithData_options_format_error(
            &data,
            objc2_foundation::NSPropertyListMutabilityOptions::Immutable,
            &mut format,
        )
    }) else {
        return Vec::new();
    };
    let Ok(array) = list.downcast::<NSArray>() else { return Vec::new() };
    array
        .iter()
        .filter_map(|item| item.downcast::<NSString>().ok())
        .map(|text| {
            let text = text.to_string();
            match text.rsplit_once('\n') {
                Some((name, color)) => Tag { name: name.to_string(), color: color.trim().parse().unwrap_or(0) },
                // A color tag without its number still has its color.
                None => Tag { color: COLORS.iter().find(|(n, _)| *n == text).map_or(0, |c| c.1), name: text },
            }
        })
        .collect()
}

/// Replaces a file's tags. Finder's color names (and tags Finder already
/// knows) get their colors.
pub fn set(path: &Path, names: &[String]) -> Result<(), String> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    let names: Vec<Retained<NSString>> = names.iter().map(|t| NSString::from_str(t)).collect();
    let array = NSArray::from_retained_slice(&names);
    let key = unsafe { objc2_foundation::NSURLTagNamesKey };
    unsafe { url.setResourceValue_forKey_error(Some(&array), key) }.map_err(|e| e.localizedDescription().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_read_with_colors() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        assert!(read(&file).is_empty());
        set(&file, &["Red".into(), "Blue".into(), "Project X".into()]).unwrap();
        let tags = read(&file);
        let summary: Vec<(&str, u8)> = tags.iter().map(|t| (t.name.as_str(), t.color)).collect();
        assert_eq!(summary[..2], [("Red", 6), ("Blue", 4)]);
        assert_eq!(summary[2].0, "Project X");
        set(&file, &[]).unwrap();
        assert!(read(&file).is_empty());
    }
}
