//! The form to connect to a server or edit a saved site: name, host,
//! port, user, how to log in, and the folders to open.

use std::cell::OnceCell;
use std::path::PathBuf;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSButton, NSControlStateValueOn, NSGridCellPlacement, NSGridView, NSModalResponseOK,
    NSOpenPanel, NSPopUpButton, NSSecureTextField, NSTextField, NSView,
};
use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use super::util::{file_url, mtm, ns, url_path};
use crate::sites::Site;
use crate::vfs::sftp::AuthMethod;

/// What the form returns.
pub struct FormResult {
    pub site: Site,
    /// The password typed (or the key passphrase), if any.
    pub password: Option<String>,
    /// Keep the site (and password) for next time.
    pub save: bool,
}

const AUTH_CHOICES: [(&str, AuthMethod); 3] =
    [("Password", AuthMethod::Password), ("Key File", AuthMethod::Key), ("SSH Agent", AuthMethod::Agent)];

struct Fields {
    key_file: Retained<NSTextField>,
    choose: Retained<NSButton>,
    password: Retained<NSSecureTextField>,
    auth: Retained<NSPopUpButton>,
}

pub struct TargetIvars {
    fields: OnceCell<Fields>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "ForkzSiteFormTarget"]
    #[ivars = TargetIvars]
    struct FormTarget;

    impl FormTarget {
        #[unsafe(method(chooseKey:))]
        fn choose_key(&self, _sender: Option<&AnyObject>) {
            self.pick_key();
        }

        #[unsafe(method(authChanged:))]
        fn auth_changed(&self, _sender: Option<&AnyObject>) {
            self.update_enabled();
        }
    }

    unsafe impl NSObjectProtocol for FormTarget {}
);

impl FormTarget {
    fn pick_key(&self) {
        let Some(fields) = self.ivars().fields.get() else { return };
        let panel = NSOpenPanel::openPanel(self.mtm());
        panel.setCanChooseFiles(true);
        panel.setCanChooseDirectories(false);
        panel.setShowsHiddenFiles(true);
        panel.setMessage(Some(&ns("Choose a private key")));
        if let Some(ssh) = dirs::home_dir().map(|h| h.join(".ssh")) {
            panel.setDirectoryURL(Some(&file_url(&ssh)));
        }
        if panel.runModal() == NSModalResponseOK
            && let Some(path) = panel.URL().and_then(|u| url_path(&u))
        {
            fields.key_file.setStringValue(&ns(&path.display().to_string()));
        }
    }

    fn update_enabled(&self) {
        let Some(fields) = self.ivars().fields.get() else { return };
        let auth = AUTH_CHOICES.get(fields.auth.indexOfSelectedItem().max(0) as usize).map(|c| c.1);
        let key = auth == Some(AuthMethod::Key);
        fields.key_file.setEnabled(key);
        fields.choose.setEnabled(key);
        // The password field doubles as the key's passphrase.
        fields.password.setEnabled(auth != Some(AuthMethod::Agent));
        fields.password.setPlaceholderString(Some(&ns(if key { "Passphrase (if any)" } else { "Optional" })));
    }
}

fn text_field(mtm: MainThreadMarker, value: &str, placeholder: &str, width: f64) -> Retained<NSTextField> {
    let field = NSTextField::initWithFrame(
        NSTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 22.0)),
    );
    field.setStringValue(&ns(value));
    field.setPlaceholderString(Some(&ns(placeholder)));
    field.widthAnchor().constraintEqualToConstant(width).setActive(true);
    field
}

fn label(mtm: MainThreadMarker, text: &str) -> Retained<NSView> {
    Retained::into_super(Retained::into_super(NSTextField::labelWithString(&ns(text), mtm)))
}

fn as_view<T: objc2::Message + AsRef<NSView>>(view: &T) -> Retained<NSView> {
    view.as_ref().retain()
}

/// Shows the form. `site` is the one to edit (None for a new connection).
pub fn show(site: Option<&Site>, password: Option<String>) -> Option<FormResult> {
    let mtm = mtm();
    let editing = site.is_some();
    let site = site.cloned().unwrap_or_else(|| Site::new("", 22, ""));
    let target: Retained<FormTarget> = {
        let this = FormTarget::alloc(mtm).set_ivars(TargetIvars { fields: OnceCell::new() });
        unsafe { msg_send![super(this), init] }
    };

    let name = text_field(mtm, if editing { &site.name } else { "" }, "Same as the server", 260.0);
    // The server field offers the hosts from ~/.ssh/config.
    let host = objc2_app_kit::NSComboBox::initWithFrame(
        objc2_app_kit::NSComboBox::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, 24.0)),
    );
    host.setStringValue(&ns(&site.host));
    host.setPlaceholderString(Some(&ns("sftp.example.com, or a host from ~/.ssh/config")));
    host.widthAnchor().constraintEqualToConstant(260.0).setActive(true);
    let aliases: Vec<Retained<NSString>> =
        crate::vfs::ssh_config::SshConfig::load().aliases().iter().map(|a| ns(a)).collect();
    if !aliases.is_empty() {
        let values: Vec<Retained<AnyObject>> =
            aliases.into_iter().map(Retained::into_super).map(Retained::into_super).collect();
        // SAFETY: the values are strings, as a combo box expects.
        unsafe { host.addItemsWithObjectValues(&NSArray::from_retained_slice(&values)) };
        host.setNumberOfVisibleItems(10);
    }
    host.setCompletes(true);
    let port = text_field(mtm, &site.port.to_string(), "22", 70.0);
    let user = text_field(mtm, &site.user, &std::env::var("USER").unwrap_or_default(), 260.0);
    let auth = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    let titles: Vec<Retained<NSString>> = AUTH_CHOICES.iter().map(|c| ns(c.0)).collect();
    auth.addItemsWithTitles(&NSArray::from_retained_slice(&titles));
    auth.selectItemAtIndex(AUTH_CHOICES.iter().position(|c| c.1 == site.auth).unwrap_or(0) as isize);
    unsafe {
        auth.setTarget(Some(&target));
        auth.setAction(Some(sel!(authChanged:)));
    }
    let password_field = NSSecureTextField::initWithFrame(
        NSSecureTextField::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, 22.0)),
    );
    password_field.setStringValue(&ns(password.as_deref().unwrap_or("")));
    password_field.widthAnchor().constraintEqualToConstant(260.0).setActive(true);
    let key_path = site.key_file.as_ref().map(|k| k.display().to_string()).unwrap_or_default();
    let key_file = text_field(mtm, &key_path, "~/.ssh/id_ed25519", 180.0);
    let choose = unsafe {
        NSButton::buttonWithTitle_target_action(&ns("Choose\u{2026}"), Some(&target), Some(sel!(chooseKey:)), mtm)
    };
    let key_row = objc2_app_kit::NSStackView::stackViewWithViews(
        &NSArray::from_retained_slice(&[as_view(&*key_file), as_view(&*choose)]),
        mtm,
    );
    key_row.setSpacing(6.0);
    let remote = text_field(mtm, site.remote_dir.as_deref().unwrap_or(""), "Login folder", 260.0);
    let local_path = site.local_dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
    let local = text_field(mtm, &local_path, "Optional: shown in the other pane", 260.0);
    let save = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &ns(if editing { "Save password in the Keychain" } else { "Add to Sites (password in the Keychain)" }),
            None,
            None,
            mtm,
        )
    };
    save.setState(NSControlStateValueOn);

    let rows: Vec<Retained<NSArray<NSView>>> = [
        ("Name:", as_view(&*name)),
        ("Server:", as_view(&*host)),
        ("Port:", as_view(&*port)),
        ("User:", as_view(&*user)),
        ("Log in with:", as_view(&*auth)),
        ("Password:", as_view(&*password_field)),
        ("Key file:", as_view(&*key_row)),
        ("Remote folder:", as_view(&*remote)),
        ("Local folder:", as_view(&*local)),
        ("", as_view(&*save)),
    ]
    .into_iter()
    .map(|(title, control)| NSArray::from_retained_slice(&[label(mtm, title), control]))
    .collect();
    let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&rows), mtm);
    grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
    grid.setRowSpacing(8.0);
    grid.setColumnSpacing(8.0);
    grid.setFrameSize(grid.fittingSize());

    let _ = target.ivars().fields.set(Fields {
        key_file: key_file.clone(),
        choose: choose.clone(),
        password: password_field.clone(),
        auth: auth.clone(),
    });
    target.update_enabled();

    let alert = NSAlert::new(mtm);
    alert.setMessageText(&ns(if editing { "Edit Site" } else { "Connect to Server" }));
    alert.setInformativeText(&ns("SFTP over SSH. Passwords are kept in your login Keychain."));
    alert.addButtonWithTitle(&ns(if editing { "Save" } else { "Connect" }));
    alert.addButtonWithTitle(&ns("Cancel"));
    alert.setAccessoryView(Some(&grid));
    alert.layout();
    alert.window().setInitialFirstResponder(Some(if editing { &name } else { &host }));
    NSApplication::sharedApplication(mtm).activate();

    loop {
        if alert.runModal() != objc2_app_kit::NSAlertFirstButtonReturn {
            return None;
        }
        let host_text = host.stringValue().to_string().trim().to_string();
        let port_number = port.stringValue().to_string().trim().parse::<u16>().ok().filter(|&p| p > 0);
        let auth_choice =
            AUTH_CHOICES.get(auth.indexOfSelectedItem().max(0) as usize).map_or(AuthMethod::Password, |c| c.1);
        let key_text = key_file.stringValue().to_string().trim().to_string();
        let problem = if host_text.is_empty() {
            Some("Enter the server\u{2019}s name or address.")
        } else if port_number.is_none() {
            Some("The port must be a number from 1 to 65535.")
        } else if auth_choice == AuthMethod::Key && key_text.is_empty() {
            Some("Choose a key file, or log in another way.")
        } else {
            None
        };
        if let Some(problem) = problem {
            alert.setInformativeText(&ns(problem));
            continue;
        }
        let mut result = site.clone();
        // A user@host typed in the server field is split.
        let (user_text, host_text) = match host_text.split_once('@') {
            Some((u, h)) if user.stringValue().to_string().trim().is_empty() => (u.to_string(), h.to_string()),
            _ => (user.stringValue().to_string().trim().to_string(), host_text),
        };
        result.host = host_text.clone();
        result.port = port_number.unwrap_or(22);
        result.user = user_text;
        result.auth = auth_choice;
        result.key_file = (!key_text.is_empty()).then(|| PathBuf::from(key_text));
        let name_text = name.stringValue().to_string().trim().to_string();
        result.name = if name_text.is_empty() { host_text } else { name_text };
        let remote_text = remote.stringValue().to_string().trim().to_string();
        result.remote_dir = (!remote_text.is_empty()).then_some(remote_text);
        let local_text = local.stringValue().to_string().trim().to_string();
        result.local_dir = (!local_text.is_empty()).then(|| PathBuf::from(local_text));
        let password_text = password_field.stringValue().to_string();
        let save_state = save.state() == NSControlStateValueOn;
        return Some(FormResult {
            site: result,
            password: (!password_text.is_empty()).then_some(password_text),
            save: save_state,
        });
    }
}
