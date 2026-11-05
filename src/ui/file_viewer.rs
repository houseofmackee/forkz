//! forkz as the default folder viewer instead of Finder (Settings >
//! General), set up the way ForkLift does it: macOS's `NSFileViewer`
//! preference, which "Show in Finder" in other apps follows, and the default
//! app for opening folders. forkz then opens the folders it is sent, and
//! shows files it is asked to reveal, selected in their folder.
//!
//! Only the app bundle can be the default (`cargo run` has no bundle).

use std::path::PathBuf;
use std::process::Command;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{msg_send, sel};
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventManager, NSBundle, NSError, NSURL};

use super::util::{ns, url_path};

const FINDER: &str = "com.apple.finder";

/// Finder's "reveal" event ('misc' 'mvis'), which `NSFileViewer` apps get.
const REVEAL_CLASS: u32 = u32::from_be_bytes(*b"misc");
const REVEAL_ID: u32 = u32::from_be_bytes(*b"mvis");
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

/// This app's bundle identifier and location, when it runs as an app.
fn bundle() -> Option<(String, Retained<NSURL>)> {
    let bundle = NSBundle::mainBundle();
    let id = bundle.bundleIdentifier()?.to_string();
    bundle.bundlePath().to_string().ends_with(".app").then(|| (id, bundle.bundleURL()))
}

/// Whether this copy of forkz can be made the default (it is an app).
pub fn available() -> bool {
    bundle().is_some()
}

/// Whether forkz is the default folder viewer now.
pub fn is_default() -> bool {
    let Some((id, url)) = bundle() else { return false };
    let viewer = Command::new("/usr/bin/defaults")
        .args(["read", "-g", "NSFileViewer"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let folder_app = NSWorkspace::sharedWorkspace()
        .URLForApplicationToOpenContentType(unsafe { objc2_uniform_type_identifiers::UTTypeFolder })
        .and_then(|u| url_path(&u));
    viewer == id && folder_app == url_path(&url)
}

/// Makes forkz the default folder viewer, or gives the job back to Finder.
pub fn set_default(on: bool) -> Result<(), String> {
    let workspace = NSWorkspace::sharedWorkspace();
    let app = if on {
        bundle().map(|(_, url)| url).ok_or("Only the forkz app can be the default folder viewer.")?
    } else {
        workspace.URLForApplicationWithBundleIdentifier(&ns(FINDER)).ok_or("Finder was not found.")?
    };
    let done = RcBlock::new(|error: *mut NSError| {
        if let Some(error) = unsafe { error.as_ref() } {
            tracing::warn!("default folder app: {}", error.localizedDescription());
        }
    });
    workspace.setDefaultApplicationAtURL_toOpenContentType_completionHandler(
        &app,
        unsafe { objc2_uniform_type_identifiers::UTTypeFolder },
        Some(&done),
    );
    let viewer = match (on, bundle()) {
        (true, Some((id, _))) => {
            Command::new("/usr/bin/defaults").args(["write", "-g", "NSFileViewer", "-string", &id]).status()
        }
        _ => Command::new("/usr/bin/defaults").args(["delete", "-g", "NSFileViewer"]).status(),
    };
    match viewer {
        Ok(_) => Ok(()),
        Err(e) => Err(format!("The NSFileViewer preference could not be changed: {e}")),
    }
}

/// Listens for "reveal" requests (sent to the `NSFileViewer` app).
/// `handler` answers `handleReveal:withReplyEvent:`.
pub fn install(handler: &AnyObject) {
    let manager = NSAppleEventManager::sharedAppleEventManager();
    let _: () = unsafe {
        msg_send![
            &manager,
            setEventHandler: handler,
            andSelector: sel!(handleReveal:withReplyEvent:),
            forEventClass: REVEAL_CLASS,
            andEventID: REVEAL_ID
        ]
    };
}

/// The files and folders an Apple event is about.
pub fn paths_of(event: &NSAppleEventDescriptor) -> Vec<PathBuf> {
    let direct: Option<Retained<NSAppleEventDescriptor>> =
        unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
    let Some(direct) = direct else { return Vec::new() };
    let one = |d: &NSAppleEventDescriptor| d.fileURLValue().and_then(|u| url_path(&u));
    let count = direct.numberOfItems();
    if count <= 0 {
        return one(&direct).into_iter().collect();
    }
    (1..=count).filter_map(|i| direct.descriptorAtIndex(i)).filter_map(|d| one(&d)).collect()
}

/// What to do with paths sent to forkz: folders open (the first in the
/// active pane, others in new tabs); files are shown selected in their
/// folder.
#[derive(Debug, PartialEq)]
pub enum Show {
    Folder(PathBuf),
    Select(PathBuf, Vec<PathBuf>),
}

pub fn plan(paths: &[PathBuf], is_folder: impl Fn(&std::path::Path) -> bool, reveal: bool) -> Vec<Show> {
    let mut shows: Vec<Show> = Vec::new();
    for path in paths {
        if is_folder(path) && !reveal {
            shows.push(Show::Folder(path.clone()));
            continue;
        }
        let Some(parent) = path.parent() else {
            shows.push(Show::Folder(path.clone()));
            continue;
        };
        match shows.iter_mut().find(|s| matches!(s, Show::Select(p, _) if p == parent)) {
            Some(Show::Select(_, selected)) => selected.push(path.clone()),
            _ => shows.push(Show::Select(parent.to_path_buf(), vec![path.clone()])),
        }
    }
    shows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_open_and_files_are_selected_in_their_folder() {
        let folder = |p: &std::path::Path| !p.to_string_lossy().contains('.');
        let paths: Vec<PathBuf> = ["/a/docs", "/a/x.txt", "/a/y.txt", "/b/z.png"].iter().map(PathBuf::from).collect();
        assert_eq!(
            plan(&paths, folder, false),
            [
                Show::Folder("/a/docs".into()),
                Show::Select("/a".into(), vec!["/a/x.txt".into(), "/a/y.txt".into()]),
                Show::Select("/b".into(), vec!["/b/z.png".into()]),
            ]
        );
        // Revealing a folder selects it in its parent.
        assert_eq!(plan(&paths[..1], folder, true), [Show::Select("/a".into(), vec!["/a/docs".into()])]);
        assert_eq!(plan(&[PathBuf::from("/")], folder, true), [Show::Folder("/".into())]);
    }
}
