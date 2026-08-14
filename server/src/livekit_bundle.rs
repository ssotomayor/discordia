//! Bundled LiveKit SFU subprocess.
//!
//! The build script downloads (or builds, on macOS) a matching
//! `livekit-server` binary into `$OUT_DIR/livekit-server` and we embed it
//! here via `include_bytes!`. Anyone linking this crate — the standalone
//! `dioxusfun-server` binary and the Dioxus client's self-host code — gets
//! the same baked-in copy.

use std::fs;
use std::path::{Path, PathBuf};
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
    /// Where the record of this child lives, so dropping cleanly takes it away.
    pid_file: PathBuf,
}

impl Drop for LivekitSubprocess {
    fn drop(&mut self) {
        // Best effort, and the failure to care about is the *other* direction:
        // a kill that skips this leaves the file behind on purpose, which is
        // exactly what the next run reads.
        let _ = fs::remove_file(&self.pid_file);
    }
}

/// Record of the SFU we started, read by the next run.
///
/// `kill_on_drop` is a destructor, so any kill that skips unwinding — force
/// quit, `taskkill /F`, a debugger, `SIGKILL` — leaves `livekit-server` running
/// and holding port 7880, reparented to launchd or to the Windows session. One
/// was found alive more than a day after its parent died.
///
/// The next session's symptom is not "port busy", which is what makes it worth
/// a file on disk. Our new child fails to bind and exits; `wait_for_ready` used
/// to connect to the **orphan** and report success, so self-host came up
/// talking to a process nobody chose and nothing could stop.
const PID_FILE: &str = "livekit-server.pid";

/// Kill the SFU a previous run left behind, if the recorded pid is still a live
/// process running the binary we recorded.
///
/// **Both halves of that check are load-bearing.** A pid on its own is not
/// evidence — operating systems recycle them, and a stale file naming a pid that
/// now belongs to something else would have us kill an innocent process.
/// Matching the image name closes that, and it is a strong match rather than a
/// coincidence-prone one because the name carries the build's content digest
/// (`livekit-server-<sha>`), which nothing else on the machine will be called.
///
/// Blocking: it shells out. Called from the `spawn_blocking` below for the same
/// reason the file work is.
fn reclaim_orphan(dir: &Path) {
    let path = dir.join(PID_FILE);
    let Ok(contents) = fs::read_to_string(&path) else {
        return;
    };
    let mut lines = contents.lines();
    let pid = lines.next().and_then(|l| l.trim().parse::<u32>().ok());
    let image = lines.next().map(str::trim).filter(|s| !s.is_empty());
    if let (Some(pid), Some(image)) = (pid, image)
        && process_matches(pid, image)
    {
        tracing::warn!(
            pid,
            image,
            "killing a livekit-server left by a previous run"
        );
        kill_pid(pid);
    }
    let _ = fs::remove_file(&path);
}

// The two platform pairs below shell out rather than take a dependency. There
// is precedent (`client::webview2`, `client::app`), and the alternative is a
// process-enumeration crate for four commands.
//
// One forward note: if the client ever gains `windows_subsystem = "windows"`
// (see TODO.md), these spawns need `CREATE_NO_WINDOW` alongside the SFU's own,
// or each one flashes a console.

/// Whether `pid` is live *and* running `image`.
#[cfg(windows)]
fn process_matches(pid: u32, image: &str) -> bool {
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output();
    // A filter that matches nothing still exits 0, printing "INFO: No tasks are
    // running which match the specified criteria." — so the image name is the
    // test, not the status.
    matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).contains(image))
}

/// Whether `pid` is live *and* running `image`.
#[cfg(not(windows))]
fn process_matches(pid: u32, image: &str) -> bool {
    // `args=` rather than `comm=`: Linux truncates `comm` to 15 characters and
    // `livekit-server-` is already 15, so every build would look alike — which
    // would defeat the digest match that makes this safe.
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output();
    matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).contains(image))
}

#[cfg(windows)]
fn kill_pid(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output();
}

#[cfg(not(windows))]
fn kill_pid(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .output();
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

    // Everything that has to happen on a thread allowed to wait: reclaiming the
    // port from a previous run, and every file this needs to touch. The caller
    // on the client side is the UI's own executor — a 49MB write there stops the
    // window redrawing, the sweep is a directory listing plus a delete per stale
    // copy on a spinning disk's worst day, and `reclaim_orphan` shells out.
    {
        let (dir, path, config_path) = (dir.clone(), path.clone(), config_path.clone());
        let stem = stem.to_string();
        let digest = digest.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            // Before anything touches the port: take back the one we orphaned.
            reclaim_orphan(&dir);

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

    let mut child = Command::new(&path)
        .arg("--config")
        .arg(&config_path)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("spawn livekit: {e}"))?;

    // Recorded before the wait, not after: if we are killed *during* the wait,
    // the child is already running and the next run still has to find it.
    if let Some(pid) = child.id() {
        let image = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = fs::write(dir.join(PID_FILE), format!("{pid}\n{image}\n"));
    }

    wait_for_ready(&mut child, DEFAULT_LIVEKIT_PORT, Duration::from_secs(10))
        .await
        .map_err(|e| format!("livekit not ready: {e}"))?;

    Ok(LivekitSubprocess {
        _child: child,
        pid_file: dir.join(PID_FILE),
    })
}

/// Wait until the SFU is accepting connections, failing fast if the child dies.
///
/// Watching the child is not belt-and-braces — it is the difference between a
/// named failure and a silent wrong answer. A port already held by something
/// else used to produce the *success* path: our child exits at once because it
/// cannot bind, and the connect then succeeds against whatever is already
/// listening. Self-host came up talking to a process we did not start and could
/// not stop.
///
/// The child is checked before the socket so that an immediate exit is reported
/// as itself rather than as a timeout.
async fn wait_for_ready(child: &mut Child, port: u16, timeout: Duration) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let addr = format!("127.0.0.1:{port}");
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "livekit-server exited on startup ({status}); port {port} is most likely \
                 held by another process"
            ));
        }
        if tokio::net::TcpStream::connect(&addr).await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("{addr} not listening after {timeout:?}"));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard that stops this from killing a stranger, exercised against the
    /// one process we can always name: this one.
    ///
    /// It also proves the platform command actually runs, which is the part that
    /// cannot be reasoned about — `tasklist` and `ps` differ in flags, output
    /// shape and what they print when they match nothing.
    #[test]
    fn a_pid_matches_only_its_own_image() {
        let me = std::process::id();
        let exe = std::env::current_exe().expect("current exe");
        let image = exe
            .file_name()
            .expect("exe file name")
            .to_string_lossy()
            .into_owned();

        assert!(
            process_matches(me, &image),
            "this test binary ({image}, pid {me}) should match itself"
        );
        // The recycled-pid case: same live pid, a name it is not running.
        assert!(
            !process_matches(me, "livekit-server-0000000000000000"),
            "a live pid running something else must not match"
        );
    }

    /// A record that names a pid which is not running the SFU must be discarded
    /// without killing anything — and the file must go, so it cannot be
    /// re-examined every start for the life of the temp directory.
    #[test]
    fn a_stale_record_is_dropped_rather_than_acted_on() {
        let dir = std::env::temp_dir().join(format!("dioxusfun-pidtest-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(PID_FILE);

        // Our own pid, deliberately paired with an image we are not running:
        // the pid resolves, the match fails, nothing is killed.
        fs::write(
            &path,
            format!("{}\nlivekit-server-deadbeef\n", std::process::id()),
        )
        .expect("write pid file");
        reclaim_orphan(&dir);
        assert!(!path.exists(), "a consumed record must not survive");

        // Garbage is the same story without the lookup.
        fs::write(&path, "not-a-pid\n").expect("write pid file");
        reclaim_orphan(&dir);
        assert!(!path.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
