//! The Multi-Rename window: rules (replace, insert, remove, change case,
//! add a number, add a date), each on the name, the extension, or both;
//! a live preview that marks problems; and saved presets.

use std::cell::{OnceCell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSControlTextEditingDelegate, NSLayoutAttribute,
    NSLineBreakMode, NSPopUpButton, NSScrollView, NSStackView, NSTableColumn, NSTableView, NSTableViewDataSource,
    NSTableViewDelegate, NSTextField, NSTextFieldDelegate, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    NSArray, NSEdgeInsets, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use super::dialogs;
use super::util::{mtm, ns};
use crate::rename::{self, Case, DateSource, Item, Match, Place, Planned, Preset, Rule, Scope, Step};

const KINDS: [&str; 6] = ["Replace Text", "Insert Text", "Remove Characters", "Change Case", "Add Number", "Add Date"];
const SCOPES: [(&str, Scope); 3] =
    [("Name", Scope::Name), ("Extension", Scope::Extension), ("Whole Name", Scope::Whole)];
const SEPARATORS: [(&str, &str); 5] =
    [("Space", " "), ("Hyphen", "-"), ("Underscore", "_"), ("Period", "."), ("No Separator", "")];

/// The option popup's choices for each kind.
fn options(kind: usize) -> &'static [&'static str] {
    match kind {
        0 => &["Ignoring Case", "Matching Case", "Regular Expression"],
        1 => &["At Start", "At End", "At Position", "Instead of Name"],
        2 => &["From Start", "From End"],
        3 => &["lowercase", "UPPERCASE", "Title Case", "Sentence case"],
        _ => &["At Start", "At End", "Instead of Name"],
    }
}

/// Placeholders for the two text fields (None hides the field), and
/// whether the separator popup shows.
fn fields(kind: usize, option: usize) -> (Option<&'static str>, Option<&'static str>, bool) {
    match kind {
        0 => (Some("Find"), Some("Replace with"), false),
        1 => (Some("Text"), (option == 2).then_some("Position"), false),
        2 => (Some("Skip (0)"), Some("Count"), false),
        3 => (None, None, false),
        4 => (Some("Start (1)"), Some("Digits (1)"), true),
        _ => (Some("YYYY-MM-DD"), None, true),
    }
}

const PLACES_SHORT: [Place; 3] = [Place::Start, Place::End, Place::Replace];

const DATE_SOURCES: [(&str, DateSource); 3] =
    [("Modified", DateSource::Modified), ("Created", DateSource::Created), ("Today", DateSource::Now)];

struct Row {
    view: Retained<NSStackView>,
    kind: Retained<NSPopUpButton>,
    a: Retained<NSTextField>,
    b: Retained<NSTextField>,
    separator: Retained<NSPopUpButton>,
    /// Which date (Date rules only).
    source: Retained<NSPopUpButton>,
    option: Retained<NSPopUpButton>,
    scope: Retained<NSPopUpButton>,
    remove: Retained<NSButton>,
    /// The counter step (not shown; from presets).
    step: std::cell::Cell<i64>,
}

impl Row {
    fn kind(&self) -> usize {
        self.kind.indexOfSelectedItem().max(0) as usize
    }

    fn option(&self) -> usize {
        self.option.indexOfSelectedItem().max(0) as usize
    }

    fn text(field: &NSTextField) -> String {
        field.stringValue().to_string()
    }

    fn number<T: std::str::FromStr>(field: &NSTextField, default: T) -> T {
        field.stringValue().to_string().trim().parse().unwrap_or(default)
    }

    fn step(&self) -> Step {
        let option = self.option();
        let separator = SEPARATORS.get(self.separator.indexOfSelectedItem().max(0) as usize).map_or(" ", |s| s.1);
        let short_place = PLACES_SHORT.get(option).copied().unwrap_or_default();
        let rule = match self.kind() {
            0 => Rule::Replace {
                find: Self::text(&self.a),
                with: Self::text(&self.b),
                matching: [Match::Text, Match::TextMatchCase, Match::Regex][option.min(2)],
            },
            1 => Rule::Insert {
                text: Self::text(&self.a),
                at: match option {
                    0 => Place::Start,
                    2 => Place::Index(Self::number(&self.b, 0)),
                    3 => Place::Replace,
                    _ => Place::End,
                },
            },
            2 => {
                Rule::Remove { from: Self::number(&self.a, 0), count: Self::number(&self.b, 0), from_end: option == 1 }
            }
            3 => Rule::Case { to: [Case::Lower, Case::Upper, Case::Title, Case::Sentence][option.min(3)] },
            4 => Rule::Counter {
                start: Self::number(&self.a, 1),
                step: self.step.get(),
                digits: Self::number(&self.b, 1),
                separator: separator.into(),
                at: short_place,
            },
            _ => {
                let format = Self::text(&self.a);
                Rule::Date {
                    format: if format.trim().is_empty() { "YYYY-MM-DD".into() } else { format },
                    source: DATE_SOURCES
                        .get(self.source.indexOfSelectedItem().max(0) as usize)
                        .map_or(DateSource::Modified, |d| d.1),
                    separator: separator.into(),
                    at: short_place,
                }
            }
        };
        let scope = SCOPES.get(self.scope.indexOfSelectedItem().max(0) as usize).map_or(Scope::Name, |s| s.1);
        Step { rule, scope }
    }

    /// Shows the controls the kind needs.
    fn configure(&self, keep_option: bool) {
        let previous = self.option();
        let choices = options(self.kind());
        self.option.removeAllItems();
        let titles: Vec<Retained<NSString>> = choices.iter().map(|t| ns(t)).collect();
        self.option.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
        let option = if keep_option && previous < choices.len() { previous } else { 0 };
        self.option.selectItemAtIndex(option as isize);
        self.update_fields();
    }

    fn update_fields(&self) {
        let (a, b, separator) = fields(self.kind(), self.option());
        for (field, placeholder) in [(&self.a, a), (&self.b, b)] {
            field.setHidden(placeholder.is_none());
            field.setPlaceholderString(placeholder.map(ns).as_deref());
        }
        self.separator.setHidden(!separator);
        self.source.setHidden(self.kind() != 5);
    }

    fn load(&self, step: &Step) {
        let set = |field: &NSTextField, text: &str| field.setStringValue(&ns(text));
        let (kind, option) = match &step.rule {
            Rule::Replace { find, with, matching } => {
                set(&self.a, find);
                set(&self.b, with);
                (0, [Match::Text, Match::TextMatchCase, Match::Regex].iter().position(|m| m == matching).unwrap_or(0))
            }
            Rule::Insert { text, at } => {
                set(&self.a, text);
                let option = match at {
                    Place::Start => 0,
                    Place::End => 1,
                    Place::Index(index) => {
                        set(&self.b, &index.to_string());
                        2
                    }
                    Place::Replace => 3,
                };
                (1, option)
            }
            Rule::Remove { from, count, from_end } => {
                set(&self.a, &from.to_string());
                set(&self.b, &count.to_string());
                (2, usize::from(*from_end))
            }
            Rule::Case { to } => {
                (3, [Case::Lower, Case::Upper, Case::Title, Case::Sentence].iter().position(|c| c == to).unwrap_or(0))
            }
            Rule::Counter { start, step, digits, separator, at } => {
                set(&self.a, &start.to_string());
                set(&self.b, &digits.to_string());
                self.step.set(*step);
                self.select_separator(separator);
                (4, PLACES_SHORT.iter().position(|p| p == at).unwrap_or(1))
            }
            Rule::Date { format, source, separator, at } => {
                set(&self.a, format);
                self.source.selectItemAtIndex(DATE_SOURCES.iter().position(|d| d.1 == *source).unwrap_or(0) as isize);
                self.select_separator(separator);
                (5, PLACES_SHORT.iter().position(|p| p == at).unwrap_or(1))
            }
        };
        self.kind.selectItemAtIndex(kind as isize);
        self.configure(false);
        self.option.selectItemAtIndex(option as isize);
        self.update_fields();
        self.scope.selectItemAtIndex(SCOPES.iter().position(|s| s.1 == step.scope).unwrap_or(0) as isize);
    }

    fn select_separator(&self, separator: &str) {
        let index = SEPARATORS.iter().position(|s| s.1 == separator).unwrap_or(0);
        self.separator.selectItemAtIndex(index as isize);
    }
}

struct Ui {
    window: Retained<NSWindow>,
    rules: Retained<NSStackView>,
    table: Retained<NSTableView>,
    status: Retained<NSTextField>,
    rename: Retained<NSButton>,
    presets: Retained<NSPopUpButton>,
    delete_preset: Retained<NSButton>,
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    existing: Vec<String>,
    rows: Vec<Row>,
    planned: Vec<Planned>,
    error: Option<String>,
    accepted: bool,
}

pub struct Ivars {
    ui: OnceCell<Ui>,
    state: RefCell<State>,
}

thread_local! {
    /// The rules used last time, shown again next time.
    static LAST: RefCell<Vec<Step>> = const { RefCell::new(Vec::new()) };
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzMultiRename"]
    #[ivars = Ivars]
    struct RenameWindow;

    impl RenameWindow {
        #[unsafe(method(kindChanged:))]
        fn kind_changed(&self, sender: &AnyObject) {
            self.with_row(sender, |row| row.configure(false));
            self.refresh();
        }

        #[unsafe(method(optionChanged:))]
        fn option_changed(&self, sender: &AnyObject) {
            self.with_row(sender, Row::update_fields);
            self.refresh();
        }

        #[unsafe(method(ruleChanged:))]
        fn rule_changed(&self, _sender: Option<&AnyObject>) {
            self.refresh();
        }

        #[unsafe(method(addRule:))]
        fn add_rule_action(&self, _sender: Option<&AnyObject>) {
            self.add_row(None);
            self.refresh();
        }

        #[unsafe(method(removeRule:))]
        fn remove_rule(&self, sender: &AnyObject) {
            self.remove_row(sender);
            self.refresh();
        }

        #[unsafe(method(presetChosen:))]
        fn preset_chosen(&self, _sender: Option<&AnyObject>) {
            self.load_preset();
        }

        #[unsafe(method(savePreset:))]
        fn save_preset_action(&self, _sender: Option<&AnyObject>) {
            self.save_preset();
        }

        #[unsafe(method(deletePreset:))]
        fn delete_preset_action(&self, _sender: Option<&AnyObject>) {
            self.delete_preset();
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            self.finish(false);
        }

        #[unsafe(method(rename:))]
        fn rename_action(&self, _sender: Option<&AnyObject>) {
            self.finish(true);
        }
    }

    unsafe impl NSObjectProtocol for RenameWindow {}

    unsafe impl NSControlTextEditingDelegate for RenameWindow {
        #[unsafe(method(controlTextDidChange:))]
        fn text_changed(&self, _notification: &NSNotification) {
            self.refresh();
        }
    }

    unsafe impl NSTextFieldDelegate for RenameWindow {}

    unsafe impl NSWindowDelegate for RenameWindow {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            NSApplication::sharedApplication(self.mtm()).stopModal();
        }
    }

    unsafe impl NSTableViewDataSource for RenameWindow {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().state.borrow().planned.len() as NSInteger
        }
    }

    unsafe impl NSTableViewDelegate for RenameWindow {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            _table: &NSTableView,
            column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            self.cell(column, row)
        }
    }
);

fn popup(
    titles: &[&str],
    target: &RenameWindow,
    action: objc2::runtime::Sel,
    mtm: MainThreadMarker,
) -> Retained<NSPopUpButton> {
    let popup = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    let titles: Vec<Retained<NSString>> = titles.iter().map(|t| ns(t)).collect();
    popup.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
    unsafe {
        popup.setTarget(Some(target));
        popup.setAction(Some(action));
    }
    popup
}

fn button(
    title: &str,
    target: &RenameWindow,
    action: objc2::runtime::Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    unsafe { NSButton::buttonWithTitle_target_action(&ns(title), Some(target), Some(action), mtm) }
}

fn view<T: Message + AsRef<NSView>>(v: &T) -> Retained<NSView> {
    v.as_ref().retain()
}

fn stack(views: &[Retained<NSView>], vertical: bool, mtm: MainThreadMarker) -> Retained<NSStackView> {
    let stack = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(views), mtm);
    if vertical {
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        stack.setAlignment(NSLayoutAttribute::Leading);
    }
    stack.setSpacing(8.0);
    stack
}

fn same<T: Message>(control: &T, sender: &AnyObject) -> bool {
    std::ptr::eq((control as *const T).cast::<AnyObject>(), sender)
}

impl RenameWindow {
    fn ui(&self) -> &Ui {
        self.ivars().ui.get().expect("built")
    }

    fn with_row(&self, sender: &AnyObject, f: impl FnOnce(&Row)) {
        let state = self.ivars().state.borrow();
        if let Some(row) =
            state.rows.iter().find(|r| same(&*r.kind, sender) || same(&*r.option, sender) || same(&*r.remove, sender))
        {
            f(row);
        }
    }

    fn add_row(&self, step: Option<&Step>) {
        let mtm = self.mtm();
        let kinds: Vec<&str> = KINDS.to_vec();
        let separators: Vec<&str> = SEPARATORS.iter().map(|s| s.0).collect();
        let scopes: Vec<&str> = SCOPES.iter().map(|s| s.0).collect();
        let field = |width: f64| {
            let field = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 22.0)),
            );
            field.widthAnchor().constraintEqualToConstant(width).setActive(true);
            unsafe { field.setDelegate(Some(ProtocolObject::from_ref(self))) };
            field
        };
        let row = Row {
            kind: popup(&kinds, self, sel!(kindChanged:), mtm),
            a: field(150.0),
            b: field(120.0),
            separator: popup(&separators, self, sel!(ruleChanged:), mtm),
            source: popup(&DATE_SOURCES.iter().map(|d| d.0).collect::<Vec<_>>(), self, sel!(ruleChanged:), mtm),
            option: popup(&[], self, sel!(optionChanged:), mtm),
            scope: popup(&scopes, self, sel!(ruleChanged:), mtm),
            remove: button("\u{2212}", self, sel!(removeRule:), mtm),
            step: std::cell::Cell::new(1),
            view: NSStackView::new(mtm),
        };
        row.remove.setToolTip(Some(&ns("Remove this rule")));
        for v in [
            view(&*row.kind),
            view(&*row.a),
            view(&*row.b),
            view(&*row.source),
            view(&*row.separator),
            view(&*row.option),
            view(&*row.scope),
            view(&*row.remove),
        ] {
            row.view.addArrangedSubview(&v);
        }
        row.view.setSpacing(6.0);
        // Hidden controls take no room.
        row.view.setDetachesHiddenViews(true);
        match step {
            Some(step) => row.load(step),
            None => row.configure(false),
        }
        self.ui().rules.addArrangedSubview(&row.view);
        self.ivars().state.borrow_mut().rows.push(row);
    }

    fn remove_row(&self, sender: &AnyObject) {
        let removed = {
            let mut state = self.ivars().state.borrow_mut();
            let index = state.rows.iter().position(|r| same(&*r.remove, sender));
            index.map(|i| state.rows.remove(i))
        };
        if let Some(row) = removed {
            self.ui().rules.removeArrangedSubview(&row.view);
            row.view.removeFromSuperview();
        }
    }

    fn clear_rows(&self) {
        let rows = std::mem::take(&mut self.ivars().state.borrow_mut().rows);
        for row in rows {
            self.ui().rules.removeArrangedSubview(&row.view);
            row.view.removeFromSuperview();
        }
    }

    fn steps(&self) -> Vec<Step> {
        self.ivars().state.borrow().rows.iter().map(Row::step).collect()
    }

    /// Plans again from the rules, and shows the result.
    fn refresh(&self) {
        let steps = self.steps();
        {
            let mut state = self.ivars().state.borrow_mut();
            match rename::plan(&state.items, &steps, &state.existing) {
                Ok(planned) => {
                    state.planned = planned;
                    state.error = None;
                }
                Err(error) => {
                    state.planned = state
                        .items
                        .iter()
                        .map(|i| Planned { old: i.name.clone(), new: i.name.clone(), problem: None })
                        .collect();
                    state.error = Some(error);
                }
            }
        }
        let ui = self.ui();
        ui.table.reloadData();
        let state = self.ivars().state.borrow();
        let problems: Vec<&Planned> = state.planned.iter().filter(|p| p.problem.is_some()).collect();
        let changing = state.planned.iter().filter(|p| p.changes()).count();
        let (text, bad) = if let Some(error) = &state.error {
            (error.clone(), true)
        } else if let Some(first) = problems.first() {
            let count =
                if problems.len() == 1 { "1 problem".to_string() } else { format!("{} problems", problems.len()) };
            let what = first.problem.as_ref().map_or("", |p| p.describe());
            (format!("{count}: {what} (\u{201c}{}\u{201d})", first.new), true)
        } else {
            let total = state.planned.len();
            (format!("{changing} of {total} {} will change.", if total == 1 { "name" } else { "names" }), false)
        };
        ui.status.setStringValue(&ns(&text));
        let color = if bad { NSColor::systemRedColor() } else { NSColor::secondaryLabelColor() };
        ui.status.setTextColor(Some(&color));
        ui.rename.setEnabled(!bad && changing > 0);
    }

    fn cell(&self, column: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
        let state = self.ivars().state.borrow();
        let planned = state.planned.get(usize::try_from(row).ok()?)?;
        let new = column?.identifier().to_string() == "new";
        let field = NSTextField::labelWithString(&ns(if new { &planned.new } else { &planned.old }), self.mtm());
        field.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        if new {
            if let Some(problem) = &planned.problem {
                field.setTextColor(Some(&NSColor::systemRedColor()));
                field.setToolTip(Some(&ns(problem.describe())));
            } else if !planned.changes() {
                field.setTextColor(Some(&NSColor::secondaryLabelColor()));
            }
        }
        Some(Retained::into_super(Retained::into_super(field)))
    }

    fn presets() -> Vec<Preset> {
        super::current_settings().map(|s| s.rename_presets).unwrap_or_default()
    }

    fn fill_presets(&self, select: Option<&str>) {
        let ui = self.ui();
        ui.presets.removeAllItems();
        ui.presets.addItemWithTitle(&ns("Presets"));
        for preset in Self::presets() {
            ui.presets.addItemWithTitle(&ns(&preset.name));
        }
        match select.and_then(|name| Self::presets().iter().position(|p| p.name == name)) {
            Some(index) => ui.presets.selectItemAtIndex(index as isize + 1),
            None => ui.presets.selectItemAtIndex(0),
        }
        ui.delete_preset.setEnabled(ui.presets.indexOfSelectedItem() > 0);
    }

    fn chosen_preset(&self) -> Option<Preset> {
        let index = self.ui().presets.indexOfSelectedItem();
        usize::try_from(index - 1).ok().and_then(|i| Self::presets().get(i).cloned())
    }

    fn load_preset(&self) {
        let preset = self.chosen_preset();
        self.ui().delete_preset.setEnabled(preset.is_some());
        let Some(preset) = preset else { return };
        self.clear_rows();
        for step in &preset.steps {
            self.add_row(Some(step));
        }
        self.refresh();
    }

    fn save_preset(&self) {
        let steps = self.steps();
        let initial = self.chosen_preset().map(|p| p.name).unwrap_or_default();
        let Some(name) = dialogs::prompt("Save Preset", "A name for these rules:", "Save", &initial, None) else {
            return;
        };
        let name = name.trim().to_string();
        super::update_settings(|settings| {
            let preset = Preset { name: name.clone(), steps };
            match settings.rename_presets.iter_mut().find(|p| p.name == name) {
                Some(existing) => *existing = preset,
                None => settings.rename_presets.push(preset),
            }
        });
        self.fill_presets(Some(&name));
    }

    fn delete_preset(&self) {
        let Some(preset) = self.chosen_preset() else { return };
        let message = format!("Delete the preset \u{201c}{}\u{201d}?", preset.name);
        if !dialogs::confirm(&message, "The rules shown stay as they are.", "Delete", true) {
            return;
        }
        super::update_settings(|settings| settings.rename_presets.retain(|p| p.name != preset.name));
        self.fill_presets(None);
    }

    fn finish(&self, accept: bool) {
        self.ivars().state.borrow_mut().accepted = accept;
        NSApplication::sharedApplication(self.mtm()).stopModal();
    }

    fn build(&self, count: usize) {
        let mtm = self.mtm();
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(900.0, 560.0)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Resizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&ns(&format!("Rename {count} {}", if count == 1 { "Item" } else { "Items" })));
        window.setMinSize(NSSize::new(760.0, 420.0));
        window.setDelegate(Some(ProtocolObject::from_ref(self)));

        let presets = popup(&["Presets"], self, sel!(presetChosen:), mtm);
        let save_preset = button("Save Preset\u{2026}", self, sel!(savePreset:), mtm);
        let delete_preset = button("Delete Preset", self, sel!(deletePreset:), mtm);
        let add = button("Add Rule", self, sel!(addRule:), mtm);
        let top = stack(&[view(&*add), view(&*presets), view(&*save_preset), view(&*delete_preset)], false, mtm);

        let rules = stack(&[], true, mtm);
        rules.setSpacing(6.0);

        let table = NSTableView::initWithFrame(NSTableView::alloc(mtm), NSRect::ZERO);
        for (id, title) in [("old", "Name"), ("new", "New Name")] {
            let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
            column.setTitle(&ns(title));
            column.setWidth(390.0);
            table.addTableColumn(&column);
        }
        table.setUsesAlternatingRowBackgroundColors(true);
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);
        scroll.heightAnchor().constraintGreaterThanOrEqualToConstant(200.0).setActive(true);

        let status = NSTextField::labelWithString(&ns(""), mtm);
        status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        // The status takes the spare room, so the buttons sit on the right.
        status.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
        status.setContentCompressionResistancePriority_forOrientation(
            1.0,
            objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
        );
        let cancel = button("Cancel", self, sel!(cancel:), mtm);
        cancel.setKeyEquivalent(&ns("\u{1b}"));
        let rename = button("Rename", self, sel!(rename:), mtm);
        rename.setKeyEquivalent(&ns("\r"));
        let bottom = stack(&[view(&*status), view(&*cancel), view(&*rename)], false, mtm);

        let content = stack(&[view(&*top), view(&*rules), view(&*scroll), view(&*bottom)], true, mtm);
        content.setSpacing(12.0);
        content.setEdgeInsets(NSEdgeInsets { top: 16.0, left: 16.0, bottom: 16.0, right: 16.0 });
        for v in [view(&*scroll), view(&*bottom), view(&*rules)] {
            v.leadingAnchor().constraintEqualToAnchor_constant(&content.leadingAnchor(), 16.0).setActive(true);
            v.trailingAnchor().constraintEqualToAnchor_constant(&content.trailingAnchor(), -16.0).setActive(true);
        }
        window.setContentView(Some(&content));

        let _ = self.ivars().ui.set(Ui { window, rules, table, status, rename, presets, delete_preset });
        self.fill_presets(None);
    }
}

/// Shows the window for these items (in the pane's order) among the names
/// in their folder. Returns the plan if Rename was chosen.
/// `initial`: rules to start with (a preset chosen in the command palette)
/// instead of the last ones used.
pub fn show(items: Vec<Item>, existing: Vec<String>, initial: Option<Vec<Step>>) -> Option<Vec<Planned>> {
    let mtm = mtm();
    let count = items.len();
    let this = RenameWindow::alloc(mtm)
        .set_ivars(Ivars { ui: OnceCell::new(), state: RefCell::new(State { items, existing, ..State::default() }) });
    let this: Retained<RenameWindow> = unsafe { msg_send![super(this), init] };
    this.build(count);
    let last = initial.unwrap_or_else(|| LAST.with(|l| l.borrow().clone()));
    if last.is_empty() {
        this.add_row(None);
    } else {
        for step in &last {
            this.add_row(Some(step));
        }
    }
    this.refresh();

    let window = this.ui().window.clone();
    window.center();
    let app = NSApplication::sharedApplication(mtm);
    app.activate();
    window.makeKeyAndOrderFront(None);
    if let Some(first) = this.ivars().state.borrow().rows.first() {
        window.makeFirstResponder(Some(&first.a));
    }
    app.runModalForWindow(&window);
    let steps = this.steps();
    LAST.with(|l| *l.borrow_mut() = steps);
    window.setDelegate(None);
    window.orderOut(None);
    let state = this.ivars().state.borrow();
    (state.accepted && state.error.is_none()).then(|| state.planned.clone())
}
