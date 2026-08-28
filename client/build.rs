use std::process::Command;

fn main() {
    println!("cargo::rerun-if-env-changed=DISCORDIA_VERSION");

    let version = std::env::var("DISCORDIA_VERSION")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(dev_version);

    println!("cargo::rustc-env=DISCORDIA_VERSION={version}");

    embed_info_plist();
}

/// macOS reads ATS and the TCC usage strings from the app's `Info.plist`, and a
/// bare `cargo run` has no bundle to carry one: ATS then stays at its default
/// and the webview cannot open the self-hosted `ws://` SFU at all. Linking the
/// file into the binary gives the unbundled build the same keys; a `.app` reads
/// its own copy and ignores this section.
fn embed_info_plist() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let plist = std::path::Path::new(&dir).join("Info.plist");
    println!("cargo::rerun-if-changed={}", plist.display());
    println!(
        "cargo::rustc-link-arg-bins=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        plist.display()
    );
}

fn dev_version() -> String {
    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    match git_short_sha() {
        Some(sha) => format!("{pkg}-dev+{sha}"),
        None => format!("{pkg}-dev"),
    }
}

fn git_short_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!sha.is_empty()).then_some(sha)
}
