//! Customize… in Settings > Appearance: a theme's name, whether it is dark,
//! and a color well for each of its colors. Saved as a user theme file.

use objc2::rc::Retained;
use objc2::{MainThreadOnly, Message};
use objc2_app_kit::{
    NSAlert, NSApplication, NSColorWell, NSGridCellPlacement, NSGridView, NSPopUpButton, NSTextField, NSView,
};
use objc2_foundation::{NSArray, NSPoint, NSRect, NSSize, NSString};

use super::themed::{color, rgb_of};
use super::util::{mtm, ns};
use crate::theme::{COLORS, Theme};

fn as_view<T: AsRef<NSView>>(view: &T) -> Retained<NSView> {
    view.as_ref().retain()
}

/// Edits a copy of `theme`; None if cancelled.
pub fn edit(theme: &Theme) -> Option<Theme> {
    let mtm = mtm();
    let mut theme = theme.clone();
    let name = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(220.0, 22.0)),
    );
    name.setStringValue(&ns(&theme.name));
    name.widthAnchor().constraintEqualToConstant(220.0).setActive(true);
    let base = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    let titles: Vec<Retained<NSString>> = ["Light", "Dark"].iter().map(|t| ns(t)).collect();
    base.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
    base.selectItemAtIndex(isize::from(theme.dark));
    let wells: Vec<Retained<NSColorWell>> = COLORS
        .iter()
        .map(|(_, field)| {
            let well = NSColorWell::initWithFrame(
                NSColorWell::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(48.0, 24.0)),
            );
            well.setColor(&color(*field(&mut theme)));
            well.widthAnchor().constraintEqualToConstant(48.0).setActive(true);
            well
        })
        .collect();

    let label = |text: &str| as_view(&NSTextField::labelWithString(&ns(text), mtm));
    let mut rows: Vec<Retained<NSArray<NSView>>> = vec![
        NSArray::from_retained_slice(&[label("Name:"), as_view(&name)]),
        NSArray::from_retained_slice(&[label("Base:"), as_view(&base)]),
    ];
    for ((title, _), well) in COLORS.iter().zip(&wells) {
        rows.push(NSArray::from_retained_slice(&[label(&format!("{title}:")), as_view(well)]));
    }
    let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
    grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
    grid.setRowSpacing(8.0);
    grid.setColumnSpacing(8.0);
    grid.setFrameSize(grid.fittingSize());

    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns("Customize Theme"));
    alert.setInformativeText(&ns(
        "Saved as a theme of your own, in forkz\u{2019}s themes folder. A theme with the same name is replaced.",
    ));
    alert.addButtonWithTitle(&ns("Save"));
    alert.addButtonWithTitle(&ns("Cancel"));
    alert.setAccessoryView(Some(&grid));
    alert.layout();
    alert.window().setInitialFirstResponder(Some(&name));
    NSApplication::sharedApplication(mtm).activate();
    loop {
        if alert.runModal() != objc2_app_kit::NSAlertFirstButtonReturn {
            return None;
        }
        let name_text = name.stringValue().to_string().trim().to_string();
        if name_text.is_empty() {
            alert.setInformativeText(&ns("Give the theme a name."));
            continue;
        }
        theme.name = name_text;
        theme.dark = base.indexOfSelectedItem() == 1;
        for ((_, field), well) in COLORS.iter().zip(&wells) {
            if let Some(rgb) = rgb_of(&well.color()) {
                *field(&mut theme) = rgb;
            }
        }
        return Some(theme);
    }
}
