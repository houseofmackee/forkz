//! Updates through Sparkle. `scripts/bundle.sh` embeds Sparkle.framework;
//! it is loaded at runtime, so builds without it (`cargo run`, tests) work
//! the same, minus Check for Updates.
//!
//! The feed is the latest regular release's `appcast.xml` (`SUFeedURL` in
//! Info.plist): pre-releases are never offered. Sparkle asks on the second
//! launch whether to check automatically; the forkz menu can change that.

use std::cell::RefCell;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSBundle, NSString};

struct Updater {
    /// The SPUStandardUpdaterController.
    controller: Retained<AnyObject>,
    /// Kept alive: Sparkle holds its delegate weakly.
    _delegate: Retained<Delegate>,
}

thread_local! {
    static UPDATER: RefCell<Option<Updater>> = const { RefCell::new(None) };
}

define_class!(
    /// Sparkle's updater delegate. Sparkle asks with `respondsToSelector:`,
    /// so only the method used is defined.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzUpdaterDelegate"]
    struct Delegate;

    impl Delegate {
        /// Debug builds, or with `FORKZ_UPDATE_TEST`: `FORKZ_FEED_URL` points
        /// Sparkle at a test feed (it must still be signed with the release
        /// key).
        #[unsafe(method(feedURLStringForUpdater:))]
        fn feed_url(&self, _updater: &AnyObject) -> *mut NSString {
            test_feed_url().map_or(std::ptr::null_mut(), |url| Retained::autorelease_return(NSString::from_str(&url)))
        }
    }
);

fn test_feed_url() -> Option<String> {
    if !cfg!(debug_assertions) && std::env::var_os("FORKZ_UPDATE_TEST").is_none() {
        return None;
    }
    std::env::var("FORKZ_FEED_URL").ok().filter(|url| !url.is_empty())
}

/// Loads Sparkle from the bundle and starts the updater, if the bundle has
/// it.
pub fn start(mtm: MainThreadMarker) {
    let Some(frameworks) = NSBundle::mainBundle().privateFrameworksPath() else { return };
    let path = NSString::from_str(&format!("{frameworks}/Sparkle.framework"));
    let Some(bundle) = NSBundle::bundleWithPath(&path) else { return };
    // SAFETY: loading Sparkle, signed with the app's own identity, from the
    // app's Frameworks folder.
    if !unsafe { bundle.load() } {
        tracing::warn!("Sparkle.framework is in the bundle but did not load");
        return;
    }
    let Some(class) = AnyClass::get(c"SPUStandardUpdaterController") else {
        tracing::warn!("Sparkle loaded without SPUStandardUpdaterController");
        return;
    };
    let delegate: Retained<Delegate> = {
        let this = Delegate::alloc(mtm).set_ivars(());
        // SAFETY: NSObject's designated initializer.
        unsafe { msg_send![super(this), init] }
    };
    // SAFETY: Sparkle's documented initializer; the delegate implements only
    // optional methods.
    let controller: Option<Retained<AnyObject>> = unsafe {
        let allocated: Allocated<AnyObject> = msg_send![class, alloc];
        msg_send![
            allocated,
            initWithStartingUpdater: Bool::YES,
            updaterDelegate: &*delegate,
            userDriverDelegate: None::<&AnyObject>
        ]
    };
    let Some(controller) = controller else {
        tracing::warn!("the Sparkle updater did not start");
        return;
    };
    tracing::info!("updates through Sparkle");
    UPDATER.with(|u| *u.borrow_mut() = Some(Updater { controller, _delegate: delegate }));
    // Update tests: check right away (`FORKZ_UPDATE_TEST=ui` shows the
    // result like Check for Updates does).
    match std::env::var("FORKZ_UPDATE_TEST").as_deref() {
        Ok("ui") => check(mtm),
        Ok(_) => {
            if let Some(updater) = updater_object() {
                // SAFETY: SPUUpdater's checkForUpdatesInBackground.
                unsafe {
                    let _: () = msg_send![&*updater, checkForUpdatesInBackground];
                }
            }
        }
        Err(_) => {}
    }
}

/// Whether this build can update itself (Sparkle is in the bundle).
pub fn available() -> bool {
    UPDATER.with(|u| u.borrow().is_some())
}

/// Check for Updates: shows Sparkle's window with the result.
pub fn check(mtm: MainThreadMarker) {
    UPDATER.with(|u| {
        if let Some(updater) = u.borrow().as_ref() {
            NSApplication::sharedApplication(mtm).activate();
            // SAFETY: SPUStandardUpdaterController's checkForUpdates: action.
            unsafe {
                let _: () = msg_send![&*updater.controller, checkForUpdates: None::<&AnyObject>];
            }
        }
    });
}

fn updater_object() -> Option<Retained<AnyObject>> {
    UPDATER.with(|u| {
        u.borrow().as_ref().and_then(|updater| {
            // SAFETY: SPUStandardUpdaterController's updater property.
            unsafe { msg_send![&*updater.controller, updater] }
        })
    })
}

/// Whether Sparkle checks for updates on its own.
pub fn checks_automatically() -> bool {
    updater_object().is_some_and(|updater| {
        // SAFETY: SPUUpdater's automaticallyChecksForUpdates property.
        let on: Bool = unsafe { msg_send![&*updater, automaticallyChecksForUpdates] };
        on.as_bool()
    })
}

pub fn set_checks_automatically(on: bool) {
    if let Some(updater) = updater_object() {
        // SAFETY: SPUUpdater's automaticallyChecksForUpdates setter.
        unsafe {
            let _: () = msg_send![&*updater, setAutomaticallyChecksForUpdates: Bool::new(on)];
        }
    }
}
