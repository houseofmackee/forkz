//! The color theme in use (Settings > Appearance), as AppKit colors, and the
//! views that draw with it: rows (background, alternate rows, selection)
//! and cells (text that turns to the selected-text color when selected).
//! Without a theme (the macOS choices), everything keeps macOS's colors.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackgroundStyle, NSBezierPath, NSColor, NSResponder, NSTableCellView, NSTableRowView,
    NSTableViewSelectionHighlightStyle, NSView,
};
use objc2_foundation::NSRect;

use crate::theme::{Rgb, Theme};

#[derive(Clone)]
pub struct Colors {
    pub background: Retained<NSColor>,
    pub alternate: Retained<NSColor>,
    pub text: Retained<NSColor>,
    pub secondary: Retained<NSColor>,
    pub selection: Retained<NSColor>,
    pub selection_text: Retained<NSColor>,
    pub accent: Retained<NSColor>,
    pub sidebar: Retained<NSColor>,
    /// Alternating row colors (a setting).
    pub stripes: bool,
}

pub fn color(rgb: Rgb) -> Retained<NSColor> {
    let (r, g, b) = rgb.unit();
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
}

/// An AppKit color as sRGB (for the theme editor).
pub fn rgb_of(color: &NSColor) -> Option<Rgb> {
    let srgb = color.colorUsingColorSpace(&objc2_app_kit::NSColorSpace::sRGBColorSpace())?;
    Some(Rgb::from_unit(srgb.redComponent(), srgb.greenComponent(), srgb.blueComponent()))
}

thread_local! {
    static COLORS: RefCell<Option<Colors>> = const { RefCell::new(None) };
    /// Changes with the theme, so lists make new cells.
    static VERSION: Cell<u32> = const { Cell::new(0) };
}

/// Sets the theme (None: macOS's colors).
pub fn set(theme: Option<&Theme>, stripes: bool) {
    let colors = theme.map(|t| Colors {
        background: color(t.background),
        alternate: color(t.alternate),
        text: color(t.text),
        secondary: color(t.secondary),
        selection: color(t.selection),
        selection_text: color(t.selection_text),
        accent: color(t.accent),
        sidebar: color(t.sidebar),
        stripes,
    });
    COLORS.with(|c| *c.borrow_mut() = colors);
    VERSION.with(|v| v.set(v.get() + 1));
}

pub fn colors() -> Option<Colors> {
    COLORS.with(|c| c.borrow().clone())
}

pub fn version() -> u32 {
    VERSION.with(Cell::get)
}

define_class!(
    /// A list row in theme colors.
    #[unsafe(super(NSTableRowView, NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzThemedRowView"]
    #[ivars = Cell<bool>]
    pub struct ThemedRowView;

    impl ThemedRowView {
        #[unsafe(method(drawBackgroundInRect:))]
        fn draw_background(&self, rect: NSRect) {
            match colors() {
                Some(c) => {
                    let odd = self.ivars().get() && c.stripes;
                    if odd { c.alternate.setFill() } else { c.background.setFill() }
                    NSBezierPath::fillRect(rect);
                }
                None => unsafe { msg_send![super(self), drawBackgroundInRect: rect] },
            }
        }

        #[unsafe(method(drawSelectionInRect:))]
        fn draw_selection(&self, rect: NSRect) {
            match colors() {
                Some(c) if self.selectionHighlightStyle() != NSTableViewSelectionHighlightStyle::None => {
                    // The other pane's selection is quieter.
                    let fill = if self.isEmphasized() { c.selection.clone() } else { c.selection.colorWithAlphaComponent(0.45) };
                    fill.setFill();
                    NSBezierPath::fillRect(self.bounds());
                }
                Some(_) => {}
                None => unsafe { msg_send![super(self), drawSelectionInRect: rect] },
            }
        }
    }
);

impl ThemedRowView {
    /// A row view; `odd` rows get the alternate color.
    pub fn new(mtm: MainThreadMarker, odd: bool) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Cell::new(odd));
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }
}

define_class!(
    /// A list cell whose text follows the theme, also when selected.
    #[unsafe(super(NSTableCellView, NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzThemedCell"]
    #[ivars = Cell<bool>]
    pub struct ThemedCell;

    impl ThemedCell {
        #[unsafe(method(setBackgroundStyle:))]
        fn set_background_style(&self, style: NSBackgroundStyle) {
            let _: () = unsafe { msg_send![super(self), setBackgroundStyle: style] };
            self.recolor(style == NSBackgroundStyle::Emphasized);
        }
    }
);

impl ThemedCell {
    /// A cell; `secondary` cells (dates, sizes) use the quieter text color.
    pub fn new(mtm: MainThreadMarker, frame: NSRect, secondary: bool) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Cell::new(secondary));
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// The text color for the theme (macOS's colors handle themselves).
    pub fn recolor(&self, selected: bool) {
        let Some(c) = colors() else { return };
        let Some(field) = (unsafe { self.textField() }) else { return };
        let color = if selected {
            &c.selection_text
        } else if self.ivars().get() {
            &c.secondary
        } else {
            &c.text
        };
        field.setTextColor(Some(color));
    }
}
