use std::time::Duration;

use dioxus::prelude::*;
use minisign_verify::{PublicKey, Signature};

const PUBLIC_KEY: &str = include_str!("../../release-signing.pub");

const PORTABLE_MARKER: &str = "PORTABLE";

fn portable_dir() -> Option<std::path::PathBuf> {
    if !cfg!(target_os = "windows") {
        return None;
    }
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    dir.join(PORTABLE_MARKER).is_file().then_some(dir)
}

pub fn asset_name() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", _) if portable_dir().is_some() => Some("Discordia-windows-portable.zip"),
        ("windows", _) => Some("Discordia-windows-setup.exe"),
        ("macos", "aarch64") => Some("Discordia-macos-arm64.dmg"),
        ("linux", "x86_64") => Some("Discordia-linux-x86_64.AppImage"),
        _ => None,
    }
}

pub fn signature_name(asset: &str) -> String {
    format!("{asset}.minisig")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    BadPublicKey(String),
    BadSignature(String),
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

pub fn verify(data: &[u8], sig: &str) -> Result<(), VerifyError> {
    let key = PublicKey::from_base64(trim_key(PUBLIC_KEY))
        .map_err(|e| VerifyError::BadPublicKey(e.to_string()))?;
    let signature = Signature::decode(sig).map_err(|e| VerifyError::BadSignature(e.to_string()))?;
    key.verify(data, &signature, false)
        .map_err(|e| VerifyError::Mismatch(e.to_string()))
}

fn trim_key(file: &str) -> &str {
    file.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .unwrap_or("")
}

fn staging_path(asset: &str) -> std::path::PathBuf {
    running_appimage()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .or_else(portable_dir)
        .map(|d| d.join(format!(".{asset}.partial")))
        .unwrap_or_else(|| std::env::temp_dir().join(asset))
}

fn running_appimage() -> Option<std::path::PathBuf> {
    let p = std::path::PathBuf::from(std::env::var_os("APPIMAGE")?);
    p.is_file().then_some(p)
}

pub async fn fetch_verified(
    asset: &str,
    asset_url: &str,
    signature_url: &str,
    mut on_progress: impl FnMut(u64, Option<u64>),
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
    let mut resp = client
        .get(asset_url)
        .send()
        .await
        .map_err(|e| format!("could not download the update: {e}"))?;

    // Streamed rather than `.bytes()` so the dialog has a number to draw. A
    // truncated body needs no length check: the signature will not match.
    let total = resp.content_length();
    let mut body: Vec<u8> = Vec::with_capacity(total.unwrap_or(0).min(1 << 28) as usize);
    on_progress(0, total);
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("the download was cut short: {e}"))?
    {
        body.extend_from_slice(&chunk);
        on_progress(body.len() as u64, total);
    }

    verify(&body, &sig).map_err(|e| e.to_string())?;

    let path = staging_path(asset);
    std::fs::write(&path, &body).map_err(|e| format!("could not save the update: {e}"))?;
    Ok(path)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    ReplaceAppImage {
        target: std::path::PathBuf,
        staged: std::path::PathBuf,
    },
    ReplacePortable {
        dir: std::path::PathBuf,
        zip: std::path::PathBuf,
    },
    RunInstaller(std::path::PathBuf),
    Open(std::path::PathBuf),
}

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

const OUTGOING: &str = ".old";

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

    if !staging.join(exe).is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(format!("the update archive contains no {exe}"));
    }

    let backups = staging.join(".replaced");
    let mut done: Vec<Undo> = Vec::new();

    let outcome = (|| {
        for rel in unpacked.iter().filter(|r| r.as_os_str() != exe) {
            place(&staging, &backups, dir, rel, &mut done)?;
        }
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

struct Undo {
    placed: std::path::PathBuf,
    restore_from: Option<std::path::PathBuf>,
}

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

pub fn sweep_outgoing() {
    let Some(path) = outgoing_path() else { return };
    std::thread::spawn(move || {
        for wait in [0, 1, 2, 5, 10] {
            if wait > 0 {
                std::thread::sleep(Duration::from_secs(wait));
            }
            if !path.exists() || std::fs::remove_file(&path).is_ok() {
                return;
            }
        }
    });
}

/// Beside the running program, whichever way it was installed: both the
/// portable swap and the Windows installer leave their cast-off here.
fn outgoing_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let name = exe.file_name()?.to_str()?;
    Some(exe.with_file_name(format!("{name}{OUTGOING}")))
}

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

/// What the caller has to do next, which is not the same in every case.
pub enum Applied {
    /// The new build is on disk and already starting; quit.
    Restarted,
    /// A visible installer has the job and needs this process gone to overwrite
    /// its files; quit.
    HandedOff,
    /// Opened for someone to finish by hand; stay.
    Opened,
}

pub fn perform(install: &Install) -> Result<Applied, String> {
    match install {
        Install::ReplaceAppImage { target, staged } => {
            swap(staged, target)?;
            std::process::Command::new(target)
                .spawn()
                .map(|_| Applied::Restarted)
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
                .map(|_| Applied::Restarted)
                .map_err(|e| format!("installed, but could not restart: {e}"))
        }
        Install::RunInstaller(path) => run_installer(path),
        Install::Open(path) => open_installer(path).map(|()| Applied::Opened),
    }
}

/// Applies a Windows setup without showing it.
///
/// The generated NSIS script is a first-run installer — it names its section
/// "Install", asks for a directory and knows nothing about an existing copy —
/// so showing it is what made an update look like a reinstall. `/S` is NSIS's
/// own silent switch, and the bundle installs per user, so nothing prompts.
///
/// Falls back to the visible installer rather than leaving someone stuck: the
/// wizard is worse, but it is not nothing.
#[cfg(target_os = "windows")]
fn run_installer(path: &std::path::Path) -> Result<Applied, String> {
    use std::os::windows::process::CommandExt;

    let exe =
        std::env::current_exe().map_err(|e| format!("could not find the running program: {e}"))?;
    let dir = exe
        .parent()
        .ok_or("the running program has no directory")?
        .to_path_buf();

    let aside = move_aside(&exe)?;
    let mut cmd = std::process::Command::new(path);
    cmd.arg("/S");
    // NSIS wants `/D` last, unquoted, and taking the rest of the line as-is —
    // `arg` would quote a path with spaces and the switch would be read wrong.
    cmd.raw_arg(format!("/D={}", dir.display()));

    match cmd.status() {
        Ok(status) if status.success() => {
            let _ = std::fs::remove_file(path);
            std::process::Command::new(&exe)
                .current_dir(&dir)
                .spawn()
                .map(|_| Applied::Restarted)
                .map_err(|e| format!("installed, but could not restart: {e}"))
        }
        other => {
            restore_aside(&exe, &aside);
            let why = match other {
                Ok(status) => format!("the installer stopped with {status}"),
                Err(e) => format!("the installer would not start: {e}"),
            };
            eprintln!("[update] silent install failed: {why}; showing the installer");
            open_installer(path).map(|()| Applied::HandedOff)
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn run_installer(path: &std::path::Path) -> Result<Applied, String> {
    open_installer(path).map(|()| Applied::HandedOff)
}

/// Frees the running program's own name. Windows refuses to overwrite a running
/// executable but is happy to rename one, and the installer needs the name, not
/// the file.
#[cfg(target_os = "windows")]
fn move_aside(exe: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("the running program has no name")?;
    let aside = exe.with_file_name(format!("{name}{OUTGOING}"));
    let _ = std::fs::remove_file(&aside);
    std::fs::rename(exe, &aside)
        .map_err(|e| format!("could not move the running program aside: {e}"))?;
    Ok(aside)
}

/// Only if nothing has taken the name in the meantime: a setup that wrote the
/// new binary and then failed has left something newer than what we moved.
#[cfg(target_os = "windows")]
fn restore_aside(exe: &std::path::Path, aside: &std::path::Path) {
    if !exe.exists() {
        let _ = std::fs::rename(aside, exe);
    }
}

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

#[derive(Clone, PartialEq)]
enum Phase {
    Offered,
    Working,
    Applying,
    Restart,
    Closing,
    HandedOff,
    Failed(String),
}

#[component]
pub fn UpdateNotice(update: crate::version::Update) -> Element {
    let mut phase = use_signal(|| Phase::Offered);
    let mut progress = use_signal(|| (0u64, None::<u64>));

    let install = update.download.clone();
    let start = move |_| {
        let Some(d) = install.clone() else { return };
        progress.set((0, None));
        phase.set(Phase::Working);
        spawn(async move {
            let mut painted = 0u64;
            let tick = move |got: u64, total: Option<u64>| {
                if should_paint(got, painted, total) {
                    painted = got;
                    progress.set((got, total));
                }
            };
            match fetch_verified(&d.asset, &d.asset_url, &d.signature_url, tick).await {
                Err(e) => phase.set(Phase::Failed(e)),
                Ok(staged) => {
                    let plan = plan_install(staged);
                    // Said before the work starts, and on a blocking thread so
                    // it is actually painted: a silent installer shows nothing
                    // of its own, so this is the only sign anything is
                    // happening.
                    phase.set(Phase::Applying);
                    let done = tokio::task::spawn_blocking(move || perform(&plan)).await;
                    match done {
                        Err(e) => phase.set(Phase::Failed(format!("the update thread died: {e}"))),
                        Ok(Err(e)) => phase.set(Phase::Failed(e)),
                        Ok(Ok(Applied::Opened)) => phase.set(Phase::HandedOff),
                        Ok(Ok(outcome)) => {
                            phase.set(match outcome {
                                Applied::Restarted => Phase::Restart,
                                _ => Phase::Closing,
                            });
                            tokio::time::sleep(Duration::from_millis(900)).await;
                            std::process::exit(0);
                        }
                    }
                }
            }
        });
    };

    let tag = update.tag.clone();
    let offer_page = update.url.clone();
    let failed_page = update.url.clone();
    let has_download = update.download.is_some();

    let current = phase();
    let (got, total) = progress();
    let step = match &current {
        Phase::Applying => "Installing — the installer runs on its own.",
        Phase::Closing => "Closing so the installer can finish.",
        Phase::Restart => "Installed — restarting Discordia.",
        Phase::HandedOff => "Verified — finish in the installer window.",
        _ => "Downloading and checking the signature…",
    };
    let warn = matches!(current, Phase::Working | Phase::Applying | Phase::Closing);
    // Only the download has a real denominator: the installer is silent, so
    // past it the bar can say "alive", never "how much longer".
    let pct = match total {
        Some(t) if t > 0 && matches!(current, Phase::Working) => Some(got.min(t) * 100 / t),
        _ => None,
    };
    let (bar_class, bar_style) = match pct {
        Some(p) => (
            "h-full rounded-full",
            format!("width: {p}%; background: var(--accent);"),
        ),
        None => (
            "dxf-bar-slide h-full rounded-full",
            "width: 28%; background: var(--accent);".to_string(),
        ),
    };
    let caption = match (&current, total) {
        (Phase::Working, Some(t)) => format!("{} of {}", mib(got), mib(t)),
        (Phase::Working, None) => format!("{} downloaded", mib(got)),
        _ if got > 0 => format!("Signature verified · {}", mib(got)),
        _ => String::new(),
    };
    let pct_label = pct.map(|p| format!("{p}%")).unwrap_or_default();

    rsx! {
        match current {
            Phase::Offered if !has_download => rsx! {
                button {
                    class: "text-[10px] text-[var(--accent)] underline",
                    title: "Opens the release page in your browser",
                    onclick: move |_| crate::app::open_external(&offer_page),
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
            Phase::Failed(e) => rsx! {
                div { class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/70",
                    div {
                        class: "dxf-modal-in w-[22rem] bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                        div {
                            class: "px-4 py-3 border-b border-[var(--border)] text-sm font-medium text-[var(--danger)]",
                            "Update failed"
                        }
                        div { class: "p-4",
                            div { class: "text-xs leading-relaxed text-[var(--text-muted)]", "{e}" }
                            div { class: "mt-4 flex items-center justify-end gap-2",
                                button {
                                    class: "text-xs px-3 py-1.5 rounded-md border border-[var(--border-strong)] text-[var(--text)]",
                                    onclick: move |_| crate::app::open_external(&failed_page),
                                    "Open release page"
                                }
                                button {
                                    class: "text-xs px-3 py-1.5 rounded-md border border-[var(--accent)] text-[var(--accent)]",
                                    onclick: move |_| phase.set(Phase::Offered),
                                    "Close"
                                }
                            }
                        }
                    }
                }
            },
            // No dismiss on purpose: closing the window mid-swap is the one
            // thing this dialog exists to prevent.
            _ => rsx! {
                div { class: "dxf-backdrop-in fixed inset-0 z-50 flex items-center justify-center bg-black/70",
                    div {
                        class: "dxf-modal-in w-[22rem] bg-[var(--panel-solid)] border border-[var(--border)] rounded-lg shadow-xl overflow-hidden",
                        div {
                            class: "px-4 py-3 border-b border-[var(--border)] text-sm font-medium text-[var(--accent)]",
                            "Updating to {tag}"
                        }
                        div { class: "p-4",
                            div { class: "text-xs text-[var(--text)]", "{step}" }
                            div {
                                class: "mt-2.5 h-1.5 rounded-full overflow-hidden",
                                style: "background: var(--bg2);",
                                div { class: "{bar_class}", style: "{bar_style}" }
                            }
                            div {
                                class: "mt-2 flex items-center justify-between text-[10px] text-[var(--text-dim)]",
                                span { "{caption}" }
                                span { "{pct_label}" }
                            }
                            if warn {
                                div {
                                    class: "mt-3 rounded-md px-2.5 py-2 text-[10px] leading-relaxed text-[var(--warn)]",
                                    style: "background: var(--accent-soft);",
                                    "Do not close Discordia. It will restart itself when the update is done."
                                }
                            }
                        }
                    }
                }
            },
        }
    }
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

const PAINT_STEP: u64 = 256 * 1024;

/// A chunk lands every few KB and repainting on each one costs more than the
/// socket does. The last one always paints, or the bar stops short of full.
fn should_paint(got: u64, painted: u64, total: Option<u64>) -> bool {
    got == 0 || got.saturating_sub(painted) >= PAINT_STEP || Some(got) == total
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\n\
        RURC4u05WSE3oGJQqK+9DS0GcRx2CylPzBUsLnAj6CgqZx/ssJ4Xc6Ix0OQZZnVzRjKbvkiIqMguQDR9WpoSmK/FsAqSxCsesg8=\n\
        trusted comment: discordia v0.1.0-pre.332 Discordia-macos-arm64.dmg\n\
        EEn/Pl9c0ILDWdYWpVkHHRQ0ZEmwfAy/mqOS4KD31ZlRTYriAZhl4txFe3DDC4lm0gGp9tsVJYrlFFo1U3RrBg==\n";

    #[test]
    fn the_built_in_key_parses() {
        assert!(
            PublicKey::from_base64(trim_key(PUBLIC_KEY)).is_ok(),
            "release-signing.pub does not decode as a minisign public key"
        );
    }

    #[test]
    fn the_comment_line_is_not_part_of_the_key() {
        assert!(!trim_key(PUBLIC_KEY).contains("untrusted comment"));
        assert!(trim_key(PUBLIC_KEY).starts_with("RW"));
    }

    #[test]
    fn the_bar_paints_the_ends_and_skips_the_dribble() {
        assert!(should_paint(0, 0, Some(9_000)), "the start must paint");
        assert!(
            !should_paint(4_096, 0, Some(9_000_000)),
            "a 4K chunk must not"
        );
        assert!(should_paint(PAINT_STEP, 0, Some(9_000_000)));
        assert!(
            should_paint(9_000_001, 9_000_000, Some(9_000_001)),
            "the last chunk must paint however small, or the bar stops short"
        );
    }

    #[test]
    fn a_download_of_unknown_length_still_advances() {
        let mut painted = 0u64;
        let mut painted_count = 0;
        for got in (0..=(4 * PAINT_STEP)).step_by(64 * 1024) {
            if should_paint(got, painted, None) {
                painted = got;
                painted_count += 1;
            }
        }
        assert_eq!(
            painted_count, 5,
            "one paint at the start, then one per step"
        );
    }

    #[test]
    fn content_that_was_not_signed_is_refused() {
        let err = verify(b"not the dmg", REAL_SIGNATURE).unwrap_err();
        assert!(
            matches!(err, VerifyError::Mismatch(_)),
            "expected a mismatch, got {err:?}"
        );
    }

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

    #[test]
    #[ignore = "downloads a 56MB release artifact"]
    fn verifies_the_real_release_artifact() {
        let base = "https://github.com/ssotomayor/discordia/releases/download/v0.1.0-pre.332";
        let dmg = format!("{base}/Discordia-macos-arm64.dmg");
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

        let mut tampered = body.to_vec();
        tampered[0] ^= 0xff;
        assert!(verify(&tampered, REAL_SIGNATURE).is_err());
    }

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

    #[test]
    #[cfg(windows)]
    fn a_file_still_held_open_is_swept_once_it_is_released() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = scratch("sweep-retry");
        let parked = dir.join("Discordia.exe.old");
        std::fs::write(&parked, b"outgoing").unwrap();

        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&parked)
            .unwrap();
        assert!(
            std::fs::remove_file(&parked).is_err(),
            "the lock did not take, so this test proves nothing"
        );

        let p = parked.clone();
        let sweeper = std::thread::spawn(move || {
            for wait in [0, 1, 2] {
                if wait > 0 {
                    std::thread::sleep(Duration::from_secs(wait));
                }
                if !p.exists() || std::fs::remove_file(&p).is_ok() {
                    return true;
                }
            }
            false
        });

        std::thread::sleep(Duration::from_millis(300));
        drop(held);

        assert!(sweeper.join().unwrap(), "the retry never got the file");
        assert!(!parked.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

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

    /// It used to need a `PORTABLE` marker to answer at all, so an installed
    /// build never swept the copy its own update left behind.
    #[test]
    fn the_cast_off_is_looked_for_beside_the_running_program() {
        let exe = std::env::current_exe().expect("a running test has a path");
        let path = outgoing_path().expect("an installed build has one too");
        assert_eq!(path.parent(), exe.parent());
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            format!("{}{OUTGOING}", exe.file_name().unwrap().to_str().unwrap())
        );
    }

    /// The installer cannot write over a running program but the name can be
    /// freed, which is the whole reason a silent update works at all.
    #[cfg(target_os = "windows")]
    #[test]
    fn moving_aside_frees_the_name_and_restoring_gives_it_back() {
        let dir = scratch("aside");
        let exe = dir.join("Discordia.exe");
        std::fs::write(&exe, b"running").unwrap();

        let aside = move_aside(&exe).expect("rename");
        assert!(!exe.exists(), "the name was not freed");
        assert_eq!(std::fs::read(&aside).unwrap(), b"running");

        restore_aside(&exe, &aside);
        assert_eq!(std::fs::read(&exe).unwrap(), b"running");
        assert!(!aside.exists());
    }

    /// A setup that wrote the new binary and then failed leaves something newer
    /// than what we moved, and rolling back over it would undo the update.
    #[cfg(target_os = "windows")]
    #[test]
    fn restoring_never_clobbers_a_binary_that_took_the_name() {
        let dir = scratch("aside-taken");
        let exe = dir.join("Discordia.exe");
        std::fs::write(&exe, b"old").unwrap();

        let aside = move_aside(&exe).expect("rename");
        std::fs::write(&exe, b"new").unwrap();
        restore_aside(&exe, &aside);

        assert_eq!(std::fs::read(&exe).unwrap(), b"new");
        assert!(aside.exists(), "the old copy is the sweeper's to remove");
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("dxf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

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

    #[test]
    fn a_failure_part_way_through_puts_every_file_back() {
        let dir = scratch("portable-midway");
        std::fs::write(dir.join("Discordia.exe"), b"old program").unwrap();
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("assets/a.woff2"), b"old a").unwrap();
        std::fs::write(dir.join("assets/b.woff2"), b"old b").unwrap();
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

    #[test]
    fn an_entry_escaping_the_folder_is_refused() {
        let dir = scratch("portable-escape");
        std::fs::write(dir.join("Discordia.exe"), b"old program").unwrap();
        let zip = zip_of(&dir, &[("../escaped.txt", b"nope")]);

        assert!(replace_portable(&zip, &dir, "Discordia.exe").is_err());
        assert!(!dir.parent().unwrap().join("escaped.txt").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_checkout_is_not_a_portable_copy() {
        assert!(portable_dir().is_none());
    }

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

        let dir = scratch("published-portable");
        let zip = dir.join("Discordia-windows-portable.zip");
        std::fs::write(&zip, &zip_bytes).unwrap();
        let installed = dir.join("app");
        std::fs::create_dir_all(&installed).unwrap();
        unzip_into(&zip, &installed).expect("unpack the archive once");
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
