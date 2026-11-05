//! The sidebar: standard places, the user's favorites, mounted volumes,
//! sites, and Finder's color tags. Clicking an item opens it in the active
//! pane (a tag: everything with it).

use std::cell::{Cell, OnceCell, RefCell};
use std::path::{Path, PathBuf};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSControlTextEditingDelegate, NSDragOperation, NSDraggingInfo, NSFont, NSImageView, NSMenu, NSMenuDelegate,
    NSMenuItem, NSPasteboardTypeFileURL, NSScrollView, NSTableCellView, NSTableColumn, NSTableView,
    NSTableViewDataSource, NSTableViewDelegate, NSTableViewDropOperation, NSTableViewStyle, NSTextField, NSView,
};
use objc2_foundation::{
    NSArray, NSIndexSet, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
};

use super::util::{icon_for_path, ns, symbol};
use crate::pane::title_of;

#[derive(Debug, Clone, PartialEq)]
enum Row {
    Header(&'static str),
    Place {
        title: String,
        path: PathBuf,
        symbol: Option<&'static str>,
        favorite: bool,
    },
    Site {
        id: String,
        title: String,
        detail: String,
    },
    /// A folder on a saved site.
    RemoteFavorite {
        site_id: String,
        path: PathBuf,
        title: String,
        detail: String,
    },
    /// A Finder color tag.
    Tag {
        name: &'static str,
        color: u8,
    },
}

/// Actions in the Sites context menu (menu item tags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteAction {
    Connect = 1,
    ConnectInOther,
    Edit,
    Delete,
    New,
    Import,
}

impl SiteAction {
    fn from_tag(tag: isize) -> Option<SiteAction> {
        [Self::Connect, Self::ConnectInOther, Self::Edit, Self::Delete, Self::New, Self::Import]
            .into_iter()
            .find(|a| *a as isize == tag)
    }
}

pub struct SidebarIvars {
    rows: RefCell<Vec<Row>>,
    table: OnceCell<Retained<NSTableView>>,
    scroll: OnceCell<Retained<NSScrollView>>,
    /// Selection changed by code (to follow the active pane), not a click.
    updating: Cell<bool>,
    menu_row: Cell<Option<usize>>,
    /// Row and text scale (View > Zoom In and Zoom Out).
    zoom: Cell<f64>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzSidebar"]
    #[ivars = SidebarIvars]
    pub struct Sidebar;

    impl Sidebar {
        #[unsafe(method(siteAction:))]
        fn site_action(&self, sender: &NSMenuItem) {
            let row = self.ivars().menu_row.take();
            let site = row.and_then(|r| match self.ivars().rows.borrow().get(r) {
                Some(Row::Site { id, .. }) => Some(id.clone()),
                _ => None,
            });
            if let Some(action) = SiteAction::from_tag(sender.tag()) {
                super::site_action(action, site);
            }
        }

        #[unsafe(method(removeFavorite:))]
        fn remove_favorite(&self, _sender: Option<&AnyObject>) {
            let Some(row) = self.ivars().menu_row.take() else { return };
            match self.ivars().rows.borrow().get(row).cloned() {
                Some(Row::Place { path, favorite: true, .. }) => super::remove_favorite(&path),
                Some(Row::RemoteFavorite { site_id, path, .. }) => super::remove_remote_favorite(&site_id, &path),
                _ => {}
            }
        }
    }

    unsafe impl NSObjectProtocol for Sidebar {}

    unsafe impl NSTableViewDataSource for Sidebar {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _table: &NSTableView) -> NSInteger {
            self.ivars().rows.borrow().len() as NSInteger
        }

        /// Folders dropped on the sidebar's empty area become favorites.
        #[unsafe(method(tableView:validateDrop:proposedRow:proposedDropOperation:))]
        fn validate_drop(
            &self,
            table: &NSTableView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            _row: NSInteger,
            _operation: NSTableViewDropOperation,
        ) -> NSDragOperation {
            let paths = super::pane_view::paths_on_pasteboard(&info.draggingPasteboard());
            if paths.iter().any(|p| p.is_dir()) {
                table.setDropRow_dropOperation(-1, NSTableViewDropOperation::On);
                NSDragOperation::Link
            } else {
                NSDragOperation::None
            }
        }

        #[unsafe(method(tableView:acceptDrop:row:dropOperation:))]
        fn accept_drop(
            &self,
            _table: &NSTableView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            _row: NSInteger,
            _operation: NSTableViewDropOperation,
        ) -> bool {
            let paths = super::pane_view::paths_on_pasteboard(&info.draggingPasteboard());
            for path in paths.into_iter().filter(|p| p.is_dir()) {
                super::add_favorite(path);
            }
            true
        }
    }

    unsafe impl NSControlTextEditingDelegate for Sidebar {}

    unsafe impl NSTableViewDelegate for Sidebar {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for_row(
            &self,
            _table: &NSTableView,
            _column: Option<&NSTableColumn>,
            row: NSInteger,
        ) -> Option<Retained<NSView>> {
            self.cell_for(row)
        }

        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group_row(&self, _table: &NSTableView, row: NSInteger) -> bool {
            self.is_header(row)
        }

        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select_row(&self, table: &NSTableView, row: NSInteger) -> bool {
            let _ = table;
            !self.is_header(row)
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_did_change(&self, _notification: &NSNotification) {
            if self.ivars().updating.get() {
                return;
            }
            let row = self.table().selectedRow();
            tracing::debug!("sidebar selected row {row}");
            match usize::try_from(row).ok().and_then(|r| self.ivars().rows.borrow().get(r).cloned()) {
                Some(Row::Place { path, .. }) => super::open_in_active_pane(path),
                Some(Row::Site { id, .. }) => super::site_action(SiteAction::Connect, Some(id)),
                Some(Row::RemoteFavorite { site_id, path, .. }) => super::open_remote_favorite(&site_id, &path),
                Some(Row::Tag { name, .. }) => super::show_tag(name),
                _ => {}
            }
        }
    }

    unsafe impl NSMenuDelegate for Sidebar {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_needs_update(&self, menu: &NSMenu) {
            menu.removeAllItems();
            let row = usize::try_from(self.table().clickedRow()).ok();
            let favorite = row.and_then(|r| self.ivars().rows.borrow().get(r).cloned());
            let add_site_item = |title: &str, action: SiteAction| {
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(self.mtm()),
                        &ns(title),
                        Some(sel!(siteAction:)),
                        &ns(""),
                    )
                };
                item.setTag(action as isize);
                unsafe { item.setTarget(Some(self)) };
                menu.addItem(&item);
            };
            self.ivars().menu_row.set(row);
            match &favorite {
                Some(Row::Site { .. }) => {
                    add_site_item("Connect", SiteAction::Connect);
                    add_site_item("Connect in Other Pane", SiteAction::ConnectInOther);
                    menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
                    add_site_item("Edit\u{2026}", SiteAction::Edit);
                    add_site_item("Delete", SiteAction::Delete);
                    return;
                }
                Some(Row::Header("Sites")) | None => {
                    add_site_item("New Site\u{2026}", SiteAction::New);
                    add_site_item("Import from FileZilla\u{2026}", SiteAction::Import);
                    return;
                }
                _ => {}
            }
            if let Some(Row::Place { favorite: true, .. } | Row::RemoteFavorite { .. }) = favorite {
                self.ivars().menu_row.set(row);
                let item = unsafe {
                    NSMenuItem::initWithTitle_action_keyEquivalent(
                        NSMenuItem::alloc(self.mtm()),
                        &ns("Remove from Sidebar"),
                        Some(sel!(removeFavorite:)),
                        &ns(""),
                    )
                };
                unsafe { item.setTarget(Some(self)) };
                menu.addItem(&item);
            }
        }
    }
);

impl Sidebar {
    fn is_header(&self, row: NSInteger) -> bool {
        let rows = self.ivars().rows.borrow();
        matches!(usize::try_from(row).ok().and_then(|r| rows.get(r)), Some(Row::Header(_)))
    }

    fn cell_for(&self, row: NSInteger) -> Option<Retained<NSView>> {
        let zoom = self.ivars().zoom.get();
        let rows = self.ivars().rows.borrow();
        let row = rows.get(usize::try_from(row).ok()?)?;
        Some(match row {
            Row::Header(title) => {
                let field = NSTextField::labelWithString(&ns(title), self.mtm());
                field.setFont(Some(&NSFont::boldSystemFontOfSize((NSFont::smallSystemFontSize() * zoom).round())));
                field.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
                Retained::into_super(Retained::into_super(field))
            }
            Row::Place { title, path, symbol: name, .. } => {
                let icon = name.and_then(|name| symbol(name, title)).unwrap_or_else(|| icon_for_path(path));
                let tint = name.is_some().then(objc2_app_kit::NSColor::controlAccentColor);
                cell(self.mtm(), title, &icon, tint, None, zoom)
            }
            Row::Site { title, detail, .. } | Row::RemoteFavorite { title, detail, .. } => {
                let icon = symbol("server.rack", title).unwrap_or_else(super::util::folder_icon);
                cell(self.mtm(), title, &icon, Some(objc2_app_kit::NSColor::systemTealColor()), Some(detail), zoom)
            }
            Row::Tag { name, color } => {
                let icon = symbol("circle.fill", name).unwrap_or_else(super::util::folder_icon);
                cell(self.mtm(), name, &icon, super::util::tag_color(*color), None, zoom)
            }
        })
    }

    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SidebarIvars {
            rows: RefCell::new(Vec::new()),
            table: OnceCell::new(),
            scroll: OnceCell::new(),
            updating: Cell::new(false),
            menu_row: Cell::new(None),
            zoom: Cell::new(1.0),
        });
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };

        let table = NSTableView::new(mtm);
        let column = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns("place"));
        table.addTableColumn(&column);
        table.setHeaderView(None);
        table.setStyle(NSTableViewStyle::SourceList);
        table.setRowHeight(24.0);
        table.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]));
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(&*this)));
            table.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        }
        let menu = NSMenu::new(mtm);
        menu.setDelegate(Some(ProtocolObject::from_ref(&*this)));
        unsafe { table.setMenu(Some(&menu)) };
        // Clicking never moves focus away from the file panes.
        table.setRefusesFirstResponder(true);

        let scroll = NSScrollView::new(mtm);
        scroll.setDocumentView(Some(&table));
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);

        let _ = this.ivars().table.set(table);
        let _ = this.ivars().scroll.set(scroll);
        this
    }

    pub fn view(&self) -> Retained<NSView> {
        Retained::into_super(self.ivars().scroll.get().expect("built").clone())
    }

    fn table(&self) -> &NSTableView {
        self.ivars().table.get().expect("built")
    }

    /// Rebuilds the rows from the favorites and mounted volumes.
    pub fn reload(
        &self,
        favorites: &[PathBuf],
        remote_favorites: &[crate::settings::RemoteFavorite],
        sites: &[crate::sites::Site],
        current: Current,
    ) {
        let mut rows = vec![Row::Header("Favorites")];
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let standard = [
            (dirs::home_dir(), "house"),
            (dirs::desktop_dir(), "menubar.dock.rectangle"),
            (dirs::document_dir(), "doc"),
            (dirs::download_dir(), "arrow.down.circle"),
            (Some(PathBuf::from("/Applications")), "square.grid.2x2"),
        ];
        for (path, name) in standard {
            if let Some(path) = path {
                let title = if path == home { title_of(&home) } else { title_of(&path) };
                rows.push(Row::Place { title, path, symbol: Some(name), favorite: false });
            }
        }
        for path in favorites {
            rows.push(Row::Place { title: title_of(path), path: path.clone(), symbol: None, favorite: true });
        }
        for favorite in remote_favorites {
            // Bookmarks of deleted sites are left out.
            let Some(site) = sites.iter().find(|s| s.id == favorite.site_id) else { continue };
            let folder = if favorite.path.parent().is_none() { "/".to_string() } else { title_of(&favorite.path) };
            rows.push(Row::RemoteFavorite {
                site_id: favorite.site_id.clone(),
                path: favorite.path.clone(),
                title: format!("{folder} ({})", site.name),
                detail: format!("{}{}", site.address(), favorite.path.display()),
            });
        }
        rows.push(Row::Header("Locations"));
        for (title, path) in volumes() {
            rows.push(Row::Place { title, path, symbol: None, favorite: false });
        }
        rows.push(Row::Header("Sites"));
        for site in sites {
            let title = if site.folder.is_empty() {
                site.name.clone()
            } else {
                format!("{} \u{203a} {}", site.folder, site.name)
            };
            rows.push(Row::Site { id: site.id.clone(), title, detail: site.address() });
        }
        rows.push(Row::Header("Tags"));
        for (name, color) in crate::fs::tags::COLORS {
            rows.push(Row::Tag { name, color });
        }
        *self.ivars().rows.borrow_mut() = rows;
        self.table().reloadData();
        self.highlight(current);
    }

    /// Scales the rows and their text (1 is normal).
    pub fn set_zoom(&self, zoom: f64) {
        if (self.ivars().zoom.get() - zoom).abs() < 0.01 {
            return;
        }
        self.ivars().zoom.set(zoom);
        self.table().setRowHeight((24.0 * zoom).round());
        self.table().reloadData();
    }

    /// Selects the row of the active pane's folder or site, if there is one.
    pub fn highlight(&self, current: Current) {
        let row = self.ivars().rows.borrow().iter().position(|r| match (r, &current) {
            (Row::Place { path, .. }, Current::Local(current)) => path == current,
            (Row::RemoteFavorite { site_id, path, .. }, Current::Site(current, location)) => {
                site_id == current && path == location
            }
            (Row::Site { id, .. }, Current::Site(current, _)) => id == current,
            _ => false,
        });
        self.ivars().updating.set(true);
        match row {
            Some(row) => self.table().selectRowIndexes_byExtendingSelection(&NSIndexSet::indexSetWithIndex(row), false),
            None => unsafe { self.table().deselectAll(None) },
        }
        self.ivars().updating.set(false);
    }
}

/// What the active pane shows, to highlight it in the sidebar.
pub enum Current {
    Local(PathBuf),
    /// A folder on a connection opened from this site (by id).
    Site(String, PathBuf),
    Other,
}

/// Mounted volumes: the startup disk first, then /Volumes.
fn volumes() -> Vec<(String, PathBuf)> {
    let mut found = Vec::new();
    let mut startup = "Macintosh HD".to_string();
    let mut others = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/Volumes") {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            // The startup disk appears as a link to "/".
            if std::fs::read_link(&path).is_ok_and(|t| t == Path::new("/")) {
                startup = name;
            } else if path.is_dir() {
                others.push((name, path));
            }
        }
    }
    others.sort_by(|a, b| crate::fs::natural_cmp(&a.0, &b.0));
    found.push((startup, PathBuf::from("/")));
    found.extend(others);
    found
}

/// A row: an icon (tinted if asked) and a title, scaled by `zoom`.
fn cell(
    mtm: MainThreadMarker,
    title: &str,
    icon: &objc2_app_kit::NSImage,
    tint: Option<Retained<objc2_app_kit::NSColor>>,
    tooltip: Option<&str>,
    zoom: f64,
) -> Retained<NSView> {
    let height = (24.0 * zoom).round();
    let side = (16.0 * zoom).round();
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(180.0, height));
    let cell = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), frame);
    let image = NSImageView::initWithFrame(
        NSImageView::alloc(mtm),
        NSRect::new(NSPoint::new(2.0, ((height - side) / 2.0).floor()), NSSize::new(side, side)),
    );
    image.setImage(Some(icon));
    image.setImageScaling(objc2_app_kit::NSImageScaling::ScaleProportionallyUpOrDown);
    if let Some(tint) = &tint {
        image.setContentTintColor(Some(tint));
    }
    let field = NSTextField::labelWithString(&ns(title), mtm);
    let size = (13.0 * zoom).round();
    field.setFont(Some(&NSFont::systemFontOfSize(size)));
    field.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    let text_height = (size * 1.25).ceil() + 1.0;
    field.setFrame(NSRect::new(
        NSPoint::new(side + 8.0, ((height - text_height) / 2.0).floor()),
        NSSize::new(180.0 - side - 12.0, text_height),
    ));
    field.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable);
    if let Some(tooltip) = tooltip {
        field.setToolTip(Some(&ns(tooltip)));
    }
    cell.addSubview(&image);
    cell.addSubview(&field);
    unsafe {
        cell.setImageView(Some(&image));
        cell.setTextField(Some(&field));
    }
    Retained::into_super(cell)
}
