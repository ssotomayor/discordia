use std::process::Command;

fn main() {
    println!("cargo::rerun-if-env-changed=DISCORDIA_VERSION");

    let version = std::env::var("DISCORDIA_VERSION")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(dev_version);

    println!("cargo::rustc-env=DISCORDIA_VERSION={version}");
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
