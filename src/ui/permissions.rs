//! The Permissions dialog: read, write, and execute for owner, group, and
//! others (kept in step with an octal field), optionally applied to what is
//! inside folders. Works on the Mac and on servers.

use std::cell::OnceCell;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSButton, NSControlStateValueOff, NSControlStateValueOn,
    NSControlTextEditingDelegate, NSGridCellPlacement, NSGridView, NSPopUpButton, NSTextField, NSTextFieldDelegate,
    NSView,
};
use objc2_foundation::{NSArray, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use super::util::{mtm, ns};

/// Which items inside folders get the new permissions too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Recurse {
    None,
    All,
    FoldersOnly,
    FilesOnly,
}

pub struct Choice {
    pub mode: u32,
    pub recurse: Recurse,
}

/// The bits of the nine checkboxes, owner read first.
const BITS: [u32; 9] = [0o400, 0o200, 0o100, 0o040, 0o020, 0o010, 0o004, 0o002, 0o001];

struct Controls {
    boxes: Vec<Retained<NSButton>>,
    octal: Retained<NSTextField>,
    /// Bits the checkboxes do not show (setuid, setgid, sticky), kept.
    special: std::cell::Cell<u32>,
}

pub struct TargetIvars {
    controls: OnceCell<Controls>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzPermissionsTarget"]
    #[ivars = TargetIvars]
    struct Target;

    impl Target {
        #[unsafe(method(boxChanged:))]
        fn box_changed(&self, _sender: Option<&AnyObject>) {
            self.update_octal();
        }

    }

    unsafe impl NSObjectProtocol for Target {}

    // Typing in the octal field updates the checkboxes as you type.
    unsafe impl NSControlTextEditingDelegate for Target {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            self.update_boxes();
        }
    }

    unsafe impl NSTextFieldDelegate for Target {}
);

impl Target {
    fn mode(&self) -> u32 {
        let Some(controls) = self.ivars().controls.get() else { return 0 };
        let boxes = controls.boxes.iter().zip(BITS).filter(|(b, _)| b.state() == NSControlStateValueOn);
        boxes.fold(controls.special.get(), |mode, (_, bit)| mode | bit)
    }

    fn update_octal(&self) {
        if let Some(controls) = self.ivars().controls.get() {
            controls.octal.setStringValue(&ns(&format!("{:03o}", self.mode())));
        }
    }

    fn update_boxes(&self) {
        let Some(controls) = self.ivars().controls.get() else { return };
        let Some(mode) = parse_octal(&controls.octal.stringValue().to_string()) else { return };
        controls.special.set(mode & 0o7000);
        for (checkbox, bit) in controls.boxes.iter().zip(BITS) {
            checkbox.setState(if mode & bit != 0 { NSControlStateValueOn } else { NSControlStateValueOff });
        }
    }
}

/// "755", "0644", "4755"; None if it is not 3 or 4 octal digits.
pub fn parse_octal(text: &str) -> Option<u32> {
    let text = text.trim();
    let text = if text.len() == 4 && text.starts_with('0') { &text[1..] } else { text };
    if !(3..=4).contains(&text.len()) || !text.chars().all(|c| ('0'..='7').contains(&c)) {
        return None;
    }
    u32::from_str_radix(text, 8).ok()
}

fn view<T: Message + AsRef<NSView>>(v: &T) -> Retained<NSView> {
    v.as_ref().retain()
}

fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSView> {
    Retained::into_super(Retained::into_super(NSTextField::labelWithString(&ns(text), mtm)))
}

/// Asks for new permissions. `mode` is the first item's; `title` names
/// what changes; `has_folders` offers applying to what is inside.
pub fn ask(title: &str, mode: u32, has_folders: bool) -> Option<Choice> {
    let mtm = mtm();
    let target: Retained<Target> = {
        let this = Target::alloc(mtm).set_ivars(TargetIvars { controls: OnceCell::new() });
        unsafe { msg_send![super(this), init] }
    };
    let boxes: Vec<Retained<NSButton>> = BITS
        .iter()
        .map(|bit| {
            let checkbox = unsafe {
                NSButton::checkboxWithTitle_target_action(&ns(""), Some(&target), Some(sel!(boxChanged:)), mtm)
            };
            checkbox.setState(if mode & bit != 0 { NSControlStateValueOn } else { NSControlStateValueOff });
            checkbox
        })
        .collect();
    let octal = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(60.0, 22.0)),
    );
    octal.setStringValue(&ns(&format!("{:03o}", mode & 0o7777)));
    octal.widthAnchor().constraintEqualToConstant(60.0).setActive(true);
    // Live updates through the delegate; no action, so Return presses Apply.
    unsafe { octal.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*target))) };

    let mut rows: Vec<Retained<NSArray<NSView>>> = vec![NSArray::from_retained_slice(&[
        label(mtm, ""),
        label(mtm, "Read"),
        label(mtm, "Write"),
        label(mtm, "Execute"),
    ])];
    for (index, who) in ["Owner:", "Group:", "Others:"].iter().enumerate() {
        let mut row = vec![label(mtm, who)];
        row.extend(boxes[index * 3..index * 3 + 3].iter().map(|b| view(&**b)));
        rows.push(NSArray::from_retained_slice(&row));
    }
    rows.push(NSArray::from_retained_slice(&[label(mtm, "Octal:"), view(&*octal), label(mtm, ""), label(mtm, "")]));
    let recurse_popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    let recurse_choices = [
        ("This item only", Recurse::None),
        ("Also everything inside", Recurse::All),
        ("Also folders inside", Recurse::FoldersOnly),
        ("Also files inside", Recurse::FilesOnly),
    ];
    if has_folders {
        let titles: Vec<Retained<NSString>> = recurse_choices.iter().map(|c| ns(c.0)).collect();
        recurse_popup.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
        rows.push(NSArray::from_retained_slice(&[
            label(mtm, "Apply to:"),
            view(&*recurse_popup),
            label(mtm, ""),
            label(mtm, ""),
        ]));
    }
    let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
    grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
    for column in 1..4 {
        grid.columnAtIndex(column).setXPlacement(NSGridCellPlacement::Center);
    }
    let octal_row = 4;
    grid.cellAtColumnIndex_rowIndex(1, octal_row).setXPlacement(NSGridCellPlacement::Leading);
    if has_folders {
        grid.mergeCellsInHorizontalRange_verticalRange(
            objc2_foundation::NSRange::new(1, 3),
            objc2_foundation::NSRange::new(5, 1),
        );
        grid.cellAtColumnIndex_rowIndex(1, 5).setXPlacement(NSGridCellPlacement::Leading);
    }
    grid.setRowSpacing(8.0);
    grid.setColumnSpacing(16.0);
    grid.setFrameSize(grid.fittingSize());

    let _ = target.ivars().controls.set(Controls {
        boxes: boxes.clone(),
        octal: octal.clone(),
        special: std::cell::Cell::new(mode & 0o7000),
    });

    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(title));
    alert.setInformativeText(&ns("Servers may refuse some changes (for example on files you do not own)."));
    alert.addButtonWithTitle(&ns("Apply"));
    alert.addButtonWithTitle(&ns("Cancel"));
    alert.setAccessoryView(Some(&grid));
    alert.layout();
    NSApplication::sharedApplication(mtm).activate();
    if alert.runModal() != NSAlertFirstButtonReturn {
        return None;
    }
    // A typed octal value counts even if Return was not pressed in the field.
    target.update_boxes();
    let recurse = if has_folders {
        recurse_choices.get(recurse_popup.indexOfSelectedItem().max(0) as usize).map_or(Recurse::None, |c| c.1)
    } else {
        Recurse::None
    };
    Some(Choice { mode: target.mode(), recurse })
}

/// Applies a choice to items (and what is inside folders, if asked), in the
/// calling thread. Returns the items that could not be changed.
pub fn apply(fs: &crate::vfs::FsRef, paths: &[PathBuf], choice: &Choice) -> Vec<(PathBuf, String)> {
    let mut failed = Vec::new();
    for path in paths {
        apply_one(fs, path, choice, true, &mut failed);
    }
    failed
}

fn apply_one(
    fs: &crate::vfs::FsRef,
    path: &std::path::Path,
    choice: &Choice,
    top: bool,
    failed: &mut Vec<(PathBuf, String)>,
) {
    use crate::fs::EntryKind;
    let Ok(entry) = fs.stat(path) else { return };
    // Links are left alone (changing them changes what they point to).
    if matches!(entry.kind, EntryKind::Symlink { .. }) {
        return;
    }
    let folder = entry.kind == EntryKind::Folder;
    let wanted = top
        || match choice.recurse {
            Recurse::None => false,
            Recurse::All => true,
            Recurse::FoldersOnly => folder,
            Recurse::FilesOnly => !folder,
        };
    let change = |failed: &mut Vec<(PathBuf, String)>| {
        if wanted && let Err(e) = fs.set_attributes(path, None, Some(choice.mode)) {
            failed.push((path.to_path_buf(), e.to_string()));
        }
    };
    if !folder || choice.recurse == Recurse::None {
        change(failed);
        return;
    }
    // A folder stays open to its owner with the new mode: change it first
    // (that may be what lets us in). Otherwise change it last, so what is
    // inside can still be reached.
    let stays_open = choice.mode & 0o500 == 0o500;
    if stays_open {
        change(failed);
    }
    match fs.list(path) {
        Ok(children) => {
            for child in children {
                apply_one(fs, &child.path, choice, false, failed);
            }
        }
        Err(e) => failed.push((path.to_path_buf(), e.to_string())),
    }
    if !stays_open {
        change(failed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octal_values() {
        assert_eq!(parse_octal("755"), Some(0o755));
        assert_eq!(parse_octal(" 0644 "), Some(0o644));
        assert_eq!(parse_octal("4755"), Some(0o4755));
        assert_eq!(parse_octal("78"), None);
        assert_eq!(parse_octal("888"), None);
        assert_eq!(parse_octal("12345"), None);
    }

    #[test]
    fn applies_recursively_by_kind() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("site");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.sh"), "").unwrap();
        std::fs::write(root.join("sub/b.txt"), "").unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let fs = crate::vfs::local();

        let failed = apply(&fs, std::slice::from_ref(&root), &Choice { mode: 0o750, recurse: Recurse::FoldersOnly });
        assert!(failed.is_empty());
        assert_eq!((mode(&root), mode(&root.join("sub"))), (0o750, 0o750));
        assert_eq!(mode(&root.join("a.sh")), 0o644, "files untouched");

        // Closing a folder: what is inside changes first, then the folder.
        apply(&fs, std::slice::from_ref(&root), &Choice { mode: 0o600, recurse: Recurse::FilesOnly });
        assert_eq!(mode(&root), 0o600, "the top item always changes");
        // Opening it again: the folder first, so its contents can be reached.
        let failed = apply(&fs, std::slice::from_ref(&root), &Choice { mode: 0o755, recurse: Recurse::All });
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(mode(&root.join("sub/b.txt")), 0o755);
        assert_eq!(mode(&root.join("sub")), 0o755);
    }
}
