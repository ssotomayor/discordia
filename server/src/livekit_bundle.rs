use std::fs;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::{Child, Command};

#[cfg(target_os = "windows")]
const LIVEKIT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/livekit-server.exe"));
#[cfg(not(target_os = "windows"))]
const LIVEKIT_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/livekit-server"));

const LIVEKIT_DIGEST: &str = include_str!(concat!(env!("OUT_DIR"), "/livekit-server.sha"));

#[cfg(target_os = "windows")]
const LIVEKIT_BIN_NAME: &str = "livekit-server.exe";
#[cfg(not(target_os = "windows"))]
const LIVEKIT_BIN_NAME: &str = "livekit-server";

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
pub const DEFAULT_LIVEKIT_KEY: &str = "devkey";
pub const DEFAULT_LIVEKIT_SECRET: &str = "secret-must-be-at-least-32-chars-long";

pub struct LivekitSubprocess {
    _child: Child,
    pid_file: PathBuf,
}

impl Drop for LivekitSubprocess {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.pid_file);
    }
}

fn pid_file_name() -> String {
    format!("livekit-server-{}.pid", ports().ws)
}

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

pub async fn spawn_livekit(advertise_ip: Option<IpAddr>) -> Result<LivekitSubprocess, String> {
    if LIVEKIT_BIN.is_empty() {
        return Err("livekit-server binary not bundled in this build".into());
    }

    let dir = std::env::temp_dir().join("dioxusfun");
    fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;

    let digest = LIVEKIT_DIGEST.trim();
    let (stem, ext) = match LIVEKIT_BIN_NAME.split_once('.') {
        Some((s, e)) => (s, format!(".{e}")),
        None => (LIVEKIT_BIN_NAME, String::new()),
    };
    let path: PathBuf = dir.join(format!("{stem}-{digest}{ext}"));

    let config_path = dir.join(format!("livekit-{}.yaml", ports().ws));
    let config = config_yaml(advertise_ip);

    {
        let (dir, path, config_path) = (dir.clone(), path.clone(), config_path.clone());
        let stem = stem.to_string();
        let digest = digest.to_string();
        tokio::task::spawn_blocking(move || -> Result<(), String> {
            reclaim_orphan(&dir);

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
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    let mut child = command.spawn().map_err(|e| format!("spawn livekit: {e}"))?;

    forward_output(&mut child);

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

    #[test]
    fn node_ip_appears_only_when_advertising() {
        assert!(!config_yaml(None).contains("node_ip"));

        let yaml = config_yaml(Some("203.0.113.5".parse().unwrap()));
        assert!(yaml.contains("node_ip: 203.0.113.5"), "{yaml}");
        assert!(yaml.contains("use_external_ip: false"), "{yaml}");
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
        let dir = std::env::temp_dir().join(format!("dioxusfun-pidtest-{}", std::process::id()));
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
