//! Settings > Filters: the filename filters, one per row (name, patterns,
//! files, folders, hide in lists, skip in copies), with a switch for all of
//! them. Every change is saved and applied right away.

use std::cell::{OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSButton, NSControlStateValueOff, NSControlStateValueOn, NSControlTextEditingDelegate, NSScrollView, NSStackView,
    NSTableColumn, NSTableView, NSTableViewDataSource, NSTableViewDelegate, NSTextField, NSTextFieldDelegate,
    NSUserInterfaceItemIdentification, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSString};

use super::util::ns;
use crate::fs::filters::{self, NameFilter};

/// The table's columns: id, title, width, and whether it is a check box.
const COLUMNS: [(&str, &str, f64, bool); 6] = [
    ("name", "Name", 100.0, false),
    ("patterns", "Patterns", 124.0, false),
    ("files", "Files", 38.0, true),
    ("folders", "Folders", 50.0, true),
    ("listings", "Hide in Lists", 78.0, true),
    ("transfers", "Skip in Copies", 90.0, true),
];

pub struct EditorIvars {
    table: OnceCell<Retained<NSTableView>>,
    master: OnceCell<Retained<NSButton>>,
    filters: RefCell<Vec<NameFilter>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzFiltersEditor"]
    #[ivars = EditorIvars]
    pub struct FiltersEditor;

    impl FiltersEditor {
        #[unsafe(method(toggleOption:))]
        fn toggle_option(&self, sender: &NSButton) {
            let id = sender.identifier().map(|i| i.to_string()).unwrap_or_default();
            let on = sender.state() == NSControlStateValueOn;
            self.change(sender.tag() as usize, |f| match id.as_str() {
                "files" => f.files = on,
                "folders" => f.folders = on,
                "listings" => f.listings = on,
                _ => f.transfers = on,
            });
        }

        #[unsafe(method(toggleMaster:))]
        fn toggle_master(&self, sender: &NSButton) {
            let on = sender.state() == NSControlStateValueOn;
            super::update_settings(|s| s.filters_on = on);
        }

        #[unsafe(method(addFilter:))]
        fn add_filter(&self, _sender: Option<&AnyObject>) {
            self.ivars().filters.borrow_mut().push(NameFilter::default());
            self.save();
            let table = self.table();
            let row = table.numberOfRows() - 1;
            table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row as usize), false);
            table.editColumn_row_withEvent_select(0, row, None, true);
        }

        #[unsafe(method(removeFilter:))]
        fn remove_filter(&self, _sender: Option<&AnyObject>) {
            let row = self.table().selectedRow();
            if let Ok(row) = usize::try_from(row)
                && row < self.ivars().filters.borrow().len()
            {
                self.ivars().filters.borrow_mut().remove(row);
                self.save();
            }
        }

        #[unsafe(method(restoreDefaults:))]
        fn restore_defaults(&self, _sender: Option<&AnyObject>) {
            *self.ivars().filters.borrow_mut() = filters::defaults();
            self.save();
        }
    }

    unsafe impl NSObjectProtocol for FiltersEditor {}

    unsafe impl NSControlTextEditingDelegate for FiltersEditor {
        /// A name or patterns field was edited.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn text_did_end_editing(&self, notification: &NSNotification) {
            let Some(field) = notification.object().and_then(|o| o.downcast::<NSTextField>().ok()) else { return };
            let id = field.identifier().map(|i| i.to_string()).unwrap_or_default();
            let text = field.stringValue().to_string();
            self.change(field.tag() as usize, |f| {
                if id == "name" {
                    if !text.trim().is_empty() {
                        f.name = text.trim().to_string();
                    }
                } else {
                    f.patterns = text.trim().to_string();
                }
            });
        }
    }

    unsafe impl NSTextFieldDelegate for FiltersEditor {}

    unsafe impl NSTableViewDataSource for FiltersEditor {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().filters.borrow().len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for FiltersEditor {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            _table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            match (column, usize::try_from(row)) {
                (Some(column), Ok(row)) => self.cell(&column.identifier().to_string(), row),
                _ => None,
            }
        }
    }
);

impl FiltersEditor {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(EditorIvars {
            table: OnceCell::new(),
            master: OnceCell::new(),
            filters: RefCell::new(Vec::new()),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn table(&self) -> &NSTableView {
        self.ivars().table.get().expect("built")
    }

    /// The tab's content.
    pub fn view(&self) -> Retained<NSView> {
        let mtm = self.mtm();
        let master = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &ns("Use filename filters (also View > Use Filename Filters)"),
                Some(self),
                Some(sel!(toggleMaster:)),
                mtm,
            )
        };
        let note = NSTextField::wrappingLabelWithString(
            &ns("Patterns are separated by semicolons: * and ? are wildcards, and a plain name matches only that \
             name. Hidden items do not show in the lists; skipped items are left out of the folders you copy \
             (what you choose yourself is always copied, and moves take everything)."),
            mtm,
        );
        note.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(objc2_app_kit::NSFont::smallSystemFontSize())));
        note.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        note.setPreferredMaxLayoutWidth(540.0);

        let table = NSTableView::new(mtm);
        for (id, title, width, _) in COLUMNS {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
            column.setTitle(&ns(title));
            column.setWidth(width);
            table.addTableColumn(&column);
        }
        table.setRowHeight(24.0);
        table.setIntercellSpacing(objc2_foundation::NSSize::new(6.0, 2.0));
        table.setUsesAlternatingRowBackgroundColors(true);
        table.setStyle(objc2_app_kit::NSTableViewStyle::Plain);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);
        scroll.heightAnchor().constraintEqualToConstant(200.0).setActive(true);
        scroll.widthAnchor().constraintEqualToConstant(540.0).setActive(true);

        let button = |title: &str, action| unsafe {
            NSButton::buttonWithTitle_target_action(&ns(title), Some(self), Some(action), mtm)
        };
        let (add, remove, defaults) = (
            button("Add Filter", sel!(addFilter:)),
            button("Remove", sel!(removeFilter:)),
            button("Restore Defaults", sel!(restoreDefaults:)),
        );
        let buttons = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[&*add as &NSView, &*remove as &NSView, &*defaults as &NSView]),
            mtm,
        );
        buttons.setSpacing(8.0);
        let stack = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[&*master as &NSView, &*note as &NSView, &*scroll as &NSView, &*buttons as &NSView]),
            mtm,
        );
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        stack.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
        stack.setSpacing(10.0);
        let _ = self.ivars().table.set(table);
        let _ = self.ivars().master.set(master);
        Retained::into_super(stack)
    }

    /// Shows the current filters.
    pub fn load(&self) {
        let Some(settings) = super::current_settings() else { return };
        *self.ivars().filters.borrow_mut() = settings.filters;
        if let Some(master) = self.ivars().master.get() {
            master.setState(if settings.filters_on { NSControlStateValueOn } else { NSControlStateValueOff });
        }
        if let Some(table) = self.ivars().table.get() {
            table.reloadData();
        }
    }

    /// An edit in place (the cell shows it already: no reload, which
    /// would also end the editing the user moved on to).
    fn change(&self, row: usize, edit: impl FnOnce(&mut NameFilter)) {
        if let Some(filter) = self.ivars().filters.borrow_mut().get_mut(row) {
            edit(filter);
        }
        self.store();
    }

    /// Rows added, removed, or restored: saved, and the table again.
    fn save(&self) {
        self.store();
        self.table().reloadData();
    }

    fn store(&self) {
        let filters = self.ivars().filters.borrow().clone();
        super::update_settings(|s| s.filters = filters);
    }

    fn cell(&self, id: &str, row: usize) -> Option<Retained<NSView>> {
        let filter = self.ivars().filters.borrow().get(row)?.clone();
        let mtm = self.mtm();
        let check = |on: bool| {
            let button = unsafe {
                NSButton::checkboxWithTitle_target_action(&ns(""), Some(self), Some(sel!(toggleOption:)), mtm)
            };
            button.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
            button.setTag(row as isize);
            button.setIdentifier(Some(&ns(id)));
            Retained::into_super(Retained::into_super(button))
        };
        let view: Retained<NSView> = match id {
            "files" => check(filter.files),
            "folders" => check(filter.folders),
            "listings" => check(filter.listings),
            "transfers" => check(filter.transfers),
            _ => {
                let text = if id == "name" { &filter.name } else { &filter.patterns };
                let field = NSTextField::textFieldWithString(&ns(text), mtm);
                field.setBordered(false);
                field.setDrawsBackground(false);
                field.setTag(row as isize);
                field.setIdentifier(Some(&ns(id)));
                if id == "patterns" {
                    field.setPlaceholderString(Some(&NSString::from_str("*.tmp; node_modules")));
                }
                unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
                Retained::into_super(Retained::into_super(field))
            }
        };
        Some(view)
    }
}
