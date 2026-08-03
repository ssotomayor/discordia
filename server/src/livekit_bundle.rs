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

#[cfg(target_os = "windows")]
const LIVEKIT_BIN_NAME: &str = "livekit-server.exe";
#[cfg(not(target_os = "windows"))]
const LIVEKIT_BIN_NAME: &str = "livekit-server";

pub const DEFAULT_LIVEKIT_PORT: u16 = 7880;
pub const DEFAULT_LIVEKIT_KEY: &str = "devkey";
pub const DEFAULT_LIVEKIT_SECRET: &str = "secret-must-be-at-least-32-chars-long";

/// True when the build successfully bundled a `livekit-server` binary.
pub fn is_bundled() -> bool {
    !LIVEKIT_BIN.is_empty()
}

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
    let path: PathBuf = dir.join(LIVEKIT_BIN_NAME);

    let needs_write = match fs::metadata(&path) {
        Ok(m) => m.len() as usize != LIVEKIT_BIN.len(),
        Err(_) => true,
    };
    if needs_write {
        fs::write(&path, LIVEKIT_BIN).map_err(|e| format!("write livekit binary: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&path).map_err(|e| e.to_string())?.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms).map_err(|e| e.to_string())?;
        }
    }

    let config_path = dir.join("livekit.yaml");
    let config = format!(
        "port: {DEFAULT_LIVEKIT_PORT}\n\
         bind_addresses:\n  - 0.0.0.0\n\
         rtc:\n  tcp_port: 7881\n  port_range_start: 50000\n  port_range_end: 50100\n  use_external_ip: false\n\
         keys:\n  {DEFAULT_LIVEKIT_KEY}: {DEFAULT_LIVEKIT_SECRET}\n\
         logging:\n  level: info\n",
    );
    fs::write(&config_path, &config).map_err(|e| format!("write livekit config: {e}"))?;

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
