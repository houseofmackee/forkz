//! Gallery view (View > as Gallery): a large preview of the selected item,
//! its name and details, and below them a strip of the folder's items (an
//! icon view in one row). As with the other views, the list stays
//! underneath (hidden) and follows the selection.

use std::cell::RefCell;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSFont, NSImageScaling, NSImageView, NSLayoutConstraintOrientation, NSLineBreakMode, NSScrollView, NSStackView,
    NSTextAlignment, NSTextField, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRect, NSSize};
use objc2_quick_look_ui::{QLPreviewItem, QLPreviewView, QLPreviewViewStyle};

use super::icon_view::IconView;
use super::pane_view::PaneController;
use super::util::{file_url, icon_of, ns};
use crate::fs::Entry;

pub struct Gallery {
    /// What the pane shows.
    pub view: Retained<NSView>,
    pub strip: Retained<IconView>,
    /// Quick Look, for items on this Mac (None if it is not available).
    quick_look: Option<Retained<QLPreviewView>>,
    /// A large icon instead, for items on servers and in archives.
    picture: Retained<NSImageView>,
    name: Retained<NSTextField>,
    details: Retained<NSTextField>,
    /// The item previewed, so choosing it again does not reload it.
    shown: RefCell<Option<PathBuf>>,
}

/// Pins `view` to all edges of `parent`.
fn fill(parent: &NSView, view: &NSView, inset: f64) {
    parent.addSubview(view);
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.leadingAnchor().constraintEqualToAnchor_constant(&parent.leadingAnchor(), inset).setActive(true);
    view.trailingAnchor().constraintEqualToAnchor_constant(&parent.trailingAnchor(), -inset).setActive(true);
    view.topAnchor().constraintEqualToAnchor_constant(&parent.topAnchor(), inset).setActive(true);
    view.bottomAnchor().constraintEqualToAnchor_constant(&parent.bottomAnchor(), -inset).setActive(true);
}

fn label(mtm: MainThreadMarker, font: &NSFont) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&ns(""), mtm);
    label.setFont(Some(font));
    label.setAlignment(NSTextAlignment::Center);
    label.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
    if let Some(colors) = super::themed::colors() {
        label.setTextColor(Some(&colors.text));
    }
    label
}

impl Gallery {
    pub fn new(mtm: MainThreadMarker, pane: &PaneController) -> Gallery {
        let (text, _) = super::pane_view::look_metrics();

        // The preview: Quick Look, with a large icon in the same place for
        // what Quick Look cannot reach.
        let preview = NSView::new(mtm);
        let quick_look = unsafe {
            QLPreviewView::initWithFrame_style(
                QLPreviewView::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(300.0, 300.0)),
                QLPreviewViewStyle::Normal,
            )
        };
        if let Some(quick_look) = &quick_look {
            unsafe { quick_look.setShouldCloseWithWindow(false) };
            fill(&preview, quick_look, 8.0);
        }
        let picture = NSImageView::new(mtm);
        picture.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        // Not a drop target for images (the pane takes drops).
        picture.unregisterDraggedTypes();
        fill(&preview, &picture, 24.0);
        picture.setHidden(true);

        let name = label(mtm, &NSFont::boldSystemFontOfSize(text + 1.0));
        let details = label(mtm, &NSFont::systemFontOfSize(text - 1.0));
        if super::themed::colors().is_none() {
            details.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        }
        let info = NSStackView::new(mtm);
        info.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        info.setSpacing(2.0);
        info.setEdgeInsets(NSEdgeInsets { top: 4.0, left: 12.0, bottom: 6.0, right: 12.0 });
        // Only as tall as the name and details: the preview takes the rest.
        info.setHuggingPriority_forOrientation(750.0, NSLayoutConstraintOrientation::Vertical);
        info.addArrangedSubview(&name);
        info.addArrangedSubview(&details);
        for field in [&name, &details] {
            field.widthAnchor().constraintLessThanOrEqualToAnchor_constant(&info.widthAnchor(), -24.0).setActive(true);
            field
                .setContentCompressionResistancePriority_forOrientation(1.0, NSLayoutConstraintOrientation::Horizontal);
        }

        let strip = IconView::new(mtm, pane, true);
        unsafe { strip.setMenu(pane.table().menu().as_deref()) };
        let strip_scroll: Retained<NSScrollView> = super::icon_view::scroll_view(&strip, mtm);
        strip_scroll.heightAnchor().constraintEqualToConstant(super::icon_view::strip_height()).setActive(true);

        // The strip along the bottom, the name and details above it, and
        // the preview in the rest.
        let view = NSView::new(mtm);
        for part in [&*preview, &*info, &*strip_scroll as &NSView] {
            view.addSubview(part);
            part.setTranslatesAutoresizingMaskIntoConstraints(false);
            part.leadingAnchor().constraintEqualToAnchor(&view.leadingAnchor()).setActive(true);
            part.trailingAnchor().constraintEqualToAnchor(&view.trailingAnchor()).setActive(true);
        }
        strip_scroll.bottomAnchor().constraintEqualToAnchor(&view.bottomAnchor()).setActive(true);
        info.bottomAnchor().constraintEqualToAnchor(&strip_scroll.topAnchor()).setActive(true);
        preview.bottomAnchor().constraintEqualToAnchor(&info.topAnchor()).setActive(true);
        preview.topAnchor().constraintEqualToAnchor(&view.topAnchor()).setActive(true);
        preview.heightAnchor().constraintGreaterThanOrEqualToConstant(120.0).setActive(true);
        // As tall as the pane allows (a stack view keeps a view with a
        // height of its own at that height otherwise).
        let tall = view.heightAnchor().constraintEqualToConstant(10_000.0);
        tall.setPriority(300.0);
        tall.setActive(true);
        Gallery { view, strip, quick_look, picture, name, details, shown: RefCell::new(None) }
    }

    /// Shows the pane's items and selection, and previews the selection.
    pub fn sync(&self, entries: &[Entry], local: bool, selected: &[PathBuf]) {
        self.strip.sync(entries.to_vec(), local, selected);
        self.show_selection(entries, local, selected);
    }

    /// Previews the first selected item (in the order shown).
    pub fn show_selection(&self, entries: &[Entry], local: bool, selected: &[PathBuf]) {
        let entry = entries.iter().find(|e| selected.contains(&e.path));
        let path = entry.map(|e| e.path.clone());
        if *self.shown.borrow() == path && path.is_some() {
            return;
        }
        *self.shown.borrow_mut() = path.clone();
        let Some(entry) = entry else {
            self.set_quick_look(None);
            self.picture.setHidden(true);
            self.name.setStringValue(&ns(""));
            self.details.setStringValue(&ns(""));
            return;
        };
        // Quick Look reads files on this Mac; for folders (which it shows
        // empty) and everything else, the icon.
        if local && self.quick_look.is_some() && !entry.is_folder_like() {
            self.set_quick_look(Some(&entry.path));
            self.picture.setHidden(true);
        } else {
            self.set_quick_look(None);
            let icon = icon_of(entry, local);
            let copy: Retained<objc2_app_kit::NSImage> = unsafe { objc2::msg_send![&icon, copy] };
            copy.setSize(NSSize::new(256.0, 256.0));
            self.picture.setImage(Some(&copy));
            self.picture.setHidden(false);
        }
        self.name.setStringValue(&ns(&entry.name));
        self.name.setToolTip(Some(&ns(&entry.name)));
        self.details.setStringValue(&ns(&details_of(entry)));
    }

    fn set_quick_look(&self, path: Option<&std::path::Path>) {
        if let Some(quick_look) = &self.quick_look {
            let item = path.map(|p| ProtocolObject::<dyn QLPreviewItem>::from_retained(file_url(p)));
            unsafe { quick_look.setPreviewItem(item.as_deref()) };
            quick_look.setHidden(path.is_none());
        }
    }

    /// The text size or theme changed.
    pub fn apply_look(&self) {
        self.strip.apply_look();
        // Shown again with the new look.
        self.shown.borrow_mut().take();
    }
}

/// "PNG image · 2.4 MB · Today at 10:20" (no size for folders).
fn details_of(entry: &Entry) -> String {
    let mut parts = vec![super::util::kind_of(entry)];
    if !entry.is_folder_like() {
        parts.push(crate::fs::format_size(entry.size));
    }
    let date = super::util::format_date(entry.modified);
    if !date.is_empty() {
        parts.push(date);
    }
    parts.join("  \u{b7}  ")
}
