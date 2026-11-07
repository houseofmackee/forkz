//! Sound cues, as Finder has them: moving items to the Trash, deleting
//! them for good, and a long transfer finishing (or failing). They follow
//! macOS's "Play user interface sound effects" setting and forkz's own
//! (Settings > General).

use std::cell::RefCell;
use std::time::Duration;

use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_app_kit::NSSound;
use objc2_foundation::NSUserDefaults;

use super::util::ns;

/// Transfers at least this long chime when they finish.
pub const LONG_TRANSFER: Duration = Duration::from_secs(5);

const SYSTEM_SOUNDS: &str = "/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Items went to the Trash.
    Trash,
    /// Items were deleted for good.
    Delete,
    /// A long transfer finished.
    Done,
    /// A long transfer finished with problems.
    Failed,
}

thread_local! {
    /// Sounds playing (kept until they are done).
    static PLAYING: RefCell<Vec<Retained<NSSound>>> = const { RefCell::new(Vec::new()) };
}

/// macOS's Sound settings: "Play user interface sound effects" (on unless
/// turned off).
fn system_allows() -> bool {
    let defaults = NSUserDefaults::standardUserDefaults();
    defaults.objectForKey(&ns("com.apple.sound.uiaudio.enabled")).is_none()
        || defaults.boolForKey(&ns("com.apple.sound.uiaudio.enabled"))
}

/// Plays a cue, if the settings allow it.
pub fn play(cue: Cue) {
    let Some(settings) = super::current_settings() else { return };
    let wanted = match cue {
        Cue::Trash | Cue::Delete => settings.general.sound_effects,
        Cue::Done | Cue::Failed => settings.general.transfer_sounds,
    };
    if !wanted || !system_allows() {
        return;
    }
    let sound = match cue {
        Cue::Trash => from_file("finder/move to trash.aif"),
        Cue::Delete => from_file("finder/empty trash.aif"),
        Cue::Done => NSSound::soundNamed(&ns("Glass")),
        Cue::Failed => NSSound::soundNamed(&ns("Basso")),
    };
    let Some(sound) = sound else { return };
    tracing::debug!("sound cue {cue:?}");
    PLAYING.with(|playing| {
        let mut playing = playing.borrow_mut();
        playing.retain(|s| s.isPlaying());
        if sound.play() {
            playing.push(sound);
        }
    });
}

fn from_file(name: &str) -> Option<Retained<NSSound>> {
    let path = format!("{SYSTEM_SOUNDS}/{name}");
    NSSound::initWithContentsOfFile_byReference(NSSound::alloc(), &ns(&path), true)
}
