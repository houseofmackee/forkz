//! The terminal pane (View > Show Terminal, Control-`): your shell, below
//! the file panes, in the active pane's folder. It follows the active
//! pane: going to another folder there types `cd` into the shell (only
//! while the shell waits for a command, never into a running program).
//!
//! The terminal itself is alacritty_terminal (Alacritty's emulator and
//! pseudo-terminal); this draws its screen and passes the keyboard,
//! mouse selection, scrolling, copy, and paste.

use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAppearanceCustomization, NSBezierPath, NSColor, NSEvent, NSEventModifierFlags, NSFont, NSPasteboard, NSView,
};
use objc2_foundation::{NSMutableAttributedString, NSPoint, NSRange, NSRect, NSSize};

use super::util::ns;

/// Space around the text.
const PAD: f64 = 6.0;

thread_local! {
    /// The terminal (one, below the panes).
    static TERMINAL: RefCell<Option<Retained<TerminalView>>> = const { RefCell::new(None) };
}

/// A redraw is already on its way to the main thread.
static REDRAW_QUEUED: AtomicBool = AtomicBool::new(false);

/// Passes the terminal's events to the main thread.
#[derive(Clone, Copy)]
struct Listener;

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                if !REDRAW_QUEUED.swap(true, Ordering::AcqRel) {
                    DispatchQueue::main().exec_async(|| {
                        REDRAW_QUEUED.store(false, Ordering::Release);
                        with_terminal(|t| t.setNeedsDisplay(true));
                    });
                }
            }
            Event::PtyWrite(text) => {
                DispatchQueue::main().exec_async(move || with_terminal(|t| t.write(text.into_bytes())))
            }
            Event::ClipboardStore(_, text) => DispatchQueue::main().exec_async(move || copy_text(&text)),
            Event::Bell => DispatchQueue::main().exec_async(|| objc2_app_kit::NSBeep()),
            Event::ChildExit(_) | Event::Exit => DispatchQueue::main().exec_async(super::terminal_exited),
            _ => {}
        }
    }
}

fn with_terminal(f: impl FnOnce(&TerminalView)) {
    if let Some(terminal) = TERMINAL.with(|t| t.borrow().clone()) {
        f(&terminal);
    }
}

fn copy_text(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    pasteboard.setString_forType(&ns(text), unsafe { objc2_app_kit::NSPasteboardTypeString });
}

pub struct TerminalIvars {
    term: OnceCell<Arc<FairMutex<Term<Listener>>>>,
    sender: OnceCell<EventLoopSender>,
    /// The pseudo-terminal's controlling side (to see what runs in it).
    master_fd: Cell<i32>,
    font: OnceCell<Retained<NSFont>>,
    bold: OnceCell<Retained<NSFont>>,
    /// Cell width and height.
    cell: Cell<(f64, f64)>,
    /// Scrolling smaller than a line, carried over.
    scroll_rest: Cell<f64>,
    /// The folder last sent with `cd`.
    folder: RefCell<PathBuf>,
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzTerminalView"]
    #[ivars = TerminalIvars]
    pub struct TerminalView;

    impl TerminalView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(setFrameSize:))]
        fn set_frame_size(&self, size: NSSize) {
            let _: () = unsafe { msg_send![super(self), setFrameSize: size] };
            self.fit();
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            self.key(event);
        }

        /// Command-C, Command-V, and Command-K here are the terminal's
        /// (not the file commands with the same keys).
        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> bool {
            self.key_equivalent(event)
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if let Some(window) = self.window() {
                window.makeFirstResponder(Some(self));
            }
            let (point, side) = self.cell_at(event);
            self.with_term(|term| term.selection = Some(Selection::new(SelectionType::Simple, point, side)));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let (point, side) = self.cell_at(event);
            self.with_term(|term| {
                if let Some(selection) = term.selection.as_mut() {
                    selection.update(point, side);
                }
            });
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            // A click without a drag selects nothing.
            self.with_term(|term| {
                if term.selection.as_ref().is_some_and(Selection::is_empty) {
                    term.selection = None;
                }
            });
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            self.scroll(event);
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            self.copy_selection();
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            self.paste_text();
        }
    }
);

impl TerminalView {
    /// Starts a shell in `folder`, in a view of this size.
    fn new(folder: &Path, frame: NSRect, mtm: MainThreadMarker) -> std::io::Result<Retained<Self>> {
        let size = super::current_settings().map_or(12.0, |s| s.appearance.text_size).clamp(9.0, 24.0) - 1.0;
        let font = NSFont::monospacedSystemFontOfSize_weight(size, unsafe { objc2_app_kit::NSFontWeightRegular });
        let bold = NSFont::monospacedSystemFontOfSize_weight(size, unsafe { objc2_app_kit::NSFontWeightBold });
        let cell = cell_size(&font);
        let this = Self::alloc(mtm).set_ivars(TerminalIvars {
            term: OnceCell::new(),
            sender: OnceCell::new(),
            master_fd: Cell::new(-1),
            font: OnceCell::new(),
            bold: OnceCell::new(),
            cell: Cell::new(cell),
            scroll_rest: Cell::new(0.0),
            folder: RefCell::new(folder.to_path_buf()),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let _ = this.ivars().font.set(font);
        let _ = this.ivars().bold.set(bold);
        let (columns, lines) = this.grid_size();
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &TermSize::new(columns, lines), Listener)));
        let mut options = tty::Options { working_directory: Some(folder.to_path_buf()), ..tty::Options::default() };
        options.env.insert("TERM".into(), "xterm-256color".into());
        options.env.insert("COLORTERM".into(), "truecolor".into());
        options.env.insert("TERM_PROGRAM".into(), "forkz".into());
        let pty = tty::new(&options, window_size(columns, lines, cell), 0)?;
        {
            use std::os::fd::AsRawFd;
            this.ivars().master_fd.set(pty.file().as_raw_fd());
        }
        let event_loop = EventLoop::new(term.clone(), Listener, pty, false, false)?;
        let _ = this.ivars().sender.set(event_loop.channel());
        let _ = this.ivars().term.set(term);
        event_loop.spawn();
        Ok(this)
    }

    fn with_term<R>(&self, f: impl FnOnce(&mut Term<Listener>) -> R) -> Option<R> {
        self.ivars().term.get().map(|term| f(&mut term.lock()))
    }

    fn write(&self, bytes: Vec<u8>) {
        if let Some(sender) = self.ivars().sender.get() {
            let _ = sender.send(Msg::Input(Cow::Owned(bytes)));
        }
        // Typing scrolls back to the bottom.
        self.with_term(|term| term.scroll_display(Scroll::Bottom));
    }

    /// Columns and lines that fit the view.
    fn grid_size(&self) -> (usize, usize) {
        let (width, height) = self.ivars().cell.get();
        let bounds = self.bounds().size;
        let columns = ((bounds.width - 2.0 * PAD) / width).floor().max(2.0) as usize;
        let lines = ((bounds.height - 2.0 * PAD) / height).floor().max(1.0) as usize;
        (columns, lines)
    }

    /// The terminal follows the view's size.
    fn fit(&self) {
        let (columns, lines) = self.grid_size();
        let cell = self.ivars().cell.get();
        let changed = self
            .with_term(|term| {
                let same = term.columns() == columns && term.screen_lines() == lines;
                if !same {
                    term.resize(TermSize::new(columns, lines));
                }
                !same
            })
            .unwrap_or(false);
        if changed && let Some(sender) = self.ivars().sender.get() {
            let _ = sender.send(Msg::Resize(window_size(columns, lines, cell)));
        }
    }

    /// The cell under the mouse (in the terminal's lines, with scrolling).
    fn cell_at(&self, event: &NSEvent) -> (Point, Side) {
        let local = self.convertPoint_fromView(event.locationInWindow(), None);
        let (width, height) = self.ivars().cell.get();
        let (offset, columns, lines) =
            self.with_term(|t| (t.grid().display_offset() as i32, t.columns(), t.screen_lines())).unwrap_or((0, 1, 1));
        let x = ((local.x - PAD) / width).max(0.0);
        let column = (x.floor() as usize).min(columns.saturating_sub(1));
        let row = (((local.y - PAD) / height).floor().max(0.0) as usize).min(lines.saturating_sub(1));
        let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
        (Point::new(Line(row as i32 - offset), Column(column)), side)
    }

    fn scroll(&self, event: &NSEvent) {
        let (_, height) = self.ivars().cell.get();
        let delta =
            if event.hasPreciseScrollingDeltas() { event.scrollingDeltaY() / height } else { event.scrollingDeltaY() };
        let total = self.ivars().scroll_rest.get() + delta;
        let lines = total.trunc() as i32;
        self.ivars().scroll_rest.set(total.fract());
        if lines == 0 {
            return;
        }
        let alternate = self
            .with_term(|t| t.mode().contains(TermMode::ALT_SCREEN) && t.mode().contains(TermMode::ALTERNATE_SCROLL))
            .unwrap_or(false);
        if alternate {
            // Full-screen programs (less, vim) scroll with the arrow keys.
            let key: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
            let bytes: Vec<u8> = key.iter().copied().cycle().take(key.len() * lines.unsigned_abs() as usize).collect();
            if let Some(sender) = self.ivars().sender.get() {
                let _ = sender.send(Msg::Input(Cow::Owned(bytes)));
            }
        } else {
            self.with_term(|t| t.scroll_display(Scroll::Delta(lines)));
            self.setNeedsDisplay(true);
        }
    }

    fn copy_selection(&self) {
        if let Some(text) = self.with_term(|t| t.selection_to_string()).flatten().filter(|t| !t.is_empty()) {
            copy_text(&text);
        } else {
            objc2_app_kit::NSBeep();
        }
    }

    fn paste_text(&self) {
        let pasteboard = NSPasteboard::generalPasteboard();
        let Some(text) = pasteboard.stringForType(unsafe { objc2_app_kit::NSPasteboardTypeString }) else { return };
        let text = text.to_string().replace("\r\n", "\r").replace('\n', "\r");
        let bracketed = self.with_term(|t| t.mode().contains(TermMode::BRACKETED_PASTE)).unwrap_or(false);
        let bytes = if bracketed { format!("\x1b[200~{text}\x1b[201~") } else { text };
        self.write(bytes.into_bytes());
    }

    fn key_equivalent(&self, event: &NSEvent) -> bool {
        let focused = self.window().and_then(|w| w.firstResponder()).is_some_and(|r| {
            std::ptr::eq(Retained::as_ptr(&r).cast::<AnyObject>(), (self as *const Self).cast::<AnyObject>())
        });
        let flags = event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
        if !focused || flags != NSEventModifierFlags::Command {
            return false;
        }
        match event.charactersIgnoringModifiers().map(|c| c.to_string()).as_deref() {
            Some("c") => self.copy_selection(),
            Some("v") => self.paste_text(),
            // Clear: the screen and what scrolled off it.
            Some("k") => {
                self.with_term(|t| {
                    t.grid_mut().clear_history();
                    t.selection = None;
                });
                self.write(b"\x0c".to_vec());
            }
            _ => return false,
        }
        true
    }

    fn key(&self, event: &NSEvent) {
        let app_cursor = self.with_term(|t| t.mode().contains(TermMode::APP_CURSOR)).unwrap_or(false);
        if let Some(bytes) = key_bytes(event, app_cursor) {
            self.with_term(|t| t.selection = None);
            self.write(bytes);
        }
    }

    /// Whether the shell waits for a command (no program runs in it).
    fn shell_idle(&self) -> bool {
        let fd = self.ivars().master_fd.get();
        if fd < 0 {
            return false;
        }
        let group = unsafe { libc::tcgetpgrp(fd) };
        if group <= 0 {
            return false;
        }
        let mut name = [0u8; 256];
        let length = unsafe { libc::proc_name(group, name.as_mut_ptr().cast(), name.len() as u32) };
        if length <= 0 {
            return false;
        }
        let name = String::from_utf8_lossy(&name[..length as usize]);
        let name = name.trim_start_matches('-');
        matches!(name, "zsh" | "bash" | "sh" | "fish" | "tcsh" | "csh" | "ksh" | "dash" | "nu" | "elvish" | "xonsh")
    }

    /// Changes the shell's folder (if it waits for a command).
    fn follow(&self, folder: &Path) {
        if *self.ivars().folder.borrow() == folder || !self.shell_idle() {
            return;
        }
        *self.ivars().folder.borrow_mut() = folder.to_path_buf();
        // The end of the line, then clear it (anything typed but not run),
        // then the cd (a leading space keeps it out of the history where
        // the shell is set up for that).
        let quoted = format!("'{}'", folder.to_string_lossy().replace('\'', "'\\''"));
        self.write(format!("\x05\x15 cd -- {quoted}\r").into_bytes());
    }

    fn draw(&self) {
        let Some(term) = self.ivars().term.get() else { return };
        let (Some(font), Some(bold)) = (self.ivars().font.get(), self.ivars().bold.get()) else { return };
        let (width, height) = self.ivars().cell.get();
        let dark = self.effectiveAppearance().name().to_string().contains("Dark");
        let palette = Palette::new(dark);
        palette.background.setFill();
        NSBezierPath::fillRect(self.bounds());
        let term = term.lock();
        let content = term.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let selection = content.selection;
        let cursor = content.cursor;
        // Runs of cells that look alike, per row: drawn as one string.
        struct Run {
            row: i32,
            column: usize,
            text: String,
            fg: Rgb,
            bg: Option<Rgb>,
            bold: bool,
            underline: bool,
        }
        let mut runs: Vec<Run> = Vec::new();
        for indexed in content.display_iter {
            let cell = indexed.cell;
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let point = indexed.point;
            let row = point.line.0 + offset;
            let mut fg = palette.resolve(cell.fg, colors, true);
            let mut bg = palette.resolve(cell.bg, colors, false);
            if cell.flags.contains(Flags::DIM) {
                fg = Rgb { r: fg.r / 3 * 2, g: fg.g / 3 * 2, b: fg.b / 3 * 2 };
            }
            let selected = selection.is_some_and(|s| s.contains(point));
            if cell.flags.contains(Flags::INVERSE) != selected {
                std::mem::swap(&mut fg, &mut bg);
            }
            let bg = (bg != palette.bg_rgb).then_some(bg);
            let text = if cell.flags.contains(Flags::HIDDEN) { ' ' } else { cell.c };
            let is_bold = cell.flags.contains(Flags::BOLD);
            let underline = cell.flags.contains(Flags::UNDERLINE);
            match runs.last_mut() {
                Some(run)
                    if run.row == row
                        && run.fg == fg
                        && run.bg == bg
                        && run.bold == is_bold
                        && run.underline == underline
                        && run.column + run.text.chars().count() == point.column.0 =>
                {
                    run.text.push(text);
                }
                _ => runs.push(Run {
                    row,
                    column: point.column.0,
                    text: text.to_string(),
                    fg,
                    bg,
                    bold: is_bold,
                    underline,
                }),
            }
        }
        for run in &runs {
            let x = PAD + run.column as f64 * width;
            let y = PAD + f64::from(run.row) * height;
            if let Some(bg) = run.bg {
                rgb(bg).setFill();
                NSBezierPath::fillRect(NSRect::new(
                    NSPoint::new(x, y),
                    NSSize::new(run.text.chars().count() as f64 * width, height),
                ));
            }
            if run.text.trim().is_empty() {
                continue;
            }
            let attributed = NSMutableAttributedString::new();
            attributed.replaceCharactersInRange_withString(NSRange::new(0, 0), &ns(&run.text));
            let all = NSRange::new(0, attributed.length());
            unsafe {
                attributed.addAttribute_value_range(
                    objc2_app_kit::NSFontAttributeName,
                    if run.bold { bold } else { font },
                    all,
                );
                attributed.addAttribute_value_range(objc2_app_kit::NSForegroundColorAttributeName, &rgb(run.fg), all);
                if run.underline {
                    attributed.addAttribute_value_range(
                        objc2_app_kit::NSUnderlineStyleAttributeName,
                        &objc2_foundation::NSNumber::new_isize(1),
                        all,
                    );
                }
                let _: () = msg_send![&attributed, drawAtPoint: NSPoint::new(x, y)];
            }
        }
        // The cursor: a block when the terminal has the focus, an outline
        // when not.
        let focused = self.window().and_then(|w| w.firstResponder()).is_some_and(|r| {
            std::ptr::eq(Retained::as_ptr(&r).cast::<AnyObject>(), (self as *const Self).cast::<AnyObject>())
        });
        if cursor.shape != CursorShape::Hidden {
            let row = cursor.point.line.0 + offset;
            let rect = NSRect::new(
                NSPoint::new(PAD + cursor.point.column.0 as f64 * width, PAD + f64::from(row) * height),
                NSSize::new(width, height),
            );
            let color = palette.cursor.colorWithAlphaComponent(if focused { 0.55 } else { 0.9 });
            color.set();
            match (focused, cursor.shape) {
                (true, CursorShape::Beam) => NSBezierPath::fillRect(NSRect::new(rect.origin, NSSize::new(2.0, height))),
                (true, CursorShape::Underline) => NSBezierPath::fillRect(NSRect::new(
                    NSPoint::new(rect.origin.x, rect.origin.y + height - 2.0),
                    NSSize::new(width, 2.0),
                )),
                (true, _) => NSBezierPath::fillRect(rect),
                (false, _) => NSBezierPath::strokeRect(rect),
            }
        }
    }
}

/// Colors: the 16 ANSI colors (as VS Code's terminal has them), the 256
/// color cube, and the background, text, and cursor for light or dark.
struct Palette {
    ansi: [Rgb; 16],
    fg_rgb: Rgb,
    bg_rgb: Rgb,
    background: Retained<NSColor>,
    cursor: Retained<NSColor>,
}

impl Palette {
    fn new(dark: bool) -> Palette {
        let c = |r, g, b| Rgb { r, g, b };
        let ansi = [
            c(0, 0, 0),
            c(205, 49, 49),
            c(13, 188, 121),
            if dark { c(229, 229, 16) } else { c(148, 148, 0) },
            c(36, 114, 200),
            c(188, 63, 188),
            c(17, 168, 205),
            if dark { c(229, 229, 229) } else { c(85, 85, 85) },
            c(102, 102, 102),
            c(241, 76, 76),
            c(35, 209, 139),
            if dark { c(245, 245, 67) } else { c(178, 178, 0) },
            c(59, 142, 234),
            c(214, 112, 214),
            c(41, 184, 219),
            if dark { c(255, 255, 255) } else { c(40, 40, 40) },
        ];
        let (fg_rgb, bg_rgb) = if dark { (c(220, 220, 220), c(30, 30, 30)) } else { (c(30, 30, 30), c(255, 255, 255)) };
        Palette { ansi, fg_rgb, bg_rgb, background: rgb(bg_rgb), cursor: rgb(fg_rgb) }
    }

    /// A cell's color as RGB (the terminal's own colors, if a program set
    /// them, first).
    fn resolve(&self, color: Color, colors: &Colors, foreground: bool) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(index) => colors[index as usize].unwrap_or_else(|| self.indexed(index)),
            Color::Named(name) => colors[name as usize].unwrap_or_else(|| match name {
                NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::DimForeground => self.fg_rgb,
                NamedColor::Background => self.bg_rgb,
                NamedColor::Cursor => self.fg_rgb,
                other => {
                    let index = other as usize;
                    if index < 16 {
                        self.ansi[index]
                    } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&index) {
                        let base = self.ansi[index - NamedColor::DimBlack as usize];
                        Rgb { r: base.r / 3 * 2, g: base.g / 3 * 2, b: base.b / 3 * 2 }
                    } else if foreground {
                        self.fg_rgb
                    } else {
                        self.bg_rgb
                    }
                }
            }),
        }
    }

    fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                Rgb { r: level(i / 36), g: level(i / 6 % 6), b: level(i % 6) }
            }
            _ => {
                let gray = 8 + (index - 232) * 10;
                Rgb { r: gray, g: gray, b: gray }
            }
        }
    }
}

fn rgb(color: Rgb) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(
        f64::from(color.r) / 255.0,
        f64::from(color.g) / 255.0,
        f64::from(color.b) / 255.0,
        1.0,
    )
}

/// The size of one character cell of a monospaced font.
fn cell_size(font: &NSFont) -> (f64, f64) {
    let attributed = NSMutableAttributedString::new();
    attributed.replaceCharactersInRange_withString(NSRange::new(0, 0), &ns("MMMMMMMMMM"));
    unsafe {
        attributed.addAttribute_value_range(objc2_app_kit::NSFontAttributeName, font, NSRange::new(0, 10));
    }
    let size: NSSize = unsafe { msg_send![&attributed, size] };
    let height = (font.ascender() - font.descender() + font.leading()).ceil() + 2.0;
    ((size.width / 10.0).ceil(), height)
}

fn window_size(columns: usize, lines: usize, cell: (f64, f64)) -> WindowSize {
    WindowSize {
        num_lines: lines as u16,
        num_cols: columns as u16,
        cell_width: cell.0 as u16,
        cell_height: cell.1 as u16,
    }
}

/// What a key sends to the terminal (None: not the terminal's).
fn key_bytes(event: &NSEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let flags = event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
    if flags.contains(NSEventModifierFlags::Command) {
        return None;
    }
    let control = flags.contains(NSEventModifierFlags::Control);
    let option = flags.contains(NSEventModifierFlags::Option);
    let arrow = |code: u8| -> Vec<u8> {
        if option {
            // Option-Left and Option-Right: a word back or forward.
            match code {
                b'D' => b"\x1bb".to_vec(),
                b'C' => b"\x1bf".to_vec(),
                _ => vec![0x1b, b'[', code],
            }
        } else if app_cursor {
            vec![0x1b, b'O', code]
        } else {
            vec![0x1b, b'[', code]
        }
    };
    let bytes = match event.keyCode() {
        123 => arrow(b'D'),
        124 => arrow(b'C'),
        125 => arrow(b'B'),
        126 => arrow(b'A'),
        36 | 76 => b"\r".to_vec(),
        51 if option => b"\x1b\x7f".to_vec(),
        51 => b"\x7f".to_vec(),
        117 => b"\x1b[3~".to_vec(),
        48 if flags.contains(NSEventModifierFlags::Shift) => b"\x1b[Z".to_vec(),
        48 => b"\t".to_vec(),
        53 => b"\x1b".to_vec(),
        115 => b"\x1b[H".to_vec(),
        119 => b"\x1b[F".to_vec(),
        116 => b"\x1b[5~".to_vec(),
        121 => b"\x1b[6~".to_vec(),
        _ => {
            if control {
                let key = event.charactersIgnoringModifiers()?.to_string();
                let c = key.chars().next()?;
                return match c {
                    'a'..='z' | 'A'..='Z' => Some(vec![(c.to_ascii_lowercase() as u8) & 0x1f]),
                    '@' | ' ' | '2' => Some(vec![0]),
                    '[' | '3' => Some(vec![0x1b]),
                    '\\' | '4' => Some(vec![0x1c]),
                    ']' | '5' => Some(vec![0x1d]),
                    '^' | '6' => Some(vec![0x1e]),
                    '_' | '-' | '7' => Some(vec![0x1f]),
                    _ => None,
                };
            }
            event.characters()?.to_string().into_bytes()
        }
    };
    (!bytes.is_empty()).then_some(bytes)
}

/// The terminal view, started in `folder` the first time.
pub fn view_for(folder: &Path, frame: NSRect, mtm: MainThreadMarker) -> std::io::Result<Retained<TerminalView>> {
    if let Some(existing) = TERMINAL.with(|t| t.borrow().clone()) {
        return Ok(existing);
    }
    let made = TerminalView::new(folder, frame, mtm)?;
    TERMINAL.with(|t| *t.borrow_mut() = Some(made.clone()));
    Ok(made)
}

/// The terminal, if it was started.
pub fn current() -> Option<Retained<TerminalView>> {
    TERMINAL.with(|t| t.borrow().clone())
}

/// The shell ended: the terminal is forgotten (the next one starts anew).
pub fn forget() {
    if let Some(terminal) = TERMINAL.with(|t| t.borrow_mut().take()) {
        if let Some(sender) = terminal.ivars().sender.get() {
            let _ = sender.send(Msg::Shutdown);
        }
        terminal.removeFromSuperview();
    }
}

/// The active pane went to another folder (on this Mac).
pub fn follow(folder: &Path) {
    if let Some(terminal) = current()
        && !terminal.isHiddenOrHasHiddenAncestor()
    {
        terminal.follow(folder);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_a_full_palette() {
        let palette = Palette::new(true);
        let colors = Colors::default();
        assert_eq!(palette.resolve(Color::Named(NamedColor::Red), &colors, true), palette.ansi[1]);
        assert_eq!(palette.resolve(Color::Named(NamedColor::Foreground), &colors, true), palette.fg_rgb);
        assert_eq!(palette.resolve(Color::Indexed(16), &colors, true), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(palette.resolve(Color::Indexed(231), &colors, true), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(palette.resolve(Color::Indexed(232), &colors, true), Rgb { r: 8, g: 8, b: 8 });
        let spec = Rgb { r: 1, g: 2, b: 3 };
        assert_eq!(palette.resolve(Color::Spec(spec), &colors, false), spec);
    }
}
