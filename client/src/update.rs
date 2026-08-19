//! Deciding what to download, and refusing to trust it until it verifies.
//!
//! `version` answers "is there something newer". This answers the two questions
//! that come after: *which file is mine*, and *is it really ours*.
//!
//! The key is compiled in from the repository root rather than fetched, which
//! is the only arrangement that means anything. A key downloaded at check time
//! is a key an attacker who can answer for GitHub also controls, and verifying
//! against it would be theatre.

use std::time::Duration;

use dioxus::prelude::*;
use minisign_verify::{PublicKey, Signature};

/// The public half of the key CI signs releases with, from the repository root.
///
/// `include_str!` rather than a literal so there is exactly one copy: the file
/// users verify downloads against, the file CI checks its own signatures
/// against before publishing, and this are the same bytes. Rotating it is one
/// edit, and a build that disagrees with the published key cannot exist.
const PUBLIC_KEY: &str = include_str!("../../release-signing.pub");

/// The file that marks a portable unzip, written into the archive by `ci.yml`.
///
/// Windows ships twice from one build, and the two update differently: the
/// installer replaces an installation, the portable is a folder the user
/// unzipped wherever they liked. Nothing about the path distinguishes them —
/// hence a file, which travels in the zip and is replaced along with it.
const PORTABLE_MARKER: &str = "PORTABLE";

/// The directory a portable copy lives in, if this is one.
fn portable_dir() -> Option<std::path::PathBuf> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    dir.join(PORTABLE_MARKER).is_file().then_some(dir)
}

/// What this build would download to update itself.
///
/// `None` on any platform we do not publish an artifact for, which is the
/// honest answer for a self-built binary — the update flow simply does not
/// offer itself rather than guessing at a file that was never uploaded.
///
/// Not a `const`: on Windows the answer depends on how this copy got here.
/// Handing the installer to a portable copy is not a lesser update, it is the
/// wrong one — it installs a second Discordia elsewhere and leaves the folder
/// the user actually opens untouched, having reported success.
pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", _) if portable_dir().is_some() => Some("Discordia-windows-portable.zip"),
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
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .or_else(portable_dir)
        .map(|d| d.join(format!(".{asset}.partial")))
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
    /// Unpack the verified zip over the folder a portable copy runs from, then
    /// re-exec. The other Windows build; see `PORTABLE_MARKER`.
    ReplacePortable {
        dir: std::path::PathBuf,
        zip: std::path::PathBuf,
    },
    /// Start the Windows installer **and get out of its way**.
    ///
    /// It writes over `Discordia.exe`, and Windows will not let it while this
    /// process is the one running that file. Left alive, the installer stops on
    /// "Error opening file for writing" and offers Abort / Retry / Ignore — a
    /// dialog that means the update failed and reads like the download was
    /// corrupt. Separate from `Open` for exactly this: the two look alike and
    /// one of them cannot be done from inside the running app.
    RunInstaller(std::path::PathBuf),
    /// Hand the verified file to the OS and stay. The macOS disk image mounts
    /// for the user to drag across, which does not require this process to be
    /// gone — macOS replaces a running bundle happily.
    Open(std::path::PathBuf),
}

/// Decide, without doing anything.
pub fn plan_install(staged: std::path::PathBuf) -> Install {
    if let Some(target) = running_appimage() {
        return Install::ReplaceAppImage { target, staged };
    }
    match portable_dir() {
        Some(dir) => Install::ReplacePortable { dir, zip: staged },
        None if cfg!(target_os = "windows") => Install::RunInstaller(staged),
        None => Install::Open(staged),
    }
}

/// The suffix the outgoing executable is parked under.
///
/// Windows refuses to overwrite a running `.exe` but allows renaming one, which
/// is the whole trick: the file the OS is executing keeps its handle under a new
/// name while the new build takes the old one.
const OUTGOING: &str = ".old";

/// Unpack a verified portable zip over the folder it is running from.
///
/// **All of it or none of it.** Every file displaced is kept until the whole
/// thing has landed, and any failure walks the replacements back out in
/// reverse. The first version of this only rolled back the executable, on the
/// theory that the files before it were interchangeable — they are not. A
/// rename can fail part way through for reasons that have nothing to do with
/// us (a scanner holding a file open, a permission, a path length), and
/// stopping there left a folder that was neither version, with nothing saying
/// which half was which.
///
/// Ordering still matters inside that: the executable moves last, because it is
/// the one file that cannot simply be overwritten and the one whose loss leaves
/// nothing to launch.
///
/// `exe` is passed rather than read from `current_exe` so this can be exercised
/// with ordinary files, on any OS. The destructive part of an updater is the
/// part that most deserves a test and least tolerates being tested in
/// production.
pub fn replace_portable(
    zip: &std::path::Path,
    dir: &std::path::Path,
    exe: &str,
) -> Result<(), String> {
    let staging = dir.join(".update-staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| format!("could not stage the update: {e}"))?;

    let unpacked = unzip_into(zip, &staging).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(&staging);
    })?;

    // Refuse before touching anything if the archive is not what it claims. An
    // update that lands every file except the program is worse than none.
    if !staging.join(exe).is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("the update archive contains no {exe}"));
    }

    // Every replacement made so far, newest last, so a failure can walk back
    // out the way it came in. Without this the loop below could stop half way
    // and leave a folder that is neither version — some files new, some old,
    // and nothing saying so.
    let backups = staging.join(".replaced");
    let mut done: Vec<Undo> = Vec::new();

    let outcome = (|| {
        for rel in unpacked.iter().filter(|r| r.as_os_str() != exe) {
            place(&staging, &backups, dir, rel, &mut done)?;
        }
        // The program last, and parked rather than deleted: Windows will not
        // overwrite a running `.exe` but will rename one, and the old file
        // keeps its handle under that name until the next start.
        let live = dir.join(exe);
        let outgoing = dir.join(format!("{exe}{OUTGOING}"));
        let _ = std::fs::remove_file(&outgoing);
        if live.exists() {
            std::fs::rename(&live, &outgoing)
                .map_err(|e| format!("could not move the running program aside: {e}"))?;
            done.push(Undo {
                placed: live.clone(),
                restore_from: Some(outgoing.clone()),
            });
        }
        std::fs::rename(staging.join(exe), &live)
            .map_err(|e| format!("could not install the new program: {e}"))?;
        Ok(())
    })();

    if let Err(e) = outcome {
        // All of it, not just the last step. "The update failed" has to mean
        // the folder is as it was; anything else asks a user to work out which
        // half of their install is which.
        for u in done.into_iter().rev() {
            let _ = std::fs::remove_file(&u.placed);
            if let Some(from) = u.restore_from {
                let _ = std::fs::rename(&from, &u.placed);
            }
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e);
    }

    let _ = std::fs::remove_dir_all(&staging);
    Ok(())
}

/// One replacement, and how to take it back.
struct Undo {
    /// Where the new file was put.
    placed: std::path::PathBuf,
    /// Where the file it displaced is waiting, if there was one.
    restore_from: Option<std::path::PathBuf>,
}

/// Move one unpacked file into place, recording what it displaced.
fn place(
    staging: &std::path::Path,
    backups: &std::path::Path,
    dir: &std::path::Path,
    rel: &std::path::Path,
    done: &mut Vec<Undo>,
) -> Result<(), String> {
    let to = dir.join(rel);
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not make room for {}: {e}", rel.display()))?;
    }
    let mut restore_from = None;
    if to.exists() {
        let kept = backups.join(rel);
        if let Some(parent) = kept.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not set {} aside: {e}", rel.display()))?;
        }
        std::fs::rename(&to, &kept)
            .map_err(|e| format!("could not set {} aside: {e}", rel.display()))?;
        restore_from = Some(kept);
    }
    if let Err(e) = std::fs::rename(staging.join(rel), &to) {
        // Undo this one here; the caller unwinds the rest.
        if let Some(kept) = &restore_from {
            let _ = std::fs::rename(kept, &to);
        }
        return Err(format!("could not replace {}: {e}", rel.display()));
    }
    done.push(Undo {
        placed: to,
        restore_from,
    });
    Ok(())
}

/// Extract every file in `zip` under `into`, returning their relative paths.
///
/// Entries are rejected rather than sanitised if they escape the destination.
/// The archive is signed, so this is not the threat it would otherwise be — but
/// "we verified it" is a reason to expect well-formed input, not a reason to
/// write wherever it says.
fn unzip_into(
    zip: &std::path::Path,
    into: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>, String> {
    let file = std::fs::File::open(zip).map_err(|e| format!("could not open the update: {e}"))?;
    let mut archive =
        zip::ZipArchive::new(file).map_err(|e| format!("the update is not a zip: {e}"))?;
    let mut written = Vec::new();
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("could not read the update: {e}"))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(format!(
                "the update contains an unsafe path: {}",
                entry.name()
            ));
        };
        let out = into.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| format!("could not unpack: {e}"))?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("could not unpack: {e}"))?;
        }
        let mut to = std::fs::File::create(&out).map_err(|e| format!("could not unpack: {e}"))?;
        std::io::copy(&mut entry, &mut to).map_err(|e| format!("could not unpack: {e}"))?;
        written.push(rel);
    }
    Ok(written)
}

/// Delete the executable a previous update parked, if it is still there.
///
/// Called at startup because that is the first moment it can succeed: while the
/// old build was running, Windows held its file open.
pub fn sweep_outgoing() {
    let Some(dir) = portable_dir() else { return };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(name) = exe.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let _ = std::fs::remove_file(dir.join(format!("{name}{OUTGOING}")));
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
/// The AppImage path starts the new image and returns; **the caller must then
/// exit**, and `UpdateNotice` does. Splitting it that way keeps the decision of
/// when to drop a voice call out of here — but the two halves are not optional
/// separately: `spawn` without an exit leaves the user with two windows, the
/// stale one still usable, while the UI says it is restarting.
pub fn perform(install: &Install) -> Result<(), String> {
    match install {
        Install::ReplaceAppImage { target, staged } => {
            swap(staged, target)?;
            std::process::Command::new(target)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("installed, but could not restart: {e}"))
        }
        Install::ReplacePortable { dir, zip } => {
            let exe = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                .ok_or("could not find the running program to replace")?;
            replace_portable(zip, dir, &exe)?;
            let _ = std::fs::remove_file(zip);
            std::process::Command::new(dir.join(&exe))
                .current_dir(dir)
                .spawn()
                .map(|_| ())
                .map_err(|e| format!("installed, but could not restart: {e}"))
        }
        Install::RunInstaller(path) | Install::Open(path) => open_installer(path),
    }
}

/// Start the verified installer, and say so if it does not start.
///
/// Not `app::open_external`, for two reasons that both bit at once. It ignores
/// the spawn result, so a launch that never happened returned the same `()` as
/// one that did — the app said "finish in the installer" with no installer. And
/// on Windows it goes through `cmd /C start <arg>`, where a path containing a
/// space arrives quoted and `start` reads a lone quoted token as the *window
/// title*: it opens an empty console and runs nothing. `%TEMP%` sits under the
/// user's profile, so any account named "John Doe" hits that.
///
/// Windows therefore runs the installer directly — it is an executable, and
/// there is nothing `cmd` was adding except a way to fail quietly.
fn open_installer(path: &std::path::Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    let mut cmd = std::process::Command::new(path);
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    };
    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("downloaded and verified, but could not start it: {e}"))
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
    /// The installer is up and this process is leaving so it can write.
    Closing,
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
                    // Three of the four paths need this process gone, for two
                    // different reasons that both end the same way: the two
                    // in-place replacements have already started a new build,
                    // and the Windows installer cannot write `Discordia.exe`
                    // while this process is running it.
                    let leaves = !matches!(plan, Install::Open(_));
                    let after = match plan {
                        // The new build is already up; this is the old one
                        // holding the window the user is looking at and, when
                        // self-hosting, the port the new one needs.
                        Install::ReplaceAppImage { .. } | Install::ReplacePortable { .. } => {
                            Phase::Restart
                        }
                        // Nothing is up yet. The installer puts the new build
                        // in place and the user starts it.
                        _ => Phase::Closing,
                    };
                    match perform(&plan) {
                        Err(e) => phase.set(Phase::Failed(e)),
                        Ok(()) if leaves => {
                            phase.set(after);
                            // Long enough for the message to be read, and — on
                            // the installer path — long enough for it to be up
                            // before its reason for existing disappears.
                            tokio::time::sleep(Duration::from_millis(900)).await;
                            // `exit`, not a graceful window close: every
                            // shutdown path here runs pre-update code, and the
                            // one thing that must be true afterwards is that
                            // none of it is still running. A teardown that hangs
                            // holds `Discordia.exe` open, which is the whole
                            // failure this is fixing.
                            std::process::exit(0);
                        }
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
            Phase::Closing => rsx! {
                span { class: "text-[10px] text-[var(--up)]", "closing so the installer can finish" }
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

    /// Windows without a portable marker means the installer, and the installer
    /// means this process has to leave — it writes over `Discordia.exe`, which
    /// Windows refuses while we are the process running it. Reported from a
    /// real update: NSIS stopped on "Error opening file for writing" with
    /// Abort / Retry / Ignore, which reads like a corrupt download.
    ///
    /// The distinction is only visible in the type, so the type is what gets
    /// pinned: `Open` and `RunInstaller` do the same thing and differ in
    /// whether the caller may stay alive afterwards.
    #[test]
    #[cfg(windows)]
    fn the_windows_installer_is_not_the_stay_alive_case() {
        assert!(
            matches!(
                plan_install(std::path::PathBuf::from("setup.exe")),
                Install::RunInstaller(_)
            ),
            "a Windows install planned as Open would leave the app holding its \
             own exe while the installer tries to overwrite it"
        );
    }

    /// Off an AppImage and off a portable unzip — every developer machine —
    /// installing hands the file to the OS and replaces nothing in place.
    ///
    /// Which of the two hand-off variants it is depends on the platform, and
    /// that is what `the_windows_installer_is_not_the_stay_alive_case` pins.
    /// What matters here is that neither of them touches a live install.
    #[test]
    fn without_an_appimage_nothing_is_replaced() {
        assert!(
            running_appimage().is_none(),
            "APPIMAGE is set while running tests, which this test cannot interpret"
        );
        assert!(!matches!(
            plan_install(std::path::PathBuf::from("x")),
            Install::ReplaceAppImage { .. } | Install::ReplacePortable { .. }
        ));
    }

    /// Build a zip in `dir` from `(relative path, contents)` pairs.
    fn zip_of(dir: &std::path::Path, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        use std::io::Write;
        let path = dir.join("update.zip");
        let mut w = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for (name, body) in entries {
            w.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(body).unwrap();
        }
        w.finish().unwrap();
        path
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dxf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The happy path, and the two things about it that matter: the program is
    /// the new one, and the old one is still on disk under its parked name —
    /// because on Windows it is still open, and deleting it now would fail.
    #[test]
    fn a_portable_update_replaces_the_folder_and_parks_the_old_program() {
        let dir = scratch("portable-ok");
        std::fs::write(dir.join("Discordia.exe"), b"old program").unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("assets/font.woff2"), b"old font").unwrap();
        std::fs::write(dir.join("PORTABLE"), b"marker").unwrap();

        let zip = zip_of(
            &dir,
            &[
                ("Discordia.exe", b"new program"),
                ("assets/font.woff2", b"new font"),
                ("PORTABLE", b"marker"),
            ],
        );
        replace_portable(&zip, &dir, "Discordia.exe").unwrap();

        assert_eq!(
            std::fs::read(dir.join("Discordia.exe")).unwrap(),
            b"new program"
        );
        assert_eq!(
            std::fs::read(dir.join("assets/font.woff2")).unwrap(),
            b"new font"
        );
        assert_eq!(
            std::fs::read(dir.join("Discordia.exe.old")).unwrap(),
            b"old program",
            "the outgoing program must survive until the next start can delete it"
        );
        assert!(
            !dir.join(".update-staging").exists(),
            "staging was left behind"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// An archive missing the program is refused *before* anything is touched.
    /// Landing every file except the one you launch is worse than landing none.
    #[test]
    fn an_archive_without_the_program_changes_nothing() {
        let dir = scratch("portable-noexe");
        std::fs::write(dir.join("Discordia.exe"), b"old program").unwrap();
        std::fs::write(dir.join("keep.txt"), b"untouched").unwrap();

        let zip = zip_of(&dir, &[("assets/font.woff2", b"new font")]);
        let err = replace_portable(&zip, &dir, "Discordia.exe").unwrap_err();

        assert!(err.contains("no Discordia.exe"), "unhelpful: {err}");
        assert_eq!(
            std::fs::read(dir.join("Discordia.exe")).unwrap(),
            b"old program"
        );
        assert_eq!(std::fs::read(dir.join("keep.txt")).unwrap(), b"untouched");
        assert!(
            !dir.join("assets").exists(),
            "a refused update still unpacked"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A rename that fails **part way through the folder**, which is the case
    /// the first version of this got wrong: it stopped where it was and left
    /// some files new and some old.
    ///
    /// The failure is induced with a plain file sitting where the archive needs
    /// a directory: `create_dir_all` refuses that on every platform, so this is
    /// a real failure rather than a mocked one. It lands on the second entry,
    /// after the first has already been replaced.
    #[test]
    fn a_failure_part_way_through_puts_every_file_back() {
        let dir = scratch("portable-midway");
        std::fs::write(dir.join("Discordia.exe"), b"old program").unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("assets/a.woff2"), b"old a").unwrap();
        std::fs::write(dir.join("assets/b.woff2"), b"old b").unwrap();
        // `blocker` is a file, and the archive wants it to be a directory.
        std::fs::write(dir.join("blocker"), b"in the way").unwrap();

        let zip = zip_of(
            &dir,
            &[
                ("assets/a.woff2", b"new a"),
                ("blocker/inner.txt", b"cannot land"),
                ("assets/b.woff2", b"new b"),
                ("Discordia.exe", b"new program"),
            ],
        );
        let err = replace_portable(&zip, &dir, "Discordia.exe").unwrap_err();
        assert!(err.contains("blocker"), "unhelpful error: {err}");

        assert_eq!(
            std::fs::read(dir.join("assets/a.woff2")).unwrap(),
            b"old a",
            "a file replaced before the failure was not put back"
        );
        assert_eq!(std::fs::read(dir.join("assets/b.woff2")).unwrap(), b"old b");
        assert_eq!(
            std::fs::read(dir.join("Discordia.exe")).unwrap(),
            b"old program"
        );
        assert!(
            !dir.join("Discordia.exe.old").exists(),
            "the program was parked by an update that did not happen"
        );
        assert!(
            !dir.join(".update-staging").exists(),
            "a failed update left its staging directory behind"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Something that is not an archive at all — a proxy's error page saved
    /// under the asset's name — is refused without disturbing the install.
    #[test]
    fn a_download_that_is_not_a_zip_changes_nothing() {
        let dir = scratch("portable-notzip");
        std::fs::write(dir.join("Discordia.exe"), b"old program").unwrap();
        let fake = dir.join("update.zip");
        std::fs::write(&fake, b"<!doctype html>").unwrap();

        assert!(replace_portable(&fake, &dir, "Discordia.exe").is_err());
        assert_eq!(
            std::fs::read(dir.join("Discordia.exe")).unwrap(),
            b"old program"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A zip that points outside the folder is refused rather than sanitised.
    /// It is signed, so this should be impossible — which is the reason to
    /// check rather than the reason to skip.
    #[test]
    fn an_entry_escaping_the_folder_is_refused() {
        let dir = scratch("portable-escape");
        std::fs::write(dir.join("Discordia.exe"), b"old program").unwrap();
        let zip = zip_of(&dir, &[("../escaped.txt", b"nope")]);

        assert!(replace_portable(&zip, &dir, "Discordia.exe").is_err());
        assert!(!dir.parent().unwrap().join("escaped.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Off a portable unzip — which is every developer machine and every
    /// non-Windows platform — nothing claims to be one.
    #[test]
    fn a_checkout_is_not_a_portable_copy() {
        assert!(portable_dir().is_none());
    }

    /// The published portable archive, unpacked over a folder, for real.
    ///
    /// The other tests build their own zips, which proves the logic and proves
    /// nothing about the artifact. This one asks GitHub for the newest release,
    /// takes the file this platform would actually download, checks its
    /// signature against the compiled-in key, and unpacks it over a scratch
    /// copy of itself — the same three steps the update button performs, on the
    /// same bytes a user would get.
    ///
    /// It fails if CI renames an artifact, stops signing one, or changes the
    /// archive's shape, none of which any synthetic test can notice.
    ///
    /// ```text
    /// cargo test -p dioxusfun --bins -- --ignored the_published_portable
    /// ```
    #[test]
    #[ignore = "downloads the published portable archive"]
    fn the_published_portable_archive_unpacks_over_a_folder() {
        let rt = tokio::runtime::Runtime::new().expect("build a runtime");
        let (zip_bytes, sig) = rt.block_on(async {
            let http = reqwest::Client::new();
            let releases: serde_json::Value = http
                .get("https://api.github.com/repos/ssotomayor/discordia/releases")
                .header("User-Agent", "Discordia-test")
                .send()
                .await
                .expect("list releases")
                .json()
                .await
                .expect("decode releases");
            let newest = releases
                .as_array()
                .expect("an array of releases")
                .iter()
                .find(|r| r["draft"] != true)
                .expect("a published release");
            let url_of = |name: &str| {
                newest["assets"]
                    .as_array()
                    .expect("assets")
                    .iter()
                    .find(|a| a["name"] == name)
                    .unwrap_or_else(|| panic!("{name} is not in {}", newest["tag_name"]))
                    ["browser_download_url"]
                    .as_str()
                    .expect("a url")
                    .to_string()
            };
            let asset = "Discordia-windows-portable.zip";
            let body = http
                .get(url_of(asset))
                .send()
                .await
                .expect("download the archive")
                .bytes()
                .await
                .expect("read the archive");
            let sig = http
                .get(url_of(&signature_name(asset)))
                .send()
                .await
                .expect("download the signature")
                .text()
                .await
                .expect("read the signature");
            (body, sig)
        });

        verify(&zip_bytes, &sig).expect("the published archive must verify");

        // Stand up a folder shaped like an installed portable copy, by
        // unpacking the archive into it, then update it with the same archive.
        let dir = scratch("published-portable");
        let zip = dir.join("Discordia-windows-portable.zip");
        std::fs::write(&zip, &zip_bytes).unwrap();
        let installed = dir.join("app");
        std::fs::create_dir_all(&installed).unwrap();
        unzip_into(&zip, &installed).expect("unpack the archive once");
        // The published build predates the marker, so put it there by hand;
        // once a release carries one this line stops mattering.
        std::fs::write(installed.join(PORTABLE_MARKER), b"portable").unwrap();
        let before = std::fs::read(installed.join("Discordia.exe")).expect("an exe in the archive");

        replace_portable(&zip, &installed, "Discordia.exe").expect("update the folder");

        assert_eq!(
            std::fs::read(installed.join("Discordia.exe")).unwrap(),
            before,
            "the replaced program is not the one from the archive"
        );
        assert!(
            installed.join("Discordia.exe.old").is_file(),
            "the outgoing program was not parked"
        );
        assert!(
            installed.join("assets").is_dir(),
            "the assets did not survive"
        );
        assert!(
            !installed.join(".update-staging").exists(),
            "staging was left behind"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every artifact any platform's update could ask for is in the newest
    /// release, and every one of them is signed.
    ///
    /// Metadata only, so it covers all four platforms for the price of one API
    /// call — where the two download tests each cover one. It is the check that
    /// notices a renamed artifact or a signature that stopped being produced,
    /// which are silent failures for every user except the one platform whoever
    /// changed it happened to test.
    ///
    /// ```text
    /// cargo test -p dioxusfun --bins -- --ignored every_asset
    /// ```
    #[test]
    #[ignore = "asks GitHub for the newest release"]
    fn every_asset_the_update_can_ask_for_is_published_and_signed() {
        let wanted = [
            "Discordia-windows-setup.exe",
            "Discordia-windows-portable.zip",
            "Discordia-macos-arm64.dmg",
            "Discordia-linux-x86_64.AppImage",
        ];
        let rt = tokio::runtime::Runtime::new().expect("build a runtime");
        let (tag, names) = rt.block_on(async {
            let releases: serde_json::Value = reqwest::Client::new()
                .get("https://api.github.com/repos/ssotomayor/discordia/releases")
                .header("User-Agent", "Discordia-test")
                .send()
                .await
                .expect("list releases")
                .json()
                .await
                .expect("decode releases");
            let newest = releases
                .as_array()
                .expect("an array")
                .iter()
                .find(|r| r["draft"] != true)
                .expect("a published release")
                .clone();
            let names: Vec<String> = newest["assets"]
                .as_array()
                .expect("assets")
                .iter()
                .filter_map(|a| a["name"].as_str().map(str::to_string))
                .collect();
            (
                newest["tag_name"].as_str().unwrap_or("?").to_string(),
                names,
            )
        });

        for asset in wanted {
            assert!(
                names.iter().any(|n| n == asset),
                "{tag} publishes no {asset} — an update on that platform would find nothing"
            );
            let sig = signature_name(asset);
            assert!(
                names.contains(&sig),
                "{tag} has {asset} but no {sig} — unverifiable, so the update refuses it"
            );
        }
    }

    /// Every platform we publish for names a file that exists in the release,
    /// and every platform we do not publish for names nothing.
    #[test]
    fn the_asset_name_matches_what_ci_uploads() {
        let published = [
            "Discordia-windows-setup.exe",
            "Discordia-macos-arm64.dmg",
            "Discordia-linux-x86_64.AppImage",
            "Discordia-windows-portable.zip",
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
