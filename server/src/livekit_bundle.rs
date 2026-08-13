//! Bundled LiveKit SFU subprocess.
//!
//! The build script downloads (or builds, on macOS) a matching
//! `livekit-server` binary into `$OUT_DIR/livekit-server` and we embed it
//! here via `include_bytes!`. Anyone linking this crate — the standalone
//! `dioxusfun-server` binary and the Dioxus client's self-host code — gets
//! the same baked-in copy.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use tokio::process::{Child, Command};

// The embedded bytes must match the filename `build.rs` wrote, which carries
// the `.exe` suffix on Windows. (These two were out of sync and broke the
// Windows build with "couldn't read .../out/livekit-server".)
#[cfg(target_os = "windows")]
const LIVEKIT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/livekit-server.exe"));
#[cfg(not(target_os = "windows"))]
const LIVEKIT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/livekit-server"));

/// Short content digest of `LIVEKIT_BIN`, computed by `build.rs`.
///
/// Computed there and not here because it hashes a compile-time constant: the
/// answer is fixed before the program starts, and paying for it at runtime
/// meant paying every time someone opened self-host. Unoptimised, SHA-256 over
/// the 49MB Windows binary takes ~650ms against 19ms optimised — and `cargo
/// run` / `dx serve` are unoptimised by definition, so that was the greater
/// part of a one-second freeze on the client's UI thread. See `build.rs`.
const LIVEKIT_DIGEST: &str = include_str!(concat!(env!("OUT_DIR"), "/livekit-server.sha"));

#[cfg(target_os = "windows")]
const LIVEKIT_BIN_NAME: &str = "livekit-server.exe";
#[cfg(not(target_os = "windows"))]
const LIVEKIT_BIN_NAME: &str = "livekit-server";

pub const DEFAULT_LIVEKIT_PORT: u16 = 7880;
pub const DEFAULT_LIVEKIT_KEY: &str = "devkey";
pub const DEFAULT_LIVEKIT_SECRET: &str = "secret-must-be-at-least-32-chars-long";

/// Handle to a running LiveKit subprocess. Dropping it kills the process
/// (tokio's `kill_on_drop`).
pub struct LivekitSubprocess {
    _child: Child,
}

/// Write the bundled `livekit-server` binary + config to a temp dir and
/// spawn it as a child process. Blocks until LiveKit is accepting TCP
/// connections on its WebSocket port.
pub async fn spawn_livekit() -> Result<LivekitSubprocess, String> {
    if LIVEKIT_BIN.is_empty() {
        return Err("livekit-server binary not bundled in this build".into());
    }

    let dir = std::env::temp_dir().join("dioxusfun");
    fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;

    // The filename carries a hash of the bytes, so a different build never
    // lands on the same path. That is not tidiness — it is the whole fix for a
    // bug that made self-hosting impossible on macOS.
    //
    // This used to be one fixed path, rewritten in place whenever the embedded
    // bytes changed length. macOS caches a code signature against the
    // (device, inode) pair, so overwriting the file while an older copy is still
    // running invalidates that cache: every later `exec` of the path dies with
    // `SIGKILL (Code Signature Invalid)` — 16 crash reports on one machine — even
    // though `codesign --verify` on the file reports it perfectly valid, because
    // verification reads the bytes and the kernel is comparing against its cache.
    // Byte-identical content at a fresh path runs; at the poisoned path it is
    // killed. Never reusing the name is what avoids the whole class.
    //
    // The old check compared *length* alone, so a rebuilt SFU of the same size
    // was never re-extracted and stale bytes ran silently. A content hash is the
    // check that was meant.
    let digest = LIVEKIT_DIGEST.trim();
    let (stem, ext) = match LIVEKIT_BIN_NAME.split_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (LIVEKIT_BIN_NAME, String::new()),
    };
    let path: PathBuf = dir.join(format!("{stem}-{digest}{ext}"));

    let config_path = dir.join("livekit.yaml");
    let config = format!(
        "port: {DEFAULT_LIVEKIT_PORT}\n\
         bind_addresses:\n  - 0.0.0.0\n\
         rtc:\n  tcp_port: 7881\n  port_range_start: 50000\n  port_range_end: 50100\n  use_external_ip: false\n\
         keys:\n  {DEFAULT_LIVEKIT_KEY}: {DEFAULT_LIVEKIT_SECRET}\n\
         logging:\n  level: info\n",
    );

    // Every file this needs to touch, on a thread that is allowed to wait. The
    // caller on the client side is the UI's own executor: a 49MB write there
    // stops the window redrawing, and the sweep below is a directory listing
    // plus a delete per stale copy on a spinning disk's worst day.
    {
        let (dir, path, config_path) = (dir.clone(), path.clone(), config_path.clone());
        let stem = stem.to_string();
        let digest = digest.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            // Sweep the copies other builds left behind. Without this the hashed
            // names accumulate one 46MB binary per build in a directory nobody
            // looks at. A copy that is still running cannot be removed on
            // Windows and need not be on Unix — either way the error is ignored,
            // because a leftover file is not a reason to refuse to start.
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if name.starts_with(&stem)
                        && !name.ends_with(&digest)
                        && name != LIVEKIT_BIN_NAME
                    {
                        let _ = fs::remove_file(entry.path());
                    }
                    // The pre-hash name from older builds, which is the poisoned
                    // one.
                    if name == LIVEKIT_BIN_NAME {
                        let _ = fs::remove_file(entry.path());
                    }
                }
            }

            if !path.exists() {
                fs::write(&path, LIVEKIT_BIN).map_err(|e| format!("write livekit binary: {e}"))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let mut perms = fs::metadata(&path)
                        .map_err(|e| e.to_string())?
                        .permissions();
                    perms.set_mode(0o755);
                    fs::set_permissions(&path, perms).map_err(|e| e.to_string())?;
                }
            }

            fs::write(&config_path, &config).map_err(|e| format!("write livekit config: {e}"))
        })
        .await
        .map_err(|e| format!("livekit extraction task: {e}"))??;
    }

    let child = Command::new(&path)
        .arg("--config")
        .arg(&config_path)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn livekit: {e}"))?;

    wait_for_port(DEFAULT_LIVEKIT_PORT, Duration::from_secs(10))
        .await
        .map_err(|e| format!("livekit not ready: {e}"))?;

    Ok(LivekitSubprocess { _child: child })
}

async fn wait_for_port(port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let addr = format!("127.0.0.1:{port}");
    loop {
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("{addr} not listening after {timeout:?}"));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}
