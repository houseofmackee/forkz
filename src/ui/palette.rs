//! The command palette (Option-Command-P): type part of a command's name,
//! pick it with the arrow keys, and Return runs it. Saved workspaces are
//! in it too.

use std::cell::{OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSColor, NSControl, NSControlTextEditingDelegate, NSFont, NSLineBreakMode,
    NSScrollView, NSSearchField, NSSearchFieldDelegate, NSStackView, NSTableColumn, NSTableView, NSTableViewDataSource,
    NSTableViewDelegate, NSTextField, NSTextFieldDelegate, NSTextView, NSUserInterfaceLayoutOrientation, NSView,
    NSWindow, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
};

use super::util::{mtm, ns};

/// One thing the palette can do.
#[derive(Debug, Clone)]
pub struct Item<T> {
    pub title: String,
    /// The shortcut, shown on the right ("" for none).
    pub detail: String,
    pub value: T,
}

/// Items whose titles contain every word typed (ignoring case), the ones
/// starting with the text first.
pub fn filter<T: Clone>(items: &[Item<T>], text: &str) -> Vec<Item<T>> {
    let text = text.trim().to_lowercase();
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut hits: Vec<(usize, &Item<T>)> = items
        .iter()
        .filter_map(|item| {
            let title = item.title.to_lowercase();
            if !words.iter().all(|w| title.contains(w)) {
                return None;
            }
            // Earlier matches, and matches at word starts, rank higher.
            let at = words.first().and_then(|w| title.find(w)).unwrap_or(0);
            let word_start = at == 0 || title[..at].ends_with(' ');
            Some((at * 2 + usize::from(!word_start), item))
        })
        .collect();
    hits.sort_by_key(|(rank, _)| *rank);
    hits.into_iter().map(|(_, item)| item.clone()).collect()
}

struct Ui {
    window: Retained<NSWindow>,
    field: Retained<NSSearchField>,
    table: Retained<NSTableView>,
}

pub struct Ivars {
    ui: OnceCell<Ui>,
    items: Vec<Item<usize>>,
    shown: RefCell<Vec<Item<usize>>>,
    chosen: RefCell<Option<usize>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzCommandPalette"]
    #[ivars = Ivars]
    struct Palette;

    impl Palette {
        #[unsafe(method(rowChosen:))]
        fn row_chosen(&self, _sender: Option<&AnyObject>) {
            self.choose();
        }
    }

    unsafe impl NSObjectProtocol for Palette {}

    unsafe impl NSControlTextEditingDelegate for Palette {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _notification: &NSNotification) {
            self.refilter();
        }

        /// Arrows move through the list; Return runs; Escape closes.
        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, _control: &NSControl, _text: &NSTextView, selector: Sel) -> bool {
            self.key(selector)
        }
    }

    unsafe impl NSTextFieldDelegate for Palette {}

    unsafe impl NSSearchFieldDelegate for Palette {}

    unsafe impl NSWindowDelegate for Palette {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            NSApplication::sharedApplication(self.mtm()).stopModal();
        }

        /// Clicking elsewhere closes it, like Spotlight.
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            NSApplication::sharedApplication(self.mtm()).stopModal();
        }
    }

    unsafe impl NSTableViewDataSource for Palette {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().shown.borrow().len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for Palette {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(&self, _table: &NSTableView, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
            self.cell(column, row)
        }
    }
);

impl Palette {
    fn ui(&self) -> &Ui {
        self.ivars().ui.get().expect("built")
    }

    fn refilter(&self) {
        let text = self.ui().field.stringValue().to_string();
        *self.ivars().shown.borrow_mut() = filter(&self.ivars().items, &text);
        let table = &self.ui().table;
        table.reloadData();
        if !self.ivars().shown.borrow().is_empty() {
            table.selectRowIndexes_byExtendingSelection(&objc2_foundation::NSIndexSet::indexSetWithIndex(0), false);
            table.scrollRowToVisible(0);
        }
    }

    fn key(&self, selector: Sel) -> bool {
        let table = &self.ui().table;
        let count = self.ivars().shown.borrow().len() as isize;
        let row = table.selectedRow();
        let select = |row: isize| {
            if count > 0 {
                let row = row.clamp(0, count - 1);
                table.selectRowIndexes_byExtendingSelection(
                    &objc2_foundation::NSIndexSet::indexSetWithIndex(row as usize),
                    false,
                );
                table.scrollRowToVisible(row);
            }
        };
        if selector == sel!(moveDown:) {
            select(row + 1);
        } else if selector == sel!(moveUp:) {
            select(row - 1);
        } else if selector == sel!(insertNewline:) {
            self.choose();
        } else if selector == sel!(cancelOperation:) {
            NSApplication::sharedApplication(self.mtm()).stopModal();
        } else {
            return false;
        }
        true
    }

    fn choose(&self) {
        let row = self.ui().table.selectedRow();
        let value = usize::try_from(row).ok().and_then(|r| self.ivars().shown.borrow().get(r).map(|i| i.value));
        if value.is_some() {
            *self.ivars().chosen.borrow_mut() = value;
            NSApplication::sharedApplication(self.mtm()).stopModal();
        }
    }

    fn cell(&self, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
        let shown = self.ivars().shown.borrow();
        let item = shown.get(usize::try_from(row).ok()?)?;
        let detail = column?.identifier().to_string() == "detail";
        let field = NSTextField::labelWithString(&ns(if detail { &item.detail } else { &item.title }), self.mtm());
        field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        if detail {
            field.setTextColor(Some(&NSColor::secondaryLabelColor()));
            field.setAlignment(objc2_app_kit::NSTextAlignment::Right);
        } else {
            field.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        }
        Some(Retained::into_super(Retained::into_super(field)))
    }

    fn build(&self, parent: Option<&NSWindow>) {
        let mtm = self.mtm();
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 360.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::FullSizeContentView,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(objc2_app_kit::NSWindowTitleVisibility::Hidden);
        window.setMovableByWindowBackground(true);
        window.setDelegate(Some(ProtocolObject::from_ref(self)));

        let field = NSSearchField::new(mtm);
        field.setPlaceholderString(Some(&ns("Type a command")));
        field.setFont(Some(&NSFont::systemFontOfSize(18.0)));
        unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
        let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), NSRect::ZERO);
        for (id, width) in [("title", 360.0), ("detail", 90.0)] {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
            column.setWidth(width);
            table.addTableColumn(&column);
        }
        table.setHeaderView(None);
        table.setColumnAutoresizingStyle(
            objc2_app_kit::NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle,
        );
        table.setRowHeight(26.0);
        table.setStyle(objc2_app_kit::NSTableViewStyle::SourceList);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
            table.setTarget(Some(self));
            table.setDoubleAction(Some(sel!(rowChosen:)));
        }
        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);
        scroll.setDrawsBackground(false);
        let stack = NSStackView::stackViewWithViews(
            &NSArray::from_retained_slice(&[
                Retained::into_super(Retained::into_super(Retained::into_super(field.clone()))),
                Retained::into_super(scroll.clone()),
            ]),
            mtm,
        );
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        stack.setSpacing(10.0);
        stack.setEdgeInsets(NSEdgeInsets { top: 28.0, left: 14.0, bottom: 14.0, right: 14.0 });
        for view in [&*field as &NSView, &scroll] {
            view.leadingAnchor().constraintEqualToAnchor_constant(&stack.leadingAnchor(), 14.0).setActive(true);
            view.trailingAnchor().constraintEqualToAnchor_constant(&stack.trailingAnchor(), -14.0).setActive(true);
        }
        scroll.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Vertical);
        window.setContentView(Some(&stack));
        stack.layoutSubtreeIfNeeded();
        table.sizeToFit();
        // Near the top of the main window, like Spotlight.
        match parent {
            Some(parent) => {
                let frame = parent.frame();
                let origin = NSPoint::new(
                    frame.origin.x + (frame.size.width - 560.0) / 2.0,
                    frame.origin.y + frame.size.height - 360.0 - 90.0,
                );
                window.setFrameOrigin(origin);
            }
            None => window.center(),
        }
        let _ = self.ivars().ui.set(Ui { window, field, table });
    }
}

/// Shows the palette; returns the value of the item chosen.
pub fn show<T: Clone>(items: Vec<Item<T>>, parent: Option<&NSWindow>) -> Option<T> {
    let mtm = mtm();
    let indexed: Vec<Item<usize>> = items
        .iter()
        .enumerate()
        .map(|(i, item)| Item { title: item.title.clone(), detail: item.detail.clone(), value: i })
        .collect();
    let this = Palette::alloc(mtm).set_ivars(Ivars {
        ui: OnceCell::new(),
        shown: RefCell::new(indexed.clone()),
        items: indexed,
        chosen: RefCell::new(None),
    });
    let this: Retained<Palette> = unsafe { msg_send![super(this), init] };
    this.build(parent);
    this.refilter();
    let window = this.ui().window.clone();
    let app = NSApplication::sharedApplication(mtm);
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&this.ui().field));
    app.runModalForWindow(&window);
    window.setDelegate(None);
    window.orderOut(None);
    let chosen = *this.ivars().chosen.borrow();
    chosen.and_then(|i| items.get(i)).map(|item| item.value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_by_words_and_ranks_starts_first() {
        let item = |title: &str| Item { title: title.to_string(), detail: String::new(), value: title.to_string() };
        let items =
            vec![item("Show Hidden Files"), item("New Folder"), item("Hide Left Pane"), item("Rename Multiple Items")];
        let titles = |text: &str| filter(&items, text).into_iter().map(|i| i.value).collect::<Vec<_>>();
        assert_eq!(titles("hid"), ["Hide Left Pane", "Show Hidden Files"]);
        assert_eq!(titles("ren mul"), ["Rename Multiple Items"]);
        assert_eq!(titles("  "), items.iter().map(|i| i.value.clone()).collect::<Vec<_>>());
        assert!(titles("zebra").is_empty());
    }
}
