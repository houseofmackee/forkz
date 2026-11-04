//! Dragging server files to Finder (or any app that takes files): each
//! dragged row is a file promise. When the drop lands, the item downloads
//! as a normal job (with progress in Activity) into a hidden folder at the
//! drop location, then moves into place and the promise is kept.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Sender, channel};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::{NSFilePromiseProvider, NSFilePromiseProviderDelegate};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSOperationQueue, NSString, NSURL};
use objc2_uniform_type_identifiers::UTType;

use super::util::{ns, url_path};
use crate::fs::Entry;
use crate::fs::ops::{Job, JobKind, Outcome, Resolution};
use crate::vfs::{self, FsRef};

pub struct PromiseIvars {
    fs: FsRef,
    path: PathBuf,
    name: String,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "ForkzFilePromise"]
    #[ivars = PromiseIvars]
    pub struct FilePromise;

    unsafe impl NSObjectProtocol for FilePromise {}

    unsafe impl NSFilePromiseProviderDelegate for FilePromise {
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn file_name(&self, _provider: &NSFilePromiseProvider, _file_type: &NSString) -> Retained<NSString> {
            ns(&self.ivars().name)
        }

        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write_promise(
            &self,
            _provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion: &block2::DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let result = self.write_to(url);
            match result {
                Ok(()) => completion.call((std::ptr::null_mut(),)),
                Err(code) => {
                    let error = NSError::new(code, unsafe { objc2_foundation::NSCocoaErrorDomain });
                    completion.call((Retained::as_ptr(&error).cast_mut(),));
                }
            }
        }

        #[unsafe(method_id(operationQueueForFilePromiseProvider:))]
        fn operation_queue(&self, _provider: &NSFilePromiseProvider) -> Retained<NSOperationQueue> {
            // Downloads wait for their job here, not on the main thread.
            NSOperationQueue::new()
        }
    }
);

/// NSUserCancelledError and NSFileWriteUnknownError.
const CANCELLED: isize = 3072;
const FAILED: isize = 512;

thread_local! {
    /// Providers only point at their delegates weakly: these keep the
    /// recent ones alive (a promise can be kept long after the drag).
    static KEPT: RefCell<VecDeque<Retained<FilePromise>>> = const { RefCell::new(VecDeque::new()) };
}

/// Jobs that keep promises, by their staging folder.
/// How a promise ended: kept, or an NSError code.
type Kept = Sender<Result<(), isize>>;

static WAITING: Mutex<Option<HashMap<PathBuf, Kept>>> = Mutex::new(None);

impl FilePromise {
    /// Runs on the provider's operation queue (not the main thread).
    fn write_to(&self, url: &NSURL) -> Result<(), isize> {
        let target = url_path(url).ok_or(FAILED)?;
        let parent = target.parent().ok_or(FAILED)?.to_path_buf();
        let staging = unique_staging(&parent);
        std::fs::create_dir(&staging).map_err(|_| FAILED)?;
        let (sender, receiver) = channel();
        WAITING.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_default().insert(staging.clone(), sender);
        let (fs, path) = (self.ivars().fs.clone(), self.ivars().path.clone());
        let destination = staging.clone();
        DispatchQueue::main().exec_async(move || {
            let mut job = super::transfer_job(JobKind::Copy, fs, vec![path], vfs::local(), destination);
            job.on_conflict = Some(Resolution::Replace);
            super::start_job(job);
        });
        let result = receiver.recv().unwrap_or(Err(FAILED)).and_then(|()| {
            let downloaded = staging.join(&self.ivars().name);
            std::fs::rename(&downloaded, &target).map_err(|e| {
                tracing::warn!("cannot move {} to {}: {e}", downloaded.display(), target.display());
                FAILED
            })
        });
        let _ = std::fs::remove_dir_all(&staging);
        result
    }
}

fn unique_staging(parent: &Path) -> PathBuf {
    let base = format!(".forkz-download-{}", std::process::id());
    (0u32..)
        .map(|n| parent.join(format!("{base}-{n}")))
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| parent.join(base))
}

/// A promise for a server item, for the drag pasteboard.
pub fn provider(fs: FsRef, entry: &Entry) -> Retained<NSFilePromiseProvider> {
    let promise: Retained<FilePromise> = {
        let this =
            FilePromise::alloc().set_ivars(PromiseIvars { fs, path: entry.path.clone(), name: entry.name.clone() });
        unsafe { msg_send![super(this), init] }
    };
    let file_type = if entry.is_folder_like() {
        unsafe { objc2_uniform_type_identifiers::UTTypeFolder }.identifier()
    } else {
        Path::new(&entry.name)
            .extension()
            .and_then(|e| UTType::typeWithFilenameExtension(&ns(&e.to_string_lossy())))
            .map_or_else(|| ns("public.data"), |t| t.identifier())
    };
    let provider = NSFilePromiseProvider::initWithFileType_delegate(
        NSFilePromiseProvider::alloc(),
        &file_type,
        ProtocolObject::from_ref(&*promise),
    );
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        kept.push_back(promise);
        while kept.len() > 256 {
            kept.pop_front();
        }
    });
    provider
}

/// A job ended: if it was keeping a promise, the promise learns how it went.
pub fn finished(job: &Job, outcome: &Outcome) {
    let waiting = WAITING.lock().unwrap_or_else(|e| e.into_inner()).as_mut().and_then(|w| w.remove(&job.destination));
    if let Some(sender) = waiting {
        let result = if outcome.cancelled {
            Err(CANCELLED)
        } else if !outcome.failed.is_empty() {
            Err(FAILED)
        } else {
            Ok(())
        };
        let _ = sender.send(result);
    }
}
