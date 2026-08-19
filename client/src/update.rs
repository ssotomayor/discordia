//! Deciding what to download, and refusing to trust it until it verifies.
//!
//! `version` answers "is there something newer". This answers the two questions
//! that come after: *which file is mine*, and *is it really ours*.
//!
//! The key is compiled in from the repository root rather than fetched, which
//! is the only arrangement that means anything. A key downloaded at check time
//! is a key an attacker who can answer for GitHub also controls, and verifying
//! against it would be theatre.

use dioxus::prelude::*;
use minisign_verify::{PublicKey, Signature};

/// The public half of the key CI signs releases with, from the repository root.
///
/// `include_str!` rather than a literal so there is exactly one copy: the file
/// users verify downloads against, the file CI checks its own signatures
/// against before publishing, and this are the same bytes. Rotating it is one
/// edit, and a build that disagrees with the published key cannot exist.
const PUBLIC_KEY: &str = include_str!("../../release-signing.pub");

/// What this build would download to update itself.
///
/// `None` on any platform we do not publish an artifact for, which is the
/// honest answer for a self-built binary — the update flow simply does not
/// offer itself rather than guessing at a file that was never uploaded.
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", _) => Some("Discordia-windows-setup.exe"),
        ("macos", "aarch64") => Some("Discordia-macos-arm64.dmg"),
        ("linux", "x86_64") => Some("Discordia-linux-x86_64.AppImage"),
        _ => None,
    }
}

/// The signature that accompanies an artifact, by the convention `minisign -S`
/// uses and `ci.yml` relies on.
pub fn signature_name(asset: &str) -> String {
    format!("{asset}.minisig")
}

/// Why a download was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// The compiled-in key is not a key. Only reachable by breaking the build.
    BadPublicKey(String),
    /// The `.minisig` did not parse — truncated download, or an error page
    /// saved under the name of a signature.
    BadSignature(String),
    /// Parsed, and does not match. The interesting one.
    Mismatch(String),
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadPublicKey(e) => write!(f, "the built-in signing key is unreadable: {e}"),
            Self::BadSignature(e) => write!(f, "the signature file is not a signature: {e}"),
            Self::Mismatch(e) => write!(f, "the download does not match its signature: {e}"),
        }
    }
}

/// Check `data` against `sig`, both freshly downloaded, using the compiled-in
/// key.
///
/// Takes the whole artifact in memory. That is a deliberate ceiling rather than
/// an oversight: streaming verification would mean writing bytes to disk before
/// knowing whether to trust them, and the largest thing published is 135MB —
/// which a machine running this app already has, since it is about to hold the
/// same file open to run it.
pub fn verify(data: &[u8], sig: &str) -> Result<(), VerifyError> {
    // `from_base64` on the key line we picked out, not `decode` on the file:
    // `decode` takes the comment from line 1 and the key from line 2, so it
    // depends on the file having exactly the shape minisign writes today.
    // Finding the key line ourselves survives a blank line or a reworded
    // comment, neither of which changes the key.
    let key = PublicKey::from_base64(trim_key(PUBLIC_KEY))
        .map_err(|e| VerifyError::BadPublicKey(e.to_string()))?;
    let signature = Signature::decode(sig).map_err(|e| VerifyError::BadSignature(e.to_string()))?;
    key.verify(data, &signature, false)
        .map_err(|e| VerifyError::Mismatch(e.to_string()))
}

/// The base64 line out of a minisign public key file.
///
/// `PublicKey::decode` wants the key alone; the file it lives in also carries
/// an `untrusted comment:` line that minisign writes and that nothing signs.
fn trim_key(file: &str) -> &str {
    file.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .unwrap_or("")
}

/// Where a verified download is staged before anything is done with it.
///
/// Beside the file it will replace on Linux, so the rename that installs it is
/// within one filesystem and therefore atomic — a cross-device rename is a copy
/// with a window in the middle where the AppImage is half-written. In the
/// temp dir everywhere else, where nothing is being replaced in place.
fn staging_path(asset: &str) -> std::path::PathBuf {
    running_appimage()
        .and_then(|p| p.parent().map(|d| d.join(format!(".{asset}.partial"))))
        .unwrap_or_else(|| std::env::temp_dir().join(asset))
}

/// The AppImage this process is running from, if it is running from one.
///
/// AppImage sets `APPIMAGE` to the absolute path of the image itself; the
/// executable inside sees a path in a mount that vanishes on exit, so
/// `current_exe` is the wrong question. `None` everywhere else, which is what
/// keeps the in-place replacement Linux-only without a `cfg`.
fn running_appimage() -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(std::env::var_os("APPIMAGE")?);
    p.is_file().then_some(p)
}

/// Download the artifact and its signature, and refuse to keep either unless
/// they agree.
///
/// The bytes never reach a path a user could run until they have verified: the
/// staging file is written only after `verify` succeeds, and named with a
/// leading dot and a `.partial` suffix so a crash between write and rename
/// leaves something obviously unfinished rather than something that looks like
/// an installer.
pub async fn fetch_verified(
    asset: &str,
    asset_url: &str,
    signature_url: &str,
) -> Result<std::path::PathBuf, String> {
    let client = reqwest::Client::new();
    let sig = client
        .get(signature_url)
        .send()
        .await
        .map_err(|e| format!("could not fetch the signature: {e}"))?
        .text()
        .await
        .map_err(|e| format!("could not read the signature: {e}"))?;
    let body = client
        .get(asset_url)
        .send()
        .await
        .map_err(|e| format!("could not download the update: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("the download was cut short: {e}"))?;

    verify(&body, &sig).map_err(|e| e.to_string())?;

    let path = staging_path(asset);
    std::fs::write(&path, &body).map_err(|e| format!("could not save the update: {e}"))?;
    Ok(path)
}

/// What installing means on this platform, decided from the path alone so the
/// decision is testable without any of it happening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// Rename over the running AppImage, then re-exec it.
    ReplaceAppImage {
        target: std::path::PathBuf,
        staged: std::path::PathBuf,
    },
    /// Hand the verified file to the OS: the Windows installer installs, and
    /// the macOS disk image mounts for the user to drag across. Neither can be
    /// finished without the user, because both are gated by SmartScreen and
    /// Gatekeeper on an unsigned binary.
    Open(std::path::PathBuf),
}

/// Decide, without doing anything.
pub fn plan_install(staged: std::path::PathBuf) -> Install {
    match running_appimage() {
        Some(target) => Install::ReplaceAppImage { target, staged },
        None => Install::Open(staged),
    }
}

/// Put `staged` where `target` is, executable, in one step.
///
/// Split out from everything around it because it is the only part that can
/// destroy something the user already had, and it is the only part that can be
/// tested here — a Windows machine cannot run an AppImage, but it can watch
/// this rename a file over another one.
///
/// `rename` rather than remove-then-copy: the running image stays openable
/// until the instant it is replaced, and there is no moment where the path
/// exists but holds half a file. On Linux the kernel keeps the old inode alive
/// for the running process, so replacing the image a process is executing is
/// safe — that is the same property `cargo` relies on to replace its own
/// binary.
pub fn swap(staged: &std::path::Path, target: &std::path::Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(target)
            .map(|m| m.permissions().mode())
            .unwrap_or(0o755);
        std::fs::set_permissions(staged, std::fs::Permissions::from_mode(mode))
            .map_err(|e| format!("could not make the update executable: {e}"))?;
    }
    std::fs::rename(staged, target).map_err(|e| format!("could not install the update: {e}"))
}

/// Carry out what `plan_install` decided.
///
/// The AppImage path re-execs and leaves it to the caller to exit; nothing here
/// kills the process, because deciding when to drop a voice call is not this
/// module's business.
pub fn perform(install: &Install) -> Result<(), String> {
    match install {
        Install::ReplaceAppImage { target, staged } => {
            swap(staged, target)?;
            std::process::Command::new(target)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("installed, but could not restart: {e}"))
        }
        Install::Open(path) => {
            crate::app::open_external(&path.to_string_lossy());
            Ok(())
        }
    }
}

/// How far the one-click install has got.
#[derive(Clone, PartialEq)]
enum Phase {
    Offered,
    /// Downloading and verifying, which are one step to the user because a
    /// download that fails to verify is not a download that happened.
    Working,
    /// Replaced, and waiting to be restarted into.
    Restart,
    /// Handed to the OS installer, which now owns the rest.
    HandedOff,
    Failed(String),
}

/// The update notice, and the install behind it.
///
/// Nothing here starts on its own. The check that produced `update` runs
/// unattended, but every byte after it waits for a click — an app that
/// downloads and replaces itself unasked is a different product than one that
/// offers to.
#[component]
pub fn UpdateNotice(update: crate::version::Update) -> Element {
    let mut phase = use_signal(|| Phase::Offered);

    let install = update.download.clone();
    let start = move |_| {
        let Some(d) = install.clone() else { return };
        phase.set(Phase::Working);
        spawn(async move {
            match fetch_verified(&d.asset, &d.asset_url, &d.signature_url).await {
                Err(e) => phase.set(Phase::Failed(e)),
                Ok(staged) => {
                    let plan = plan_install(staged);
                    let restarts = matches!(plan, Install::ReplaceAppImage { .. });
                    match perform(&plan) {
                        Err(e) => phase.set(Phase::Failed(e)),
                        Ok(()) if restarts => phase.set(Phase::Restart),
                        Ok(()) => phase.set(Phase::HandedOff),
                    }
                }
            }
        });
    };

    let tag = update.tag.clone();
    let page = update.url.clone();
    rsx! {
        match phase() {
            // No artifact for this platform — a self-built binary, or an
            // architecture CI does not publish for. The release page is the
            // honest offer there.
            Phase::Offered if update.download.is_none() => rsx! {
                button {
                    class: "text-[10px] text-[var(--accent)] underline",
                    title: "Opens the release page in your browser",
                    onclick: move |_| crate::app::open_external(&page),
                    "{tag} available"
                }
            },
            Phase::Offered => rsx! {
                button {
                    class: "text-[10px] text-[var(--accent)] underline",
                    title: "Downloads {tag}, checks its signature, and installs it",
                    onclick: start,
                    "update to {tag}"
                }
            },
            Phase::Working => rsx! {
                span { class: "text-[10px] text-[var(--text-muted)]", "downloading {tag}…" }
            },
            Phase::Restart => rsx! {
                span { class: "text-[10px] text-[var(--up)]", "{tag} installed — restarting" }
            },
            Phase::HandedOff => rsx! {
                span { class: "text-[10px] text-[var(--up)]", "verified — finish in the installer" }
            },
            // Deliberately not a toast that goes away. A refused signature is
            // the one outcome worth interrupting for: it means the file served
            // was not the file CI built.
            Phase::Failed(e) => rsx! {
                span {
                    class: "text-[10px] text-[var(--danger)] max-w-[320px] truncate",
                    title: "{e}",
                    "update failed — {e}"
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The signature CI published beside `Discordia-macos-arm64.dmg` in
    /// `v0.1.0-pre.332`, the first release it signed — downloaded from the
    /// release, not written by hand.
    ///
    /// Here so the parser meets what GitHub actually serves. The `.dmg` it
    /// signs is 56MB and is not, so nothing here can assert a *successful*
    /// verification; `verifies_the_real_release_artifact` does that, and needs
    /// a network to do it.
    const REAL_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\n\
        RURC4u05WSE3oGJQqK+9DS0GcRx2CylPzBUsLnAj6CgqZx/ssJ4Xc6Ix0OQZZnVzRjKbvkiIqMguQDR9WpoSmK/FsAqSxCsesg8=\n\
        trusted comment: discordia v0.1.0-pre.332 Discordia-macos-arm64.dmg\n\
        EEn/Pl9c0ILDWdYWpVkHHRQ0ZEmwfAy/mqOS4KD31ZlRTYriAZhl4txFe3DDC4lm0gGp9tsVJYrlFFo1U3RrBg==\n";

    /// The key that ships in the binary has to be a key. Cheap, and the only
    /// test that fails if someone commits a truncated `release-signing.pub`.
    #[test]
    fn the_built_in_key_parses() {
        assert!(
            PublicKey::from_base64(trim_key(PUBLIC_KEY)).is_ok(),
            "release-signing.pub does not decode as a minisign public key"
        );
    }

    /// The comment line is not part of the key, and feeding it in whole is the
    /// obvious mistake.
    #[test]
    fn the_comment_line_is_not_part_of_the_key() {
        assert!(!trim_key(PUBLIC_KEY).contains("untrusted comment"));
        assert!(trim_key(PUBLIC_KEY).starts_with("RW"));
    }

    /// The one that matters: content that is not what was signed is refused.
    /// A verifier that accepts everything passes every other test here.
    #[test]
    fn content_that_was_not_signed_is_refused() {
        let err = verify(b"not the dmg", REAL_SIGNATURE).unwrap_err();
        assert!(
            matches!(err, VerifyError::Mismatch(_)),
            "expected a mismatch, got {err:?}"
        );
    }

    /// A truncated or substituted `.minisig` — an HTML error page saved under
    /// that name is the realistic case — is refused as a signature rather than
    /// silently treated as a mismatch.
    #[test]
    fn something_that_is_not_a_signature_is_refused() {
        for junk in ["", "<!doctype html>", "untrusted comment: only\n"] {
            let err = verify(b"anything", junk).unwrap_err();
            assert!(
                matches!(err, VerifyError::BadSignature(_)),
                "{junk:?} was not rejected as a malformed signature: {err:?}"
            );
        }
    }

    /// The only test that proves a *successful* verification, and the only one
    /// that proves the whole arrangement holds end to end: the key CI signs
    /// with, the key compiled in here, and a file GitHub is serving right now
    /// are the same three things.
    ///
    /// `#[ignore]`d because it downloads 56MB, in the repository's existing
    /// sense of the word — not optional, just not something every `cargo test`
    /// should do. Run it when touching this module:
    ///
    /// ```text
    /// cargo test -p dioxusfun --bins -- --ignored verifies_the_real
    /// ```
    #[test]
    #[ignore = "downloads a 56MB release artifact"]
    fn verifies_the_real_release_artifact() {
        let base = "https://github.com/ssotomayor/discordia/releases/download/v0.1.0-pre.332";
        let dmg = format!("{base}/Discordia-macos-arm64.dmg");
        // The async client, not `reqwest::blocking`: turning that feature on
        // for one ignored test would put a second HTTP stack in every build.
        let rt = tokio::runtime::Runtime::new().expect("build a runtime");
        let body = rt.block_on(async {
            reqwest::get(&dmg)
                .await
                .expect("download the artifact")
                .bytes()
                .await
                .expect("read the artifact")
        });
        verify(&body, REAL_SIGNATURE).expect("the published artifact must verify");

        // And the same bytes with one flipped byte must not, or the assertion
        // above only proves that `verify` returns Ok.
        let mut tampered = body.to_vec();
        tampered[0] ^= 0xff;
        assert!(verify(&tampered, REAL_SIGNATURE).is_err());
    }

    /// The destructive step, watched doing its job. Runs everywhere, including
    /// the Windows machine this was written on, because it is `rename` and a
    /// permission bit rather than anything Linux-specific.
    #[test]
    fn swap_replaces_the_target_in_one_step() {
        let dir = std::env::temp_dir().join(format!("dxf-swap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("Discordia.AppImage");
        let staged = dir.join(".Discordia.AppImage.partial");
        std::fs::write(&target, b"old").unwrap();
        std::fs::write(&staged, b"new").unwrap();

        swap(&staged, &target).unwrap();

        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert!(!staged.exists(), "the staging file outlived the install");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A failed swap must not have eaten the thing it was replacing. This is
    /// the case that decides whether a broken update costs a restart or costs
    /// the user their app.
    #[test]
    fn a_swap_that_cannot_run_leaves_the_original_alone() {
        let dir = std::env::temp_dir().join(format!("dxf-swap-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("Discordia.AppImage");
        std::fs::write(&target, b"old").unwrap();

        let err = swap(&dir.join("was-never-downloaded"), &target).unwrap_err();

        assert!(err.contains("could not"), "unhelpful error: {err}");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"old",
            "the original was destroyed by an install that never happened"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Off an AppImage — every developer machine, and Windows and macOS
    /// always — installing means handing the file to the OS, never replacing
    /// anything in place.
    #[test]
    fn without_an_appimage_nothing_is_replaced() {
        assert!(
            running_appimage().is_none(),
            "APPIMAGE is set while running tests, which this test cannot interpret"
        );
        assert!(matches!(
            plan_install(std::path::PathBuf::from("x")),
            Install::Open(_)
        ));
    }

    /// Every platform we publish for names a file that exists in the release,
    /// and every platform we do not publish for names nothing.
    #[test]
    fn the_asset_name_matches_what_ci_uploads() {
        let published = [
            "Discordia-windows-setup.exe",
            "Discordia-macos-arm64.dmg",
            "Discordia-linux-x86_64.AppImage",
        ];
        if let Some(name) = asset_name() {
            assert!(
                published.contains(&name),
                "{name} is not one of the files ci.yml stages"
            );
            assert_eq!(signature_name(name), format!("{name}.minisig"));
        }
    }
}
