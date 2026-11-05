//! Other apps forkz hands work to: the terminal (Open in Terminal) and the
//! compare tool (Compare Files). Only installed ones are offered.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use objc2_app_kit::NSWorkspace;

use super::util::{ns, url_path};
use crate::settings::{COMPARE_TOOLS, TERMINALS};

/// Where an app is, by bundle identifier (several may be given, comma-
/// separated, for apps that changed theirs).
pub fn app_path(bundle_ids: &str) -> Option<PathBuf> {
    let workspace = NSWorkspace::sharedWorkspace();
    bundle_ids
        .split(',')
        .find_map(|id| workspace.URLForApplicationWithBundleIdentifier(&ns(id.trim())).and_then(|u| url_path(&u)))
}

/// FileMerge comes with Xcode, inside it.
fn filemerge_path() -> Option<PathBuf> {
    let output = Command::new("/usr/bin/xcode-select").arg("-p").output().ok()?;
    let developer = PathBuf::from(String::from_utf8(output.stdout).ok()?.trim());
    let app = developer.parent()?.join("Applications/FileMerge.app");
    app.exists().then_some(app).or_else(|| app_path("com.apple.FileMerge"))
}

/// The installed terminals: (key, name).
pub fn installed_terminals() -> Vec<(&'static str, &'static str)> {
    TERMINALS.iter().filter(|(_, _, id)| app_path(id).is_some()).map(|(key, name, _)| (*key, *name)).collect()
}

/// The installed compare tools: (key, name).
pub fn installed_compare_tools() -> Vec<(&'static str, &'static str)> {
    COMPARE_TOOLS
        .iter()
        .filter(|(key, _, id)| match *key {
            "forkz" => true,
            "filemerge" => filemerge_path().is_some(),
            _ => app_path(id).is_some(),
        })
        .map(|(key, name, _)| (*key, *name))
        .collect()
}

fn terminal_app(key: &str) -> (String, PathBuf) {
    let found =
        TERMINALS.iter().find(|(k, _, _)| *k == key).and_then(|(k, _, id)| Some((k.to_string(), app_path(id)?)));
    found.unwrap_or_else(|| ("terminal".into(), PathBuf::from("/System/Applications/Utilities/Terminal.app")))
}

fn open_app(app: &Path, extra: &[&str]) -> io::Result<()> {
    let mut command = Command::new("/usr/bin/open");
    command.arg("-a").arg(app).args(extra);
    command.spawn().map(|_| ())
}

/// Opens a terminal window in a local folder.
pub fn open_terminal(key: &str, folder: &Path) -> io::Result<()> {
    let (key, app) = terminal_app(key);
    let folder = folder.to_string_lossy();
    match key.as_str() {
        "ghostty" => open_app(&app, &["-n", "--args", &format!("--working-directory={folder}")]),
        "kitty" => open_app(&app, &["-n", "--args", "--directory", &folder]),
        _ => open_app(&app, &[&folder]),
    }
}

/// Runs an SSH session (a shell command) in a new window of the chosen
/// terminal: Terminal and iTerm open a .command file, Warp a tab config
/// (its URL scheme cannot run commands), Ghostty and Kitty take it as
/// their command.
pub fn run_ssh(key: &str, command: &str) -> io::Result<()> {
    let (key, app) = terminal_app(key);
    if key == "warp" {
        return run_in_warp(&app, command);
    }
    let script = std::env::temp_dir().join(format!("forkz-ssh-{}.command", crate::sites::new_id()));
    std::fs::write(&script, format!("#!/bin/sh\nrm -f \"$0\"\nexec {command}\n"))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;
    let script_text = script.to_string_lossy();
    match key.as_str() {
        "iterm" => open_app(&app, &[&script_text]),
        "ghostty" => open_app(&app, &["-n", "--args", "-e", "/bin/sh", &script_text]),
        "kitty" => open_app(&app, &["-n", "--args", "/bin/sh", &script_text]),
        _ => open_app(&app, &[&script_text]),
    }
}

/// The tab config forkz writes for Warp (in ~/.warp/tab_configs/, replaced
/// each time) and opens with Warp's URL scheme.
const WARP_TAB_CONFIG: &str = "forkz_ssh_session";

#[derive(serde::Serialize)]
struct WarpTabConfig {
    name: String,
    title: String,
    panes: Vec<WarpPane>,
}

#[derive(serde::Serialize)]
struct WarpPane {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    commands: Vec<String>,
}

/// The tab config's text: one terminal pane running the command.
pub fn warp_tab_config(command: &str) -> String {
    let config = WarpTabConfig {
        name: "forkz SSH session".into(),
        title: "forkz SSH".into(),
        panes: vec![WarpPane { id: "main".into(), kind: "terminal".into(), commands: vec![command.to_string()] }],
    };
    toml::to_string(&config).unwrap_or_default()
}

fn run_in_warp(app: &Path, command: &str) -> io::Result<()> {
    let folder = dirs::home_dir().ok_or_else(|| io::Error::other("no home folder"))?.join(".warp/tab_configs");
    std::fs::create_dir_all(&folder)?;
    std::fs::write(folder.join(format!("{WARP_TAB_CONFIG}.toml")), warp_tab_config(command))?;
    let url = format!("warp://tab_config/{WARP_TAB_CONFIG}?new_window=true");
    // To the chosen Warp app (in case another app also handles warp://).
    open_app(app, &[&url])
}

/// Compares two local files in the chosen tool.
pub fn compare(key: &str, left: &Path, right: &Path) -> io::Result<()> {
    let tool = COMPARE_TOOLS.iter().find(|(k, _, _)| *k == key && *k != "forkz").or_else(|| COMPARE_TOOLS.get(1));
    let Some((key, name, id)) = tool else { return Err(io::Error::other("No compare tool is set.")) };
    let not_installed = || io::Error::new(io::ErrorKind::NotFound, format!("{name} is not installed."));
    let spawn = |program: &Path, args: &[&str]| -> io::Result<()> {
        Command::new(program).args(args).arg(left).arg(right).spawn().map(|_| ())
    };
    if *key == "filemerge" {
        filemerge_path().ok_or_else(not_installed)?;
        return spawn(Path::new("/usr/bin/opendiff"), &[]);
    }
    let app = app_path(id).ok_or_else(not_installed)?;
    // Each tool's own command line helper, inside the app.
    let helper = match *key {
        "kaleidoscope" => Some((app.join("Contents/MacOS/ksdiff"), vec![])),
        "beyondcompare" => Some((app.join("Contents/MacOS/bcomp"), vec![])),
        "bbedit" => Some((app.join("Contents/Helpers/bbdiff"), vec![])),
        "vscode" => Some((app.join("Contents/Resources/app/bin/code"), vec!["--diff"])),
        "araxis" => Some((app.join("Contents/Utilities/compare"), vec![])),
        _ => None,
    };
    match helper {
        Some((program, args)) if program.exists() => spawn(&program, &args),
        // Otherwise the app gets both files and decides.
        _ => {
            let (l, r) = (left.to_string_lossy(), right.to_string_lossy());
            open_app(&app, &[&l, &r])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warp_tab_config_runs_the_command() {
        let command = "ssh -p 22 -t 'me@host' 'cd '\\''/srv/my site'\\'' && exec $SHELL -l'";
        let text = warp_tab_config(command);
        let parsed: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(parsed["name"].as_str(), Some("forkz SSH session"));
        let pane = &parsed["panes"][0];
        assert_eq!(pane["type"].as_str(), Some("terminal"));
        assert_eq!(pane["commands"][0].as_str(), Some(command), "quotes survive the TOML");
    }
}
