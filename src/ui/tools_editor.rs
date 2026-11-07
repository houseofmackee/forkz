//! Settings > Tools: your own commands, one per row (name, command, toolbar
//! icon, and whether to show the output). Every change is saved right away.

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
use crate::settings::{CustomTool, example_tools};

/// The table's columns: id, title, width, and whether it is a check box.
const COLUMNS: [(&str, &str, f64, bool); 4] = [
    ("name", "Name", 120.0, false),
    ("command", "Command", 250.0, false),
    ("symbol", "Toolbar Icon", 90.0, false),
    ("output", "Output", 56.0, true),
];

pub struct EditorIvars {
    table: OnceCell<Retained<NSTableView>>,
    tools: RefCell<Vec<CustomTool>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzToolsEditor"]
    #[ivars = EditorIvars]
    pub struct ToolsEditor;

    impl ToolsEditor {
        #[unsafe(method(toggleOption:))]
        fn toggle_option(&self, sender: &NSButton) {
            let on = sender.state() == NSControlStateValueOn;
            self.change(sender.tag() as usize, |t| t.show_output = on);
        }

        #[unsafe(method(addTool:))]
        fn add_tool(&self, _sender: Option<&AnyObject>) {
            self.ivars().tools.borrow_mut().push(CustomTool {
                name: "New Tool".into(),
                command: "open {selection}".into(),
                symbol: "hammer".into(),
                show_output: false,
            });
            self.save();
            let table = self.table();
            let row = table.numberOfRows() - 1;
            table.selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row as usize), false);
            table.editColumn_row_withEvent_select(0, row, None, true);
        }

        #[unsafe(method(removeTool:))]
        fn remove_tool(&self, _sender: Option<&AnyObject>) {
            let row = self.table().selectedRow();
            if let Ok(row) = usize::try_from(row)
                && row < self.ivars().tools.borrow().len()
            {
                self.ivars().tools.borrow_mut().remove(row);
                self.save();
            }
        }

        #[unsafe(method(addExamples:))]
        fn add_examples(&self, _sender: Option<&AnyObject>) {
            let mut tools = self.ivars().tools.borrow_mut();
            for example in example_tools() {
                if !tools.iter().any(|t| t.name == example.name) {
                    tools.push(example);
                }
            }
            drop(tools);
            self.save();
        }
    }

    unsafe impl NSObjectProtocol for ToolsEditor {}

    unsafe impl NSControlTextEditingDelegate for ToolsEditor {
        /// A name, command, or icon field was edited.
        #[unsafe(method(controlTextDidEndEditing:))]
        fn text_did_end_editing(&self, notification: &NSNotification) {
            let Some(field) = notification.object().and_then(|o| o.downcast::<NSTextField>().ok()) else { return };
            let id = field.identifier().map(|i| i.to_string()).unwrap_or_default();
            let text = field.stringValue().to_string();
            self.change(field.tag() as usize, |t| match id.as_str() {
                "name" if !text.trim().is_empty() => t.name = text.trim().to_string(),
                "command" => t.command = text.trim().to_string(),
                "symbol" if !text.trim().is_empty() => t.symbol = text.trim().to_string(),
                _ => {}
            });
        }
    }

    unsafe impl NSTextFieldDelegate for ToolsEditor {}

    unsafe impl NSTableViewDataSource for ToolsEditor {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().tools.borrow().len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for ToolsEditor {
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

impl ToolsEditor {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(EditorIvars { table: OnceCell::new(), tools: RefCell::new(Vec::new()) });
        unsafe { msg_send![super(this), init] }
    }

    fn table(&self) -> &NSTableView {
        self.ivars().table.get().expect("built")
    }

    /// The tab's content.
    pub fn view(&self) -> Retained<NSView> {
        let mtm = self.mtm();
        let note = NSTextField::wrappingLabelWithString(
            &ns("Commands run by zsh in the pane\u{2019}s folder (on this Mac), from Commands > Tools, the toolbar \
             (View > Customize Toolbar), or the command palette. In a command, {selection} is the selected items \
             (or the folder), {file} the first one, {names} their names, {folder} the pane\u{2019}s folder, and \
             {other-folder} and {other-selection} the other pane\u{2019}s; paths are quoted for you. Toolbar icons \
             are SF Symbol names, such as hammer or terminal."),
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
            button("Add Tool", sel!(addTool:)),
            button("Remove", sel!(removeTool:)),
            button("Add Examples", sel!(addExamples:)),
        );
        let buttons = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[&*add as &NSView, &*remove as &NSView, &*defaults as &NSView]),
            mtm,
        );
        buttons.setSpacing(8.0);
        let stack = NSStackView::stackViewWithViews(
            &NSArray::from_slice(&[&*note as &NSView, &*scroll as &NSView, &*buttons as &NSView]),
            mtm,
        );
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        stack.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
        stack.setSpacing(10.0);
        let _ = self.ivars().table.set(table);
        Retained::into_super(stack)
    }

    /// Shows the current tools.
    pub fn load(&self) {
        let Some(settings) = super::current_settings() else { return };
        *self.ivars().tools.borrow_mut() = settings.tools;
        if let Some(table) = self.ivars().table.get() {
            table.reloadData();
        }
    }

    /// An edit in place (the cell shows it already: no reload, which
    /// would also end the editing the user moved on to).
    fn change(&self, row: usize, edit: impl FnOnce(&mut CustomTool)) {
        if let Some(tool) = self.ivars().tools.borrow_mut().get_mut(row) {
            edit(tool);
        }
        self.store();
    }

    /// Rows added, removed, or restored: saved, and the table again.
    fn save(&self) {
        self.store();
        self.table().reloadData();
    }

    fn store(&self) {
        let tools = self.ivars().tools.borrow().clone();
        super::update_settings(|s| s.tools = tools);
        super::tools_changed();
    }

    fn cell(&self, id: &str, row: usize) -> Option<Retained<NSView>> {
        let tool = self.ivars().tools.borrow().get(row)?.clone();
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
            "output" => check(tool.show_output),
            _ => {
                let text = match id {
                    "name" => &tool.name,
                    "symbol" => &tool.symbol,
                    _ => &tool.command,
                };
                let field = NSTextField::textFieldWithString(&ns(text), mtm);
                field.setBordered(false);
                field.setDrawsBackground(false);
                field.setTag(row as isize);
                field.setIdentifier(Some(&ns(id)));
                if id == "command" {
                    field.setPlaceholderString(Some(&NSString::from_str("open -a TextEdit {selection}")));
                    field.setFont(Some(&objc2_app_kit::NSFont::monospacedSystemFontOfSize_weight(11.0, unsafe {
                        objc2_app_kit::NSFontWeightRegular
                    })));
                }
                unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
                Retained::into_super(Retained::into_super(field))
            }
        };
        Some(view)
    }
}
