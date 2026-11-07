//! Builds forkz's Shortcuts actions (macos/Intents, in Swift: Apple's App
//! Intents can only be declared in Swift) into a static library that is
//! linked into the app. The constant values the Swift compiler extracts go
//! next to the binary (target/<profile>/ForkzIntents.swiftconstvalues), for
//! scripts/bundle.sh to turn into the metadata Shortcuts reads.
//!
//! Set FORKZ_NO_INTENTS=1 to build without them (no Xcode needed).

use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = "macos/Intents/ForkzIntents.swift";
const DEPLOYMENT_TARGET: &str = "14.0";

/// The protocols whose conforming types the compiler describes for the
/// App Intents metadata (as Xcode passes them).
const PROTOCOLS: &str = r#"["AppIntent","EntityQuery","AppEntity","TransientEntity","AppEnum","AppShortcutProviding","AppShortcutsProvider","AnyResolverProviding","AppIntentsPackage","DynamicOptionsProvider"]"#;

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");
    println!("cargo:rerun-if-env-changed=FORKZ_NO_INTENTS");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") || std::env::var_os("FORKZ_NO_INTENTS").is_some()
    {
        return;
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let arch = match std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => "arm64",
        Ok("x86_64") => "x86_64",
        Ok(other) => panic!("no Shortcuts actions for {other} (set FORKZ_NO_INTENTS=1)"),
        Err(_) => "arm64",
    };
    let triple = format!("{arch}-apple-macos{DEPLOYMENT_TARGET}");
    let sdk = run("xcrun", &["--sdk", "macosx", "--show-sdk-path"]);
    let swiftc = run("xcrun", &["--find", "swiftc"]);
    let toolchain = Path::new(&swiftc).ancestors().nth(3).expect("toolchain").to_path_buf();

    let protocols = out.join("protocols.json");
    std::fs::write(&protocols, PROTOCOLS).expect("protocols list");
    let library = out.join("libForkzIntents.a");
    let constants = out.join("ForkzIntents.swiftconstvalues");
    let release = std::env::var("PROFILE").as_deref() == Ok("release");
    let mut args: Vec<String> = vec![
        "-parse-as-library".into(),
        "-emit-library".into(),
        "-static".into(),
        "-wmo".into(),
        "-module-name".into(),
        "ForkzIntents".into(),
        "-swift-version".into(),
        "5".into(),
        "-target".into(),
        triple.clone(),
        "-sdk".into(),
        sdk.clone(),
        "-emit-const-values".into(),
        "-emit-const-values-path".into(),
        constants.display().to_string(),
        "-Xfrontend".into(),
        "-const-gather-protocols-file".into(),
        "-Xfrontend".into(),
        protocols.display().to_string(),
        "-o".into(),
        library.display().to_string(),
    ];
    args.push(if release { "-O".into() } else { "-Onone".into() });
    args.push(Path::new(SOURCE).canonicalize().expect("Swift source").display().to_string());
    let status = Command::new(&swiftc).args(&args).current_dir(&out).status().expect("swiftc runs");
    assert!(status.success(), "swiftc failed (set FORKZ_NO_INTENTS=1 to build without Shortcuts actions)");

    // For scripts/bundle.sh: target/<profile>/ (OUT_DIR is
    // target/<profile>/build/forkz-<hash>/out).
    if let Some(profile_dir) = out.ancestors().nth(3) {
        let _ = std::fs::copy(&constants, profile_dir.join("ForkzIntents.swiftconstvalues"));
        let _ = std::fs::write(profile_dir.join("ForkzIntents.triple"), &triple);
    }

    // Everything in it, though nothing in Rust refers to the types: App
    // Intents finds them at run time.
    println!("cargo:rustc-link-arg=-Wl,-force_load,{}", library.display());
    println!("cargo:rustc-link-search=native={sdk}/usr/lib/swift");
    println!("cargo:rustc-link-search=native={}/usr/lib/swift/macosx", toolchain.display());
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    for framework in ["AppIntents", "Foundation"] {
        println!("cargo:rustc-link-arg=-Wl,-framework,{framework}");
    }
}

fn run(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap_or_else(|e| panic!("{program}: {e}"));
    assert!(output.status.success(), "{program} {args:?} failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
