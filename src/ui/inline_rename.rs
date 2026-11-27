//! Renaming in the list itself, as in Finder: press Return, or click the
//! name of the selected item again (a slow double-click). The name becomes
//! a field, with the part before the extension selected; Return or a click
//! elsewhere renames, Escape leaves it as it was. Tab and Shift-Tab rename
//! and go on to the next or previous item, as in Path Finder. Changes to
//! the folder wait until the editing is done.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{NSColor, NSControl, NSControlTextEditingDelegate, NSTextField, NSTextFieldDelegate, NSTextView};
use objc2_foundation::{NSNotification, NSObject, NSRange};

use super::util::{mtm, ns};

pub struct EditIvars {
    pane: usize,
    path: PathBuf,
    name: String,
    field: Retained<NSTextField>,
    cancelled: Cell<bool>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzInlineRename"]
    #[ivars = EditIvars]
    struct InlineRename;

    unsafe impl NSObjectProtocol for InlineRename {}

    unsafe impl NSControlTextEditingDelegate for InlineRename {
        /// Return, Tab, or a click elsewhere: done.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn did_end_editing(&self, _notification: &NSNotification) {
            finish(!self.ivars().cancelled.get());
        }

        /// Escape: leave the name as it was. Tab and Shift-Tab: rename,
        /// then edit the next or previous item's name.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn command(&self, _control: &NSControl, _view: &NSTextView, command: Sel) -> bool {
            let step = if command == sel!(insertTab:) {
                1
            } else if command == sel!(insertBacktab:) {
                -1
            } else {
                0
            };
            let cancel = command == sel!(cancelOperation:);
            if cancel {
                self.ivars().cancelled.set(true);
                finish(false);
            } else if step != 0 {
                finish_and_move(step);
            }
            cancel || step != 0
        }
    }

    unsafe impl NSTextFieldDelegate for InlineRename {}
);

thread_local! {
    static ACTIVE: RefCell<Option<Retained<InlineRename>>> = const { RefCell::new(None) };
    /// The pane's folder changed while a name was edited: list it again
    /// when done.
    static RELOAD_LATER: Cell<bool> = const { Cell::new(false) };
    /// The item to edit next (Tab), once the pane has listed its folder
    /// again after the rename.
    static NEXT: RefCell<Option<(usize, PathBuf)>> = const { RefCell::new(None) };
}

/// Whether a name is being edited in a pane.
pub fn is_editing(pane: usize) -> bool {
    ACTIVE.with(|a| a.borrow().as_ref().is_some_and(|e| e.ivars().pane == pane))
}

/// The pane's folder changed during editing: it is listed again after.
pub fn reload_later() {
    RELOAD_LATER.with(|r| r.set(true));
}

/// Starts editing an item's name in the list; false if it cannot be (not
/// in list view, or the item is not shown).
pub fn begin(pane: usize, path: &Path) -> bool {
    if ACTIVE.with(|a| a.borrow().is_some()) {
        finish(true);
    }
    let Some(controller) = super::pane(pane) else { return false };
    let Some(field) = controller.name_field_for(path) else {
        tracing::debug!("rename in place: no name field for {}", path.display());
        return false;
    };
    let name = crate::pane::title_of(path);
    let folder = controller.entries_for(&[path.to_path_buf()]).first().is_some_and(|e| e.is_folder_like());
    let edit = InlineRename::alloc(mtm()).set_ivars(EditIvars {
        pane,
        path: path.to_path_buf(),
        name: name.clone(),
        field: field.clone(),
        cancelled: Cell::new(false),
    });
    let edit: Retained<InlineRename> = unsafe { msg_send![super(edit), init] };
    field.setStringValue(&ns(&name));
    field.setEditable(true);
    field.setSelectable(true);
    field.setDrawsBackground(true);
    field.setBackgroundColor(Some(&NSColor::textBackgroundColor()));
    field.setTextColor(Some(&NSColor::textColor()));
    field.setBezeled(false);
    field.setFocusRingType(objc2_app_kit::NSFocusRingType::Exterior);
    unsafe { field.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*edit))) };
    ACTIVE.with(|a| *a.borrow_mut() = Some(edit));
    let Some(window) = field.window() else {
        tracing::debug!("rename in place: the name field is not in a window");
        finish(false);
        return false;
    };
    window.makeFirstResponder(Some(&field));
    // The name without its extension, as in Finder.
    let stem = match name.rfind('.') {
        Some(dot) if dot > 0 && !folder => &name[..dot],
        _ => name.as_str(),
    };
    if let Some(editor) = field.currentEditor() {
        editor.setSelectedRange(NSRange::new(0, stem.encode_utf16().count()));
    }
    true
}

/// Tab or Shift-Tab: renames, then edits the next or previous item's name
/// (after the folder is listed again, when the name changed).
fn finish_and_move(step: isize) {
    let next = ACTIVE.with(|a| {
        let active = a.borrow();
        let edit = active.as_ref()?;
        let pane = super::pane(edit.ivars().pane)?;
        Some((edit.ivars().pane, pane.neighbor(&edit.ivars().path, step)?))
    });
    NEXT.with(|n| *n.borrow_mut() = next);
    if !finish(true) {
        // Nothing to list again: on to the next one now.
        continue_with(None);
    }
}

/// The pane listed its folder: edit the name Tab moved to, if any.
pub fn continue_with(pane: Option<usize>) {
    let Some((index, path)) = NEXT.with(|n| {
        let mut next = n.borrow_mut();
        if pane.is_none_or(|p| next.as_ref().is_some_and(|(i, _)| *i == p)) { next.take() } else { None }
    }) else {
        return;
    };
    // After the list has settled.
    DispatchQueue::main().exec_async(move || {
        let Some(pane) = super::pane(index) else { return };
        pane.select_paths(std::slice::from_ref(&path));
        begin(index, &path);
    });
}

/// Ends editing: renames if asked and the name changed, puts the field
/// back as a label, and gives the list the focus again. Returns whether
/// the folder is being listed again (after a rename).
fn finish(rename: bool) -> bool {
    let Some(edit) = ACTIVE.with(|a| a.borrow_mut().take()) else { return false };
    let ivars = edit.ivars();
    let field = &ivars.field;
    let typed = field.stringValue().to_string();
    unsafe { field.setDelegate(None) };
    field.setEditable(false);
    field.setSelectable(false);
    field.setDrawsBackground(false);
    field.setStringValue(&ns(&ivars.name));
    let pane = super::pane(ivars.pane);
    if let Some(pane) = &pane {
        pane.redraw_rows();
        pane.focus();
    }
    let new_name = typed.trim();
    let mut listing = false;
    if rename && !new_name.is_empty() && new_name != ivars.name {
        if let Some(pane) = &pane {
            listing = super::rename_to(pane, &ivars.path, new_name);
            if !listing {
                // It could not be renamed: stay on it.
                NEXT.with(|n| n.borrow_mut().take());
            }
        }
    } else if RELOAD_LATER.with(|r| r.replace(false))
        && let Some(pane) = &pane
    {
        pane.reload();
        listing = true;
    }
    RELOAD_LATER.with(|r| r.set(false));
    listing
}
