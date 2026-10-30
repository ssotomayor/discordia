//! Who holds the media port. Readiness sends this host's credentials, so it
//! waits until the port is shown to be the child's: anything else that
//! answers there would be handed an admin token, then every caller's.

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Holder {
    Nobody,
    Child,
    /// Another process, or one this user cannot see — which is another user's.
    Other,
}

pub(super) fn holder(port: u16, child: u32) -> Holder {
    let Some(pids) = listening_pids(port) else {
        return Holder::Other;
    };
    if pids.iter().any(|p| *p != child) || shadowed_on_loopback(port) {
        Holder::Other
    } else if pids.is_empty() {
        Holder::Nobody
    } else {
        Holder::Child
    }
}

/// `0` stands for a listener whose process this user cannot see.
#[cfg(target_os = "linux")]
fn listening_pids(port: u16) -> Option<Vec<u32>> {
    use std::collections::HashSet;
    const LISTEN: &str = "0A";
    let mut inodes: HashSet<String> = HashSet::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        for row in text.lines().skip(1) {
            let cols: Vec<&str> = row.split_whitespace().collect();
            let Some((local, state, inode)) = cols
                .get(1)
                .zip(cols.get(3))
                .zip(cols.get(9))
                .map(|((l, s), i)| (l, s, i))
            else {
                continue;
            };
            let on_port = local
                .rsplit_once(':')
                .and_then(|(_, p)| u16::from_str_radix(p, 16).ok())
                == Some(port);
            if on_port && *state == LISTEN {
                inodes.insert(format!("socket:[{inode}]"));
            }
        }
    }
    let mut pids = Vec::new();
    let mut seen = HashSet::new();
    for proc in std::fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = proc.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(fds) = std::fs::read_dir(proc.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if let Ok(target) = std::fs::read_link(fd.path()) {
                let target = target.to_string_lossy().into_owned();
                if inodes.contains(&target) && seen.insert(target) {
                    pids.push(pid);
                }
            }
        }
    }
    if seen.len() < inodes.len() {
        pids.push(0);
    }
    Some(pids)
}

/// `lsof` lists only this user's processes, so another user's listener is
/// caught by `shadowed_on_loopback` instead.
#[cfg(all(unix, not(target_os = "linux")))]
fn listening_pids(port: u16) -> Option<Vec<u32>> {
    let out = std::process::Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fp"])
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix('p')?.parse().ok())
            .collect(),
    )
}

#[cfg(windows)]
fn listening_pids(port: u16) -> Option<Vec<u32>> {
    use std::os::windows::process::CommandExt;
    let mut pids = Vec::new();
    for proto in ["TCP", "TCPv6"] {
        let out = std::process::Command::new("netstat")
            .args(["-ano", "-p", proto])
            .creation_flags(super::CREATE_NO_WINDOW)
            .output()
            .ok()?;
        for row in String::from_utf8_lossy(&out.stdout).lines() {
            let cols: Vec<&str> = row.split_whitespace().collect();
            if cols.len() == 5
                && cols[3] == "LISTENING"
                && cols[1]
                    .rsplit_once(':')
                    .and_then(|(_, p)| p.parse::<u16>().ok())
                    == Some(port)
                && let Ok(pid) = cols[4].parse()
            {
                pids.push(pid);
            }
        }
    }
    Some(pids)
}

/// The SFU binds the wildcard, and macOS lets another socket bind 127.0.0.1
/// on the same port and take every loopback connection. Binding that address
/// ourselves fails exactly when someone already has.
#[cfg(all(unix, not(target_os = "linux")))]
fn shadowed_on_loopback(port: u16) -> bool {
    // Bound, never listening, so no caller's connection can land on the probe.
    let probe = tokio::net::TcpSocket::new_v4().and_then(|s| {
        s.set_reuseaddr(true)?;
        s.bind(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
    });
    probe.is_err()
}

#[cfg(any(target_os = "linux", windows))]
fn shadowed_on_loopback(_port: u16) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listener_is_ours_only_when_the_child_holds_it() {
        let listener = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let me = std::process::id();
        assert_eq!(holder(port, me), Holder::Child);
        assert_eq!(
            holder(port, me + 1),
            Holder::Other,
            "another process's listener"
        );
        drop(listener);
    }
}
