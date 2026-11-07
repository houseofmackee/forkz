//! The app's side of its Shortcuts actions (macos/Intents/ForkzIntents.swift,
//! built in by build.rs): the actions hand over forkz:// links, and ask for
//! the names of saved sites, synclets, smart folders, workspaces, and the
//! commands they may run.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};

use dispatch2::DispatchQueue;

use super::commands::Command;

thread_local! {
    /// Links that came before the app was ready (an action can start it).
    static WAITING: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// A link from an action: followed on the main thread (once the app is
/// ready). Called from any thread.
///
/// # Safety
/// `link` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn forkz_intent_link(link: *const c_char) {
    if link.is_null() {
        return;
    }
    let link = unsafe { CStr::from_ptr(link) }.to_string_lossy().into_owned();
    DispatchQueue::main().exec_async(move || {
        if super::app().is_some() {
            super::open_link(&link);
        } else {
            WAITING.with(|w| w.borrow_mut().push(link));
        }
    });
}

/// Follows the links that came before the app was ready.
pub fn follow_waiting() {
    for link in WAITING.with(|w| std::mem::take(&mut *w.borrow_mut())) {
        super::open_link(&link);
    }
}

/// Names for an action's choices, one "id<TAB>name" a line: "site",
/// "synclet", "smart-folder", "workspace", or "command". Read from the
/// saved settings (any thread). Free with `forkz_intent_free`.
///
/// # Safety
/// `kind` is a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn forkz_intent_names(kind: *const c_char) -> *mut c_char {
    if kind.is_null() {
        return std::ptr::null_mut();
    }
    let kind = unsafe { CStr::from_ptr(kind) }.to_string_lossy().into_owned();
    let lines = names(&kind).into_iter().map(|(id, name)| format!("{id}\t{name}")).collect::<Vec<_>>().join("\n");
    CString::new(lines.replace('\0', "")).map_or(std::ptr::null_mut(), CString::into_raw)
}

/// Frees what `forkz_intent_names` returned.
///
/// # Safety
/// `text` came from `forkz_intent_names` (or is null), and is freed once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn forkz_intent_free(text: *mut c_char) {
    if !text.is_null() {
        drop(unsafe { CString::from_raw(text) });
    }
}

/// (id, name) pairs; ids are what forkz:// links take.
pub fn names(kind: &str) -> Vec<(String, String)> {
    let one = |name: &str| (name.to_string(), name.to_string());
    match kind {
        "site" => crate::sites::SiteStore::load()
            .sites
            .iter()
            .map(|s| {
                let title =
                    if s.folder.is_empty() { s.name.clone() } else { format!("{} \u{203a} {}", s.folder, s.name) };
                (s.id.clone(), title)
            })
            .collect(),
        "synclet" => crate::settings::Settings::load().synclets.iter().map(|s| one(&s.name)).collect(),
        "smart-folder" => crate::settings::Settings::load().smart_folders.iter().map(|s| one(&s.name)).collect(),
        "workspace" => crate::settings::Settings::load().workspaces.iter().map(|w| one(&w.name)).collect(),
        "command" => Command::ALL
            .iter()
            .filter(|c| !crate::links::UNSAFE_COMMANDS.contains(&c.id().as_str()))
            .map(|c| (c.id(), c.title().trim_end_matches('\u{2026}').to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hands_names_across() {
        let text = unsafe { forkz_intent_names(c"command".as_ptr()) };
        let lines = unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned();
        unsafe { forkz_intent_free(text) };
        assert!(lines.lines().any(|l| l == "ComparePanes\tCompare Panes"));
        assert!(lines.lines().any(|l| l == "FindDuplicates\tFind Duplicates"));
        // Nothing that deletes or moves.
        assert!(!lines.lines().any(|l| l.starts_with("Trash\t") || l.starts_with("DeleteImmediately\t")));
        assert!(names("nonsense").is_empty());
        assert!(unsafe { forkz_intent_names(std::ptr::null()) }.is_null());
    }
}
