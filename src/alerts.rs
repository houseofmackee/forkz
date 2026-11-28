//! macOS notifications, for scheduled synclets that stop working (and work
//! again). Only the app can post them: macOS ties notifications to an app
//! bundle, so a build run from source (no bundle) posts none. A scheduled
//! synclet's job runs the app's own executable, so it posts as forkz, with
//! the permission the app asked for.

use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
};

/// Whether this process is forkz.app (notifications need its bundle).
pub fn available() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some_and(|id| id.to_string().starts_with("io.github.houseofmackee"))
}

/// Asks once whether forkz may post notifications (macOS remembers the
/// answer; System Settings > Notifications changes it).
pub fn ask_permission() {
    if !available() {
        return;
    }
    let done = RcBlock::new(|granted: Bool, _error: *mut NSError| {
        tracing::info!("notifications allowed: {}", granted.as_bool());
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &done);
}

/// Posts a notification, and waits (briefly) until macOS has it, so a
/// process about to end does not lose it. Without permission, nothing
/// shows.
pub fn post(id: &str, title: &str, body: &str) {
    if !available() {
        return;
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(id), &content, None);
    let (sent, delivered) = mpsc::channel::<()>();
    let done = RcBlock::new(move |error: *mut NSError| {
        // SAFETY: macOS passes a valid error, or null.
        if let Some(error) = unsafe { error.as_ref() } {
            tracing::info!("the notification was not shown: {}", error.localizedDescription());
        }
        let _ = sent.send(());
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .addNotificationRequest_withCompletionHandler(&request, Some(&done));
    let _ = delivered.recv_timeout(Duration::from_secs(5));
}
