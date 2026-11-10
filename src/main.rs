use std::process::ExitCode;

const USAGE: &str = "\
forkz: a dual-pane file manager

Usage:
  forkz                                Start the app
  forkz FOLDER                         Start with FOLDER in the left pane
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
