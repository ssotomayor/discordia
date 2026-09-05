//! Discord's local RPC socket, spoken well enough for a game's existing Rich
//! Presence integration to reach us with no cooperation from anybody.
//!
//! Frames are a 4-byte LE opcode, a 4-byte LE length, then JSON. Only the
//! handshake, `SET_ACTIVITY` and the ping pair matter for presence; everything
//! else is answered politely and dropped.

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedSender;

use crate::protocol::{Activity, ActivityKind};

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;

/// A frame big enough to be a bug rather than a presence. Discord's own cap is
/// in this region and a game that trips it is not one we can display anyway.
const MAX_FRAME: u32 = 64 * 1024;

/// How many socket names to claim. Discord uses 0-9 and a second client takes
/// the next free one, so a game walks the range until something answers.
const SLOTS: u8 = 10;

pub struct RpcActivity {
    pub activity: Option<Activity>,
}

/// The paths a game already looks in. Ours is the same shape under a different
/// name, for when squatting Discord's is not wanted.
pub fn socket_names(discord_compatible: bool) -> Vec<String> {
    let stem = if discord_compatible {
        "discord-ipc"
    } else {
        "dioxusfun-ipc"
    };
    (0..SLOTS).map(|n| format!("{stem}-{n}")).collect()
}

/// `SET_ACTIVITY` carries the shape Discord's SDK sends. `assets` is dropped on
/// purpose: `large_image` is a key into a dashboard we do not host, so the only
/// honest thing to do with it is show the text instead of a broken picture.
fn activity_from_frame(args: &Value) -> Option<Activity> {
    let activity = args.get("activity")?;
    if activity.is_null() {
        return None;
    }
    let text = |key: &str| {
        activity
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|s| !s.trim().is_empty())
    };
    // A game names itself by `client_id`, and the human name of that app lives
    // in Discord's dashboard. `details` is the closest thing we can show.
    let name = text("name")
        .or_else(|| text("details"))
        .or_else(|| text("state"))?;
    let started_ms = activity
        .get("timestamps")
        .and_then(|t| t.get("start"))
        .and_then(Value::as_i64)
        .map(normalise_timestamp);
    // Whichever field was promoted must not also print underneath itself.
    let details = text("details").filter(|d| *d != name);
    let state = text("state").filter(|st| *st != name);
    Some(Activity {
        kind: ActivityKind::Playing,
        name,
        details,
        state,
        started_ms,
    })
}

/// The SDK sends seconds, the docs say milliseconds, and games ship both. A
/// start in 1970 is the seconds reading, so scale it.
fn normalise_timestamp(raw: i64) -> i64 {
    const YEAR_2001_MS: i64 = 978_307_200_000;
    if raw > 0 && raw < YEAR_2001_MS {
        raw * 1000
    } else {
        raw
    }
}

async fn write_frame<S>(stream: &mut S, op: u32, body: &Value) -> std::io::Result<()>
where
    S: AsyncWriteExt + Unpin,
{
    let json = serde_json::to_vec(body).unwrap_or_else(|_| b"{}".to_vec());
    let mut out = Vec::with_capacity(8 + json.len());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    stream.write_all(&out).await
}

/// One connected game, until it goes away. Every exit path clears the activity,
/// because a crashed game that stays "playing" forever is the worst outcome.
pub async fn serve<S>(mut stream: S, tx: UnboundedSender<RpcActivity>)
where
    S: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let mut handshaken = false;
    loop {
        let mut header = [0u8; 8];
        if stream.read_exact(&mut header).await.is_err() {
            break;
        }
        let op = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        if len > MAX_FRAME {
            break;
        }
        let mut body = vec![0u8; len as usize];
        if stream.read_exact(&mut body).await.is_err() {
            break;
        }
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

        match op {
            OP_HANDSHAKE => {
                handshaken = true;
                let ready = json!({
                    "cmd": "DISPATCH",
                    "evt": "READY",
                    "data": { "v": 1, "config": {} },
                });
                if write_frame(&mut stream, OP_FRAME, &ready).await.is_err() {
                    break;
                }
            }
            OP_PING => {
                if write_frame(&mut stream, OP_PONG, &json).await.is_err() {
                    break;
                }
            }
            OP_FRAME if handshaken => {
                let cmd = json.get("cmd").and_then(Value::as_str).unwrap_or_default();
                if cmd == "SET_ACTIVITY" {
                    let activity = json.get("args").and_then(activity_from_frame);
                    if tx.send(RpcActivity { activity }).is_err() {
                        break;
                    }
                }
                let nonce = json.get("nonce").cloned().unwrap_or(Value::Null);
                let ack = json!({ "cmd": cmd, "data": null, "evt": null, "nonce": nonce });
                if write_frame(&mut stream, OP_FRAME, &ack).await.is_err() {
                    break;
                }
            }
            OP_CLOSE => break,
            _ => {}
        }
    }
    let _ = tx.send(RpcActivity { activity: None });
}

#[cfg(unix)]
pub async fn listen(names: Vec<String>, tx: UnboundedSender<RpcActivity>) {
    use tokio::net::UnixListener;

    let Some(dir) = socket_dir() else { return };
    for name in names {
        let path = dir.join(&name);
        // A path left by a crash binds nothing and refuses everything, so the
        // stale file is removed before the slot is judged taken.
        if std::os::unix::net::UnixStream::connect(&path).is_err() {
            let _ = std::fs::remove_file(&path);
        }
        let Ok(listener) = UnixListener::bind(&path) else {
            continue;
        };
        tracing::info!(socket = %path.display(), "rich presence socket listening");
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, tx.clone()));
            }
        });
        return;
    }
    tracing::warn!("no free rich presence socket; another client holds them all");
}

#[cfg(unix)]
fn socket_dir() -> Option<std::path::PathBuf> {
    ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .iter()
        .find_map(|k| std::env::var_os(k).map(std::path::PathBuf::from))
        .or_else(|| Some(std::path::PathBuf::from("/tmp")))
}

#[cfg(windows)]
pub async fn listen(names: Vec<String>, tx: UnboundedSender<RpcActivity>) {
    use tokio::net::windows::named_pipe::ServerOptions;

    for name in names {
        let path = format!(r"\\.\pipe\{name}");
        let Ok(first) = ServerOptions::new().first_pipe_instance(true).create(&path) else {
            continue;
        };
        tracing::info!(pipe = %path, "rich presence pipe listening");
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut server = first;
            loop {
                if server.connect().await.is_err() {
                    break;
                }
                // The next instance has to exist before this one is handed off,
                // or the pipe name disappears between two games connecting.
                let next = match ServerOptions::new().create(&path) {
                    Ok(next) => next,
                    Err(_) => break,
                };
                let connected = std::mem::replace(&mut server, next);
                tokio::spawn(serve(connected, tx.clone()));
            }
        });
        return;
    }
    tracing::warn!("no free rich presence pipe; another client holds them all");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seconds_timestamp_is_scaled_and_a_millis_one_is_left_alone() {
        assert_eq!(normalise_timestamp(1_700_000_000), 1_700_000_000_000);
        assert_eq!(normalise_timestamp(1_700_000_000_000), 1_700_000_000_000);
    }

    #[test]
    fn a_frame_without_a_nameable_field_yields_nothing() {
        assert!(activity_from_frame(&json!({ "activity": {} })).is_none());
        assert!(activity_from_frame(&json!({ "activity": null })).is_none());
        assert!(activity_from_frame(&json!({})).is_none());
    }

    #[test]
    fn details_stand_in_for_a_name_the_dashboard_would_have_held() {
        let a = activity_from_frame(&json!({
            "activity": { "details": "Seablock", "state": "hour 400" }
        }))
        .unwrap();
        assert_eq!(a.name, "Seablock");
        // Not repeated underneath itself once it has been promoted.
        assert_eq!(a.details, None);
        assert_eq!(a.state.as_deref(), Some("hour 400"));
    }

    #[test]
    fn a_named_activity_keeps_all_three_lines_and_its_clock() {
        let a = activity_from_frame(&json!({
            "activity": {
                "name": "Factorio",
                "details": "Seablock",
                "state": "In a Match",
                "timestamps": { "start": 1_700_000_000 },
            }
        }))
        .unwrap();
        assert_eq!(a.name, "Factorio");
        assert_eq!(a.details.as_deref(), Some("Seablock"));
        assert_eq!(a.state.as_deref(), Some("In a Match"));
        assert_eq!(a.started_ms, Some(1_700_000_000_000));
    }

    #[test]
    fn the_two_socket_families_never_collide() {
        let ours = socket_names(false);
        let theirs = socket_names(true);
        assert_eq!(ours.len(), SLOTS as usize);
        assert!(ours.iter().all(|n| !theirs.contains(n)));
        assert_eq!(theirs[0], "discord-ipc-0");
    }
}
