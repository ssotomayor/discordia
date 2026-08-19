//! Bundled LiveKit SFU subprocess.
//!
//! The build script downloads (or builds, on macOS) a matching
//! `livekit-server` binary into `$OUT_DIR/livekit-server` and we embed it
//! here via `include_bytes!`. Anyone linking this crate — the standalone
//! `dioxusfun-server` binary and the Dioxus client's self-host code — gets
//! the same baked-in copy.

use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::{Child, Command};

// Filename must match what `build.rs` writes (`.exe` on Windows); mismatch
// breaks the build.
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

/// Env var that moves the bundled SFU off its default ports.
///
/// It exists so two self-hosting instances can coexist on one machine. Before
/// it, the port, the generated config and the pid file were all fixed, so the
/// second instance's orphan reclaim killed the first one's SFU and took 7880 —
/// and that configuration was already broken beforehand, since a second SFU
/// could never have bound the same port anyway.
pub const LIVEKIT_PORT_ENV: &str = "DISCORDIA_LIVEKIT_PORT";

/// The three ports one SFU needs, moved as a block.
///
/// Derived from one number rather than configured separately: they have to stay
/// adjacent for a port-mapping caller to reason about them, and three env vars
/// would let them drift apart in ways only a packet capture would explain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivekitPorts {
    /// WebSocket / signalling.
    pub ws: u16,
    /// ICE/TCP fallback, for peers whose network blocks UDP.
    pub tcp: u16,
    /// The single UDP mux port.
    pub udp: u16,
}

impl LivekitPorts {
    fn from_base(base: u16) -> LivekitPorts {
        LivekitPorts {
            ws: base,
            tcp: base + 1,
            udp: base + 2,
        }
    }
}

/// Resolve the ports from a raw env value.
///
/// Takes the string rather than reading the environment, so it can be tested
/// without a global mutation every other test in the binary would see.
/// Anything unparseable, zero, or close enough to the top of the range that
/// `base + 2` would wrap falls back to the default: a self-host that silently
/// binds ports nobody asked for is worse than one that ignores a typo.
pub fn ports_from(raw: Option<&str>) -> LivekitPorts {
    let base = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|b| *b > 0 && *b <= u16::MAX - 2)
        .unwrap_or(DEFAULT_LIVEKIT_PORT);
    LivekitPorts::from_base(base)
}

/// The ports this process will use, read once.
pub fn ports() -> LivekitPorts {
    static PORTS: std::sync::OnceLock<LivekitPorts> = std::sync::OnceLock::new();
    *PORTS.get_or_init(|| ports_from(std::env::var(LIVEKIT_PORT_ENV).ok().as_deref()))
}

/// ICE/TCP fallback, for peers whose network blocks UDP.
pub const DEFAULT_LIVEKIT_TCP_PORT: u16 = 7881;
/// **One** UDP port for all media, rather than a range.
///
/// LiveKit's single-port mux replaces the 100-port range this used to write.
/// The range was never wrong, it was unmappable: asking a router for a hundred
/// forwards is not a thing to do, so a self-hosted SFU had no dialable media
/// path at all. One port is one mapping — which is what makes the port-mapping
/// work in `client::portmap` worth anything.
pub const DEFAULT_LIVEKIT_UDP_PORT: u16 = 7882;
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
        // Leaving the file on kill is intentional: the next run reads it.
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
///
/// **Named per port**, which is what lets two instances coexist. The reclaim
/// cannot tell "the recorded SFU is a leftover" from "it belongs to a sibling
/// that is still running" — both are a live process with our image name — so
/// the second instance used to kill the first one's SFU. Giving each port its
/// own record removes the question instead of answering it.
fn pid_file_name() -> String {
    format!("livekit-server-{}.pid", ports().ws)
}

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
    let path = dir.join(pid_file_name());
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

// Shell out to avoid a process-enumeration dependency for four commands. The
// two Windows ones carry `CREATE_NO_WINDOW`: the client is a windowed build, so
// without it each call flashes a console of its own.

/// The `CreateProcess` flag for "no console, ever".
///
/// Spelled out rather than pulled from the `windows` crate: it is one constant,
/// and this crate does not otherwise depend on it.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether `pid` is live *and* running `image`.
#[cfg(windows)]
fn process_matches(pid: u32, image: &str) -> bool {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    // Filter exit code is 0 even with no match, so check the image name in
    // stdout, not the status.
    matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).contains(image))
}

/// Whether `pid` is live *and* running `image`.
#[cfg(not(windows))]
fn process_matches(pid: u32, image: &str) -> bool {
    // Use `args=` not `comm=`: Linux truncates `comm` to 15 chars, making
    // `livekit-server-` indistinguishable from other builds.
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "args="])
        .output();
    matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).contains(image))
}

#[cfg(windows)]
fn kill_pid(pid: u32) {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .creation_flags(CREATE_NO_WINDOW)
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
///
/// `advertise_ip` is the address remote peers should be told to send media to —
/// the public side of a port mapping. Pass `None` when there is no such address
/// and LiveKit advertises the machine's own LAN address, which is what it has
/// always done.
///
/// **It is `node_ip` and not `use_external_ip: true` on purpose**, even though
/// the latter is the switch LiveKit's own docs name for this. Two reasons, both
/// read out of the vendored source rather than assumed:
///
///  * `use_external_ip` makes `livekit-server` resolve its address over STUN
///    *during config validation*, and a failure there is fatal — the process
///    exits instead of starting. A LAN party with no internet, or a network that
///    blocks STUN, would lose voice entirely rather than lose only the remote
///    half of it. We already know the external address: the router told us when
///    it granted the mapping, and that answer is better than a STUN reply.
///  * It costs up to three 5-second STUN attempts before the port opens, which
///    is longer than callers wait for it.
///
/// What both share is that LiveKit *replaces* the host ICE candidate with the
/// advertised address rather than adding to it (pion defaults host rewrites to
/// `AddressRewriteReplace`, and LiveKit installs a catch-all rule). So once an
/// address is advertised, everyone — including this machine and anyone on its
/// LAN — reaches the SFU through it, which only works if the router hairpins.
/// That is why the caller probes for hairpinning before passing one in.
pub async fn spawn_livekit(advertise_ip: Option<IpAddr>) -> Result<LivekitSubprocess, String> {
    if LIVEKIT_BIN.is_empty() {
        return Err("livekit-server binary not bundled in this build".into());
    }

    let dir = std::env::temp_dir().join("dioxusfun");
    fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;

    // Hash in filename prevents macOS code-signature cache invalidation
    // (SIGKILL on exec) when overwriting a running binary. Also fixes stale-
    // byte bug where same-length rebuilds weren't re-extracted.
    let digest = LIVEKIT_DIGEST.trim();
    let (stem, ext) = match LIVEKIT_BIN_NAME.split_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (LIVEKIT_BIN_NAME, String::new()),
    };
    let path: PathBuf = dir.join(format!("{stem}-{digest}{ext}"));

    // Per-port naming prevents two instances from overwriting each other's
    // config.
    let config_path = dir.join(format!("livekit-{}.yaml", ports().ws));
    let config = config_yaml(advertise_ip);

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

    let mut command = Command::new(&path);
    command
        .arg("--config")
        .arg(&config_path)
        .kill_on_drop(true)
        // Piped rather than inherited, and forwarded below. A windowed client
        // has no console behind those handles, so inheriting them sends the
        // SFU's account of its own startup nowhere — and that account is
        // exactly what is wanted when self-host does not come up.
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Console-subsystem child of a windowed parent: Windows would hand it a
    // console of its own, which is a terminal full of SFU logs opening beside
    // the app the moment anyone self-hosts.
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|e| format!("spawn livekit: {e}"))?;

    forward_output(&mut child);

    // Recorded before the wait, not after: if we are killed *during* the wait,
    // the child is already running and the next run still has to find it.
    if let Some(pid) = child.id() {
        let image = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = fs::write(dir.join(pid_file_name()), format!("{pid}\n{image}\n"));
    }

    wait_for_ready(&mut child, ports().ws, Duration::from_secs(10))
        .await
        .map_err(|e| format!("livekit not ready: {e}"))?;

    Ok(LivekitSubprocess {
        _child: child,
        pid_file: dir.join(pid_file_name()),
    })
}

/// Relay the child's output into our own log, a line at a time.
///
/// Both streams at `info` and with the child's own level left in the text:
/// livekit prefixes every line with its level, and re-levelling stderr to
/// `warn` would relabel its routine startup chatter as a problem.
fn forward_output(child: &mut Child) {
    fn forward<R>(reader: R)
    where
        R: tokio::io::AsyncRead + Unpin + Send + 'static,
    {
        use tokio::io::AsyncBufReadExt;
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::info!("livekit: {line}");
            }
        });
    }

    if let Some(out) = child.stdout.take() {
        forward(out);
    }
    if let Some(err) = child.stderr.take() {
        forward(err);
    }
}

/// The config we hand `livekit-server`, kept out of `spawn_livekit` so its
/// shape can be asserted without a subprocess.
fn config_yaml(advertise_ip: Option<IpAddr>) -> String {
    config_yaml_for(advertise_ip, ports())
}

fn config_yaml_for(advertise_ip: Option<IpAddr>, p: LivekitPorts) -> String {
    let node_ip = advertise_ip
        .map(|ip| format!("  node_ip: {ip}\n"))
        .unwrap_or_default();
    let (ws, tcp, udp) = (p.ws, p.tcp, p.udp);
    format!(
        "port: {ws}\n\
         bind_addresses:\n  - 0.0.0.0\n\
         rtc:\n  tcp_port: {tcp}\n  udp_port: {udp}\n  use_external_ip: false\n{node_ip}\
         keys:\n  {DEFAULT_LIVEKIT_KEY}: {DEFAULT_LIVEKIT_SECRET}\n\
         logging:\n  level: info\n",
    )
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

    /// One UDP port and no range, because a range is unmappable — and no
    /// `port_range_*` left behind, which LiveKit would prefer over the mux if
    /// both were present.
    /// The env var moves all three ports together, and a bad value is ignored
    /// rather than obeyed — binding ports nobody asked for is worse than
    /// ignoring a typo.
    #[test]
    fn the_port_override_moves_the_block_or_is_ignored() {
        let d = LivekitPorts {
            ws: DEFAULT_LIVEKIT_PORT,
            tcp: DEFAULT_LIVEKIT_TCP_PORT,
            udp: DEFAULT_LIVEKIT_UDP_PORT,
        };
        assert_eq!(ports_from(None), d, "unset is the default");
        assert_eq!(ports_from(Some("  ")), d, "blank is the default");
        assert_eq!(ports_from(Some("hello")), d, "unparseable is the default");
        assert_eq!(ports_from(Some("0")), d, "zero is not a port");
        // `base + 2` must not wrap: 65534 would give a udp port of 0.
        assert_eq!(ports_from(Some("65534")), d, "too close to the top");

        let moved = ports_from(Some("7890"));
        assert_eq!(moved.ws, 7890);
        assert_eq!(moved.tcp, 7891, "the block stays adjacent");
        assert_eq!(moved.udp, 7892);
        assert!(
            config_yaml_for(None, moved).contains("port: 7890"),
            "the config the SFU reads has to agree with what we advertise"
        );
        assert!(config_yaml_for(None, moved).contains("udp_port: 7892"));
    }

    #[test]
    fn media_rides_a_single_udp_port() {
        let yaml = config_yaml(None);
        assert!(
            yaml.contains(&format!("udp_port: {DEFAULT_LIVEKIT_UDP_PORT}")),
            "{yaml}"
        );
        assert!(!yaml.contains("port_range"), "{yaml}");
    }

    /// Advertising is opt-in per spawn: without an address the config is the
    /// one every non-mapped host has always had, so a machine that could not
    /// map a port keeps LAN voice working exactly as before.
    #[test]
    fn node_ip_appears_only_when_advertising() {
        assert!(!config_yaml(None).contains("node_ip"));

        let yaml = config_yaml(Some("203.0.113.5".parse().unwrap()));
        assert!(yaml.contains("node_ip: 203.0.113.5"), "{yaml}");
        // Never both: `use_external_ip` would re-resolve over STUN at startup
        // and take the process down with it when that fails.
        assert!(yaml.contains("use_external_ip: false"), "{yaml}");
    }

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
        let path = dir.join(pid_file_name());

        // Our own pid, deliberately paired with an image we are not running:
        // the pid resolves, the match fails, nothing is killed.
        fs::write(
            &path,
            format!("{}\nlivekit-server-deadbeef\n", std::process::id()),
        )
        .expect("write pid file");
        reclaim_orphan(&dir);
        assert!(!path.exists(), "a consumed record must not survive");

        fs::write(&path, "not-a-pid\n").expect("write pid file");
        reclaim_orphan(&dir);
        assert!(!path.exists());

        let _ = fs::remove_dir_all(&dir);
    }
}
