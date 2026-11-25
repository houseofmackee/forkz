use std::process::ExitCode;

const USAGE: &str = "\
forkz: a dual-pane file manager

Usage:
  forkz                                Start the app
  forkz FOLDER                         Open FOLDER in forkz (forkz . for this one)
  forkz sftp://[user@]host[:port]/path Connect (using a saved site if one matches);
                                       also ftp, ftps, dav, davs, s3, gs, azure,
                                       box, smb, afp, and nfs addresses
  forkz 'forkz://open?path=...'        Follow a forkz:// link (see the README)
  forkz --version                      Print the version
  forkz --help       Print this help
";

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FORKZ_LOG").unwrap_or_else(|_| "forkz=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    // Launch Services may pass `-psn_*`, and Cocoa reads `-Name value`
    // defaults (like `-AppleLanguages "(de)"`); neither is ours.
    let mut args = Vec::new();
    let mut rest = std::env::args().skip(1);
    while let Some(arg) = rest.next() {
        if arg.starts_with("-psn_") {
            continue;
        }
        if arg.starts_with("-Apple") || arg.starts_with("-NS") {
            rest.next();
            continue;
        }
        args.push(arg);
    }
    match args.first().map(String::as_str) {
        None => match forkz::ui::run(None) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("forkz: {e:#}");
                ExitCode::FAILURE
            }
        },
        Some("--version" | "-V") => {
            println!("forkz {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("--help" | "-h") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(target)
            if target
                .split_once("://")
                .is_some_and(|(scheme, _)| forkz::sites::Protocol::from_scheme(scheme).is_some())
                || target.starts_with("forkz:")
                || std::path::Path::new(target).is_dir() =>
        {
            // The installed app: handed to it (running, or started), as
            // `open` does, rather than a second copy.
            if hand_to_app(target) {
                return ExitCode::SUCCESS;
            }
            match forkz::ui::run(Some(target.to_string())) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("forkz: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        Some(other) => {
            eprintln!("forkz: unknown argument \"{other}\"\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

/// Hands a folder or address to the forkz app through a forkz:// link
/// (macOS gives it to the running app, or starts it). Only for the app's
/// own command (inside forkz.app, also through the /usr/local/bin link): a
/// build run from source runs itself.
fn hand_to_app(target: &str) -> bool {
    let executable = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok());
    if !executable.is_some_and(|e| e.to_string_lossy().contains(".app/Contents/MacOS/")) {
        return false;
    }
    std::process::Command::new("/usr/bin/open").arg(link_for(target)).status().is_ok_and(|s| s.success())
}

/// The forkz:// link that opens a folder or address (a link as it is).
fn link_for(target: &str) -> String {
    if target.starts_with("forkz:") {
        return target.to_string();
    }
    // A folder as a full path ("." means nothing to the app).
    let place = if target.contains("://") {
        target.to_string()
    } else {
        std::path::Path::new(target).canonicalize().map_or_else(|_| target.to_string(), |p| p.display().to_string())
    };
    format!("forkz://open?path={}", percent_encoding::utf8_percent_encode(&place, percent_encoding::NON_ALPHANUMERIC))
}

#[cfg(test)]
mod tests {
    use super::link_for;

    #[test]
    fn makes_links_for_folders_and_addresses() {
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        let encoded =
            |text: &str| percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC).to_string();
        assert_eq!(link_for("."), format!("forkz://open?path={}", encoded(&here.display().to_string())));
        assert_eq!(link_for("sftp://me@host/var/www"), "forkz://open?path=sftp%3A%2F%2Fme%40host%2Fvar%2Fwww");
        assert_eq!(link_for("forkz://workspace?name=Work"), "forkz://workspace?name=Work");
        // Decoded, a link gives back the place (the app's link parser).
        let link = link_for("/tmp/a b&c");
        assert!(link.ends_with("%2Ftmp%2Fa%20b%26c") || link.contains("private"), "{link}");
    }
}
