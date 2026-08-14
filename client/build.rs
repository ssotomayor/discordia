//! Stamps the build's own version into the binary.
//!
//! `CARGO_PKG_VERSION` is `0.1.0` in every build this project has ever
//! published — 223 pre-releases at the time of writing — because the release
//! number lives only in the tag CI creates
//! (`v0.1.0-pre.${{ github.run_number }}`, `ci.yml`) and never reached the
//! binary. So the app could not name which build it was, and "which artifact
//! did you download?" — the first question asked of the icon report in
//! `TODO.md`, among others — had no answer available to the person reporting.
//!
//! CI sets `DISCORDIA_VERSION` to exactly the tag it publishes, so the string
//! on screen and the release on GitHub are the same string. Anything built
//! without it — `cargo run`, `dx serve`, a contributor's checkout — gets one
//! that cannot be mistaken for a release.
//!
//! Kept as a build script rather than read at runtime because there is nothing
//! to read: the answer is fixed when the binary is produced, and a runtime
//! lookup would be a lookup of something that is not there.

use std::process::Command;

fn main() {
    println!("cargo::rerun-if-env-changed=DISCORDIA_VERSION");

    // Empty counts as unset. `windows-release.yml` builds on both tag pushes
    // and manual runs, and an expression that yields "" on the manual path is
    // simpler there than conditioning the whole `env:` block.
    let version = std::env::var("DISCORDIA_VERSION")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(dev_version);

    println!("cargo::rustc-env=DISCORDIA_VERSION={version}");
}

/// What a build outside CI calls itself.
///
/// The `-dev` is the load-bearing part, not decoration. A local build labelled
/// `v0.1.0-pre.223` would be indistinguishable from the published one in a bug
/// report, which is the exact confusion this file exists to remove.
///
/// The commit is a convenience on top: it survives being pasted into an issue
/// and says which tree the build came from. It is captured when this script
/// last ran, so a dev build's sha can lag a commit or two — cargo has no cheap
/// way to make `HEAD` a dependency that also survives git worktrees, and this
/// is a label on a local build rather than evidence about a released one.
fn dev_version() -> String {
    let pkg = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    match git_short_sha() {
        Some(sha) => format!("{pkg}-dev+{sha}"),
        None => format!("{pkg}-dev"),
    }
}

/// `git rev-parse --short HEAD`, or `None` for any reason at all — no git on
/// PATH, a source tarball with no repository, a shallow checkout. None of those
/// should fail a build over a label.
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
