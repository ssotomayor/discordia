use std::fs;
use std::io;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rand::RngCore;
use sha2::{Digest, Sha256};
use tokio::process::{Child, Command};

#[cfg(target_os = "windows")]
const LIVEKIT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/livekit-server.exe"));
#[cfg(not(target_os = "windows"))]
const LIVEKIT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/livekit-server"));

const LIVEKIT_DIGEST: &str = include_str!(concat!(env!("OUT_DIR"), "/livekit-server.sha"));

const LIVEKIT_BIN_STEM: &str = "livekit-server";

#[cfg(target_os = "windows")]
const LIVEKIT_BIN_EXT: &str = ".exe";
#[cfg(not(target_os = "windows"))]
const LIVEKIT_BIN_EXT: &str = "";

pub const DEFAULT_LIVEKIT_PORT: u16 = 7880;

pub const LIVEKIT_PORT_ENV: &str = "DISCORDIA_LIVEKIT_PORT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivekitPorts {
    pub ws: u16,
    pub tcp: u16,
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

pub fn ports_from(raw: Option<&str>) -> LivekitPorts {
    let base = raw
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|b| *b > 0 && *b <= u16::MAX - 2)
        .unwrap_or(DEFAULT_LIVEKIT_PORT);
    LivekitPorts::from_base(base)
}

pub fn ports() -> LivekitPorts {
    static PORTS: std::sync::OnceLock<LivekitPorts> = std::sync::OnceLock::new();
    *PORTS.get_or_init(|| ports_from(std::env::var(LIVEKIT_PORT_ENV).ok().as_deref()))
}

pub const DEFAULT_LIVEKIT_TCP_PORT: u16 = 7881;
pub const DEFAULT_LIVEKIT_UDP_PORT: u16 = 7882;

pub const KEYS_FILE: &str = "livekit-keys";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    pub key: String,
    pub secret: String,
}

impl Credentials {
    pub fn generate() -> Credentials {
        let mut bytes = [0u8; 36];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Credentials {
            key: format!("dxf{}", hex::encode(&bytes[..4])),
            secret: hex::encode(&bytes[4..]),
        }
    }

    fn parse(contents: &str) -> Option<Credentials> {
        let mut lines = contents.lines().map(str::trim);
        let key = lines.next().filter(|l| !l.is_empty())?;
        let secret = lines.next().filter(|l| !l.is_empty())?;
        Some(Credentials {
            key: key.into(),
            secret: secret.into(),
        })
    }
}

pub fn credentials(data_dir: &Path) -> io::Result<Credentials> {
    let path = data_dir.join(KEYS_FILE);
    if let Ok(contents) = fs::read_to_string(&path)
        && let Some(creds) = Credentials::parse(&contents)
    {
        return Ok(creds);
    }
    let creds = Credentials::generate();
    fs::create_dir_all(data_dir)?;
    let tmp = data_dir.join(format!("{KEYS_FILE}.tmp"));
    let _ = fs::remove_file(&tmp);
    write_private(&tmp, format!("{}\n{}\n", creds.key, creds.secret))?;
    fs::rename(&tmp, &path)?;
    Ok(creds)
}

// A pair that lives one run still lets voice work; a fixed literal would let anyone in.
pub fn credentials_or_ephemeral(data_dir: &Path) -> Credentials {
    credentials(data_dir).unwrap_or_else(|e| {
        tracing::error!(
            error = %e,
            path = %data_dir.join(KEYS_FILE).display(),
            "cannot persist livekit credentials; voice keys will change on restart"
        );
        Credentials::generate()
    })
}

#[cfg(unix)]
pub(crate) fn write_private(path: &Path, contents: String) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(contents.as_bytes())
}

#[cfg(not(unix))]
pub(crate) fn write_private(path: &Path, contents: String) -> io::Result<()> {
    fs::write(path, contents)
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if let Some(parent) = dir.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(e) = fs::DirBuilder::new().mode(0o700).create(dir)
        && !dir.is_dir()
    {
        return Err(e);
    }
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

fn write_executable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o755);
    }
    opts.open(path)?.write_all(bytes)
}

fn file_digest(path: &Path) -> io::Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    io::copy(&mut fs::File::open(path)?, &mut hasher)?;
    Ok(hasher.finalize().into())
}

// Whatever sits under the name gets executed, so the name alone is not proof of what it is.
fn ensure_binary(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let path = dir.join(name);
    let expected: [u8; 32] = Sha256::digest(bytes).into();
    if file_digest(&path).ok() == Some(expected) {
        return Ok(path);
    }
    let tmp = dir.join(format!("{name}.tmp"));
    let _ = fs::remove_file(&tmp);
    write_executable(&tmp, bytes)?;
    fs::rename(&tmp, &path)?;
    Ok(path)
}

// Each bundle upgrade lands under a new digest; the previous 50 MB would otherwise stay forever.
fn sweep_stale(dir: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(LIVEKIT_BIN_STEM) && name != keep && !name.ends_with(".pid") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

pub struct LivekitSubprocess {
    child: Child,
    pid_file: PathBuf,
}

impl Drop for LivekitSubprocess {
    fn drop(&mut self) {
        // Kill before forgetting the pid. `kill_on_drop` would do it a moment
        // later, but the record removed first is the one `reclaim_orphan`
        // needs if the kill never lands.
        let _ = self.child.start_kill();
        let _ = fs::remove_file(&self.pid_file);
    }
}

fn pid_file_name() -> String {
    format!("{LIVEKIT_BIN_STEM}-{}.pid", ports().ws)
}

fn parse_pid_record(contents: &str) -> Option<(u32, &str)> {
    let mut lines = contents.lines();
    let pid = lines.next()?.trim().parse::<u32>().ok()?;
    let image = lines.next()?.trim();
    image
        .strip_prefix(LIVEKIT_BIN_STEM)
        .is_some_and(|rest| rest.starts_with('-'))
        .then_some((pid, image))
}

fn reclaim_orphan(dir: &Path) {
    let path = dir.join(pid_file_name());
    let Ok(contents) = fs::read_to_string(&path) else {
        return;
    };
    if let Some((pid, image)) = parse_pid_record(&contents)
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

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn process_matches(pid: u32, image: &str) -> bool {
    use std::os::windows::process::CommandExt;
    let out = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    matches!(out, Ok(o) if String::from_utf8_lossy(&o.stdout).contains(image))
}

#[cfg(not(windows))]
fn process_matches(pid: u32, image: &str) -> bool {
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

pub async fn spawn_livekit(
    advertise_ip: Option<IpAddr>,
    creds: &Credentials,
    data_dir: &Path,
) -> Result<LivekitSubprocess, String> {
    if LIVEKIT_BIN.is_empty() {
        return Err("livekit-server binary not bundled in this build".into());
    }

    let dir = data_dir.join("livekit");
    let bin_name = format!(
        "{LIVEKIT_BIN_STEM}-{}{LIVEKIT_BIN_EXT}",
        LIVEKIT_DIGEST.trim()
    );
    let config_path = dir.join(format!("livekit-{}.yaml", ports().ws));
    let config = config_yaml(advertise_ip, creds);

    let path = {
        let (dir, bin_name, config_path) = (dir.clone(), bin_name.clone(), config_path.clone());
        tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
            private_dir(&dir).map_err(|e| format!("livekit dir {}: {e}", dir.display()))?;
            reclaim_orphan(&dir);
            sweep_stale(&dir, &bin_name);
            let path = ensure_binary(&dir, &bin_name, LIVEKIT_BIN)
                .map_err(|e| format!("write livekit binary: {e}"))?;
            let _ = fs::remove_file(&config_path);
            write_private(&config_path, config)
                .map_err(|e| format!("write livekit config: {e}"))?;
            Ok(path)
        })
        .await
        .map_err(|e| format!("livekit extraction task: {e}"))??
    };

    let mut command = Command::new(&path);
    command
        .arg("--config")
        .arg(&config_path)
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|e| format!("spawn livekit: {e}"))?;

    forward_output(&mut child);

    if let Some(pid) = child.id() {
        let _ = fs::write(dir.join(pid_file_name()), format!("{pid}\n{bin_name}\n"));
    }

    wait_for_ready(&mut child, ports().ws, Duration::from_secs(10))
        .await
        .map_err(|e| format!("livekit not ready: {e}"))?;

    Ok(LivekitSubprocess {
        child,
        pid_file: dir.join(pid_file_name()),
    })
}

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

fn config_yaml(advertise_ip: Option<IpAddr>, creds: &Credentials) -> String {
    config_yaml_for(advertise_ip, ports(), creds)
}

fn config_yaml_for(advertise_ip: Option<IpAddr>, p: LivekitPorts, creds: &Credentials) -> String {
    let node_ip = advertise_ip
        .map(|ip| format!("  node_ip: {ip}\n"))
        .unwrap_or_default();
    let (ws, tcp, udp) = (p.ws, p.tcp, p.udp);
    let (key, secret) = (yaml_quote(&creds.key), yaml_quote(&creds.secret));
    format!(
        "port: {ws}\n\
         bind_addresses:\n  - 0.0.0.0\n\
         rtc:\n  tcp_port: {tcp}\n  udp_port: {udp}\n  use_external_ip: false\n{node_ip}\
         keys:\n  {key}: {secret}\n\
         logging:\n  level: info\n",
    )
}

// Operator-supplied keys reach this YAML too, and a bare digit run would parse as a number.
fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

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

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dioxusfun-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

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
        assert_eq!(ports_from(Some("65534")), d, "too close to the top");

        let moved = ports_from(Some("7890"));
        assert_eq!(moved.ws, 7890);
        assert_eq!(moved.tcp, 7891, "the block stays adjacent");
        assert_eq!(moved.udp, 7892);
        let creds = Credentials::generate();
        assert!(
            config_yaml_for(None, moved, &creds).contains("port: 7890"),
            "the config the SFU reads has to agree with what we advertise"
        );
        assert!(config_yaml_for(None, moved, &creds).contains("udp_port: 7892"));
    }

    #[test]
    fn media_rides_a_single_udp_port() {
        let yaml = config_yaml(None, &Credentials::generate());
        assert!(
            yaml.contains(&format!("udp_port: {DEFAULT_LIVEKIT_UDP_PORT}")),
            "{yaml}"
        );
        assert!(!yaml.contains("port_range"), "{yaml}");
    }

    #[test]
    fn node_ip_appears_only_when_advertising() {
        let creds = Credentials::generate();
        assert!(!config_yaml(None, &creds).contains("node_ip"));

        let yaml = config_yaml(Some("203.0.113.5".parse().unwrap()), &creds);
        assert!(yaml.contains("node_ip: 203.0.113.5"), "{yaml}");
        assert!(yaml.contains("use_external_ip: false"), "{yaml}");
    }

    #[test]
    fn the_sfu_is_keyed_with_what_we_generated_and_nothing_public() {
        let creds = Credentials::generate();
        let yaml = config_yaml(None, &creds);
        assert!(
            yaml.contains(&format!(
                "keys:\n  \"{}\": \"{}\"\n",
                creds.key, creds.secret
            )),
            "{yaml}"
        );
        assert!(!yaml.contains("devkey"), "{yaml}");
        assert!(!yaml.contains("secret-must-be-at-least"), "{yaml}");

        let odd = Credentials {
            key: "k\"ey".into(),
            secret: "s\\ecret".into(),
        };
        assert!(
            config_yaml(None, &odd).contains("\"k\\\"ey\": \"s\\\\ecret\""),
            "quotes and backslashes survive the trip through YAML"
        );
    }

    #[test]
    fn credentials_persist_per_dir_and_differ_across_dirs() {
        let base = scratch("keys");
        let (a, b) = (base.join("a"), base.join("b"));

        let first = credentials(&a).expect("first");
        assert!(
            first.key.starts_with("dxf") && first.key.len() == 11,
            "{}",
            first.key
        );
        assert_eq!(
            first.secret.len(),
            64,
            "LiveKit refuses secrets under 32 chars"
        );
        assert!(first.secret.chars().all(|c| c.is_ascii_hexdigit()));

        assert_eq!(
            credentials(&a).expect("second"),
            first,
            "a restart keeps the keys"
        );
        assert_ne!(credentials(&b).expect("other dir"), first);
        assert!(!a.join(format!("{KEYS_FILE}.tmp")).exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(a.join(KEYS_FILE))
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
        }

        fs::write(a.join(KEYS_FILE), "only-a-key\n").expect("truncate");
        let regenerated = credentials(&a).expect("regenerate");
        assert_ne!(regenerated, first, "a half-written file is not trusted");
        assert_eq!(credentials(&a).expect("stable again"), regenerated);

        fs::write(a.join(KEYS_FILE), "\n\n").expect("blank");
        assert_ne!(credentials(&a).expect("blank lines"), regenerated);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_planted_binary_under_the_right_name_is_replaced() {
        let dir = scratch("bintest").join("livekit");
        private_dir(&dir).expect("private dir");
        let name = "livekit-server-cafebabe";
        let payload = b"#!/bin/sh\nexit 0\n";

        fs::write(dir.join(name), b"not the bundle").expect("plant");
        let path = ensure_binary(&dir, name, payload).expect("replace");
        assert_eq!(path, dir.join(name));
        assert_eq!(fs::read(&path).expect("read back"), payload);
        assert!(!dir.join(format!("{name}.tmp")).exists());

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let meta = fs::metadata(&path).expect("meta");
            assert_ne!(meta.permissions().mode() & 0o111, 0, "must be executable");
            let inode = meta.ino();
            ensure_binary(&dir, name, payload).expect("reuse");
            assert_eq!(
                fs::metadata(&path).expect("meta").ino(),
                inode,
                "matching bytes are reused, not rewritten"
            );
            let dir_mode = fs::metadata(&dir).expect("dir meta").permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700, "dir mode {dir_mode:o}");
        }

        let _ = fs::remove_file(&path);
        let fresh = ensure_binary(&dir, name, payload).expect("fresh");
        assert_eq!(fs::read(fresh).expect("read back"), payload);

        let _ = fs::remove_dir_all(dir.parent().expect("parent"));
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_wide_open_dir_is_tightened() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("dirtest").join("livekit");
        fs::create_dir_all(&dir).expect("create");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("loosen");

        private_dir(&dir).expect("tighten");
        let mode = fs::metadata(&dir).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "mode {mode:o}");

        let _ = fs::remove_dir_all(dir.parent().expect("parent"));
    }

    #[test]
    fn a_pid_record_names_only_our_own_image() {
        assert_eq!(
            parse_pid_record("42\nlivekit-server-deadbeef\n"),
            Some((42, "livekit-server-deadbeef"))
        );
        assert_eq!(
            parse_pid_record(" 7 \n  livekit-server-deadbeef.exe  \n"),
            Some((7, "livekit-server-deadbeef.exe"))
        );
        assert_eq!(parse_pid_record("42\nsshd\n"), None, "a foreign image");
        assert_eq!(
            parse_pid_record("42\nlivekit-server\n"),
            None,
            "an unversioned name could be an operator's own SFU"
        );
        assert_eq!(parse_pid_record("42\nlivekit-serverd\n"), None);
        assert_eq!(parse_pid_record("not-a-pid\nlivekit-server-x\n"), None);
        assert_eq!(parse_pid_record("42\n"), None);
    }

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

    #[test]
    fn a_stale_record_is_dropped_rather_than_acted_on() {
        let dir = scratch("pidtest");
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(pid_file_name());

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
