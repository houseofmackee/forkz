//! The viewer (View, F3): a file's contents as text or hex in a window of
//! its own, on the Mac, on servers, and in archives. Up to 64 MB is read
//! (in the background); text is decoded as UTF-8, UTF-16, or Latin-1, and
//! files that look binary open in hex. Command-F searches the text.

use std::cell::{OnceCell, RefCell};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSControlStateValueOn, NSControlTextEditingDelegate, NSFont, NSScrollView,
    NSSegmentedControl, NSStackView, NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate,
    NSTextField, NSTextView, NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
};

use objc2_app_kit::NSUserInterfaceItemIdentification;

use super::util::{mtm, ns, small_label};
use crate::fs::view::{self, ROW};
use crate::vfs::FsRef;

/// How much of a file is read.
const LIMIT: u64 = 64 * 1024 * 1024;

thread_local! {
    /// Open viewers (each closes on its own).
    static VIEWERS: RefCell<Vec<Retained<Viewer>>> = const { RefCell::new(Vec::new()) };
}

struct Parts {
    window: Retained<NSWindow>,
    mode: Retained<NSSegmentedControl>,
    wrap: Retained<NSButton>,
    status: Retained<NSTextField>,
    text_scroll: Retained<NSScrollView>,
    text: Retained<NSTextView>,
    hex_scroll: Retained<NSScrollView>,
    hex: Retained<NSTableView>,
}

pub struct ViewerIvars {
    parts: OnceCell<Parts>,
    bytes: RefCell<Rc<Vec<u8>>>,
    /// "12 KB", and "the first 64 MB of 2.1 GB" when cut off.
    size_note: RefCell<String>,
    encoding: RefCell<&'static str>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzViewer"]
    #[ivars = ViewerIvars]
    struct Viewer;

    impl Viewer {
        #[unsafe(method(modeChanged:))]
        fn mode_changed(&self, _sender: Option<&AnyObject>) {
            self.show_mode();
        }

        #[unsafe(method(wrapChanged:))]
        fn wrap_changed(&self, _sender: Option<&AnyObject>) {
            self.apply_wrap();
        }
    }

    unsafe impl NSObjectProtocol for Viewer {}

    unsafe impl NSControlTextEditingDelegate for Viewer {}

    unsafe impl NSWindowDelegate for Viewer {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            let me = self as *const Viewer as usize;
            DispatchQueue::main().exec_async(move || {
                VIEWERS.with(|v| v.borrow_mut().retain(|viewer| Retained::as_ptr(viewer) as usize != me));
            });
        }
    }

    unsafe impl NSTableViewDataSource for Viewer {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            view::hex_rows(self.ivars().bytes.borrow().len()) as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for Viewer {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            match (column, usize::try_from(row)) {
                (Some(column), Ok(row)) => self.hex_cell(table, column, row),
                _ => None,
            }
        }
    }
);

/// Opens a viewer for a file.
pub fn open(fs: FsRef, path: PathBuf, size: u64) {
    let mtm = mtm();
    let viewer = Viewer::alloc(mtm).set_ivars(ViewerIvars {
        parts: OnceCell::new(),
        bytes: RefCell::new(Rc::new(Vec::new())),
        size_note: RefCell::new(String::new()),
        encoding: RefCell::new(""),
    });
    let viewer: Retained<Viewer> = unsafe { msg_send![super(viewer), init] };
    viewer.build(&path, mtm);
    VIEWERS.with(|v| v.borrow_mut().push(viewer.clone()));
    let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let me = Retained::as_ptr(&viewer) as usize;
    std::thread::spawn(move || {
        let read = fs.open_read(&path).and_then(|reader| {
            let mut bytes = Vec::new();
            reader.take(LIMIT).read_to_end(&mut bytes)?;
            Ok(bytes)
        });
        DispatchQueue::main().exec_async(move || {
            // Still open (looked up by identity: windows can close first).
            let viewer = VIEWERS.with(|v| v.borrow().iter().find(|x| Retained::as_ptr(x) as usize == me).cloned());
            if let Some(viewer) = viewer {
                viewer.loaded(read, size, &title);
            }
        });
    });
}

impl Viewer {
    fn parts(&self) -> &Parts {
        self.ivars().parts.get().expect("built")
    }

    fn build(&self, path: &Path, mtm: MainThreadMarker) {
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(820.0, 620.0)),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        window.setTitle(&ns(&name));
        window.setRepresentedURL(Some(&super::util::file_url(path)));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        window.setMinSize(NSSize::new(420.0, 240.0));

        let mode = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &NSArray::from_retained_slice(&[ns("Text"), ns("Hex")]),
                objc2_app_kit::NSSegmentSwitchTracking::SelectOne,
                Some(self),
                Some(sel!(modeChanged:)),
                mtm,
            )
        };
        mode.setSelectedSegment(0);
        let wrap = unsafe {
            NSButton::checkboxWithTitle_target_action(&ns("Wrap Lines"), Some(self), Some(sel!(wrapChanged:)), mtm)
        };
        wrap.setState(NSControlStateValueOn);
        let status = small_label("Loading\u{2026}", mtm);
        status.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        status.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
        let bar = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[&*mode as &NSView, &*wrap as &NSView, &*status as &NSView]),
            mtm,
        );
        bar.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        bar.setSpacing(12.0);
        bar.setEdgeInsets(NSEdgeInsets { top: 8.0, left: 10.0, bottom: 8.0, right: 10.0 });

        let font = NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { objc2_app_kit::NSFontWeightRegular });
        let text_scroll = NSTextView::scrollableTextView(mtm);
        let text: Retained<NSTextView> =
            text_scroll.documentView().and_then(|v| v.downcast::<NSTextView>().ok()).expect("a text view");
        text.setEditable(false);
        text.setRichText(false);
        text.setFont(Some(&font));
        text.setUsesFindBar(true);
        text.setIncrementalSearchingEnabled(true);
        text.setTextContainerInset(NSSize::new(6.0, 6.0));

        let hex = NSTableView::new(mtm);
        for (id, title, width) in [("offset", "Offset", 86.0), ("hex", "Hex", 400.0), ("ascii", "Characters", 150.0)] {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
            column.setTitle(&ns(title));
            column.setWidth(width);
            hex.addTableColumn(&column);
        }
        hex.setRowHeight(18.0);
        hex.setUsesAlternatingRowBackgroundColors(true);
        hex.setStyle(objc2_app_kit::NSTableViewStyle::Plain);
        unsafe {
            hex.setDataSource(Some(ProtocolObject::from_ref(self)));
            hex.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        let hex_scroll = NSScrollView::new(mtm);
        hex_scroll.setDocumentView(Some(&hex));
        hex_scroll.setHasVerticalScroller(true);
        hex_scroll.setHidden(true);

        let content = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[&*bar as &NSView, &*text_scroll as &NSView, &*hex_scroll as &NSView]),
            mtm,
        );
        content.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        content.setSpacing(0.0);
        for view in [&*bar as &NSView, &text_scroll, &hex_scroll] {
            view.leadingAnchor().constraintEqualToAnchor(&content.leadingAnchor()).setActive(true);
            view.trailingAnchor().constraintEqualToAnchor(&content.trailingAnchor()).setActive(true);
        }
        for view in [&*text_scroll as &NSView, &hex_scroll] {
            view.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Vertical);
        }
        window.setContentView(Some(&content));
        window.center();
        let _ = self.ivars().parts.set(Parts {
            window: window.clone(),
            mode,
            wrap,
            status,
            text_scroll,
            text,
            hex_scroll,
            hex,
        });
        self.apply_wrap();
        window.makeKeyAndOrderFront(None);
    }

    /// The file's bytes arrived (or could not be read).
    fn loaded(&self, read: std::io::Result<Vec<u8>>, size: u64, title: &str) {
        let parts = self.parts();
        let bytes = match read {
            Ok(bytes) => bytes,
            Err(e) => {
                parts.status.setStringValue(&ns(&format!("\u{201c}{title}\u{201d} could not be read: {e}")));
                return;
            }
        };
        let shown = crate::fs::format_size(bytes.len() as u64);
        *self.ivars().size_note.borrow_mut() = if (bytes.len() as u64) < size {
            format!("the first {shown} of {}", crate::fs::format_size(size))
        } else {
            shown
        };
        let binary = view::looks_binary(&bytes);
        let (text, encoding) = view::decode(&bytes);
        *self.ivars().encoding.borrow_mut() = encoding;
        *self.ivars().bytes.borrow_mut() = Rc::new(bytes);
        parts.text.setString(&ns(&text));
        parts.hex.reloadData();
        parts.mode.setSelectedSegment(isize::from(binary));
        self.show_mode();
    }

    fn show_mode(&self) {
        let parts = self.parts();
        let hex = parts.mode.selectedSegment() == 1;
        parts.text_scroll.setHidden(hex);
        parts.hex_scroll.setHidden(!hex);
        parts.wrap.setHidden(hex);
        let note = self.ivars().size_note.borrow().clone();
        let status = if hex {
            format!("{note}, {} bytes a row", ROW)
        } else {
            format!("{note}, {}", self.ivars().encoding.borrow())
        };
        parts.status.setStringValue(&ns(&status));
        let focus: &NSView = if hex { &parts.hex } else { &parts.text };
        parts.window.makeFirstResponder(Some(focus));
    }

    /// Lines wrap at the window's width, or run on (scrolling sideways).
    fn apply_wrap(&self) {
        let parts = self.parts();
        let wrap = parts.wrap.state() == NSControlStateValueOn;
        let Some(container) = (unsafe { parts.text.textContainer() }) else { return };
        let huge = 1.0e7;
        container.setWidthTracksTextView(wrap);
        if wrap {
            let width = parts.text_scroll.contentSize().width;
            container.setContainerSize(NSSize::new(width, huge));
            parts.text.setFrameSize(NSSize::new(width, parts.text.frame().size.height));
        } else {
            container.setContainerSize(NSSize::new(huge, huge));
        }
        parts.text.setHorizontallyResizable(!wrap);
        parts.text.setMaxSize(NSSize::new(huge, huge));
        parts.text_scroll.setHasHorizontalScroller(!wrap);
    }

    fn hex_cell(&self, table: &NSTableView, column: &NSTableColumn, row: usize) -> Option<Retained<NSView>> {
        let id = column.identifier().to_string();
        let bytes = self.ivars().bytes.borrow().clone();
        let (offset, hex, ascii) = view::hex_row(&bytes, row);
        let text = match id.as_str() {
            "offset" => offset,
            "hex" => hex,
            _ => ascii,
        };
        let reuse = ns(&id);
        let existing: Option<Retained<NSView>> = unsafe { table.makeViewWithIdentifier_owner(&reuse, None) };
        let field = match existing.and_then(|v| v.downcast::<NSTextField>().ok()) {
            Some(field) => field,
            None => {
                let field = NSTextField::labelWithString(&ns(""), self.mtm());
                field.setIdentifier(Some(&reuse));
                let font =
                    NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { objc2_app_kit::NSFontWeightRegular });
                field.setFont(Some(&font));
                if id == "offset" {
                    field.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
                }
                field
            }
        };
        field.setStringValue(&ns(&text));
        Some(Retained::into_super(Retained::into_super(field)))
    }
}
