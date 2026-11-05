use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use zeroize::Zeroizing;

use super::{SCOPE, random_token};

const AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN: &str = "https://oauth2.googleapis.com/token";
const CONSENT_WINDOW: Duration = Duration::from_secs(5 * 60);
const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
const MAX_REQUEST_BYTES: usize = 8 * 1024;

struct OAuthClient {
    id: String,
    secret: String,
}

fn setting(runtime: &str, built: Option<&'static str>) -> Option<String> {
    std::env::var(runtime)
        .ok()
        .or_else(|| built.map(str::to_owned))
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

// An installed app's client secret ships in every copy, so Google does not treat
// it as confidential; PKCE is what keeps a stolen code useless.
fn oauth_client() -> Option<OAuthClient> {
    Some(OAuthClient {
        id: setting(
            "GOOGLE_OAUTH_CLIENT_ID",
            option_env!("GOOGLE_OAUTH_CLIENT_ID"),
        )?,
        secret: setting(
            "GOOGLE_OAUTH_CLIENT_SECRET",
            option_env!("GOOGLE_OAUTH_CLIENT_SECRET"),
        )?,
    })
}

pub(super) fn configured() -> bool {
    oauth_client().is_some()
}

/// Opens the browser and waits for Google to send it back to us. `done_page`
/// is what the tab shows afterwards, already in the reader's language.
pub(super) async fn token(
    http: &reqwest::Client,
    done_page: String,
) -> Result<Zeroizing<String>, String> {
    let client = oauth_client().ok_or("Google sign-in is not set up in this build.")?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|_| "Couldn't start Google sign-in.".to_string())?;
    let port = listener
        .local_addr()
        .map_err(|_| "Couldn't start Google sign-in.".to_string())?
        .port();
    let redirect = format!("http://127.0.0.1:{port}");
    let verifier = Zeroizing::new(random_token(32));
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token(16);
    let url = url::Url::parse_with_params(
        AUTH,
        [
            ("client_id", client.id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("response_type", "code"),
            ("scope", SCOPE),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("state", state.as_str()),
            ("prompt", "select_account"),
        ],
    )
    .map_err(|_| "Couldn't start Google sign-in.".to_string())?;
    crate::app::open_external(url.as_str());

    let code = tokio::time::timeout(CONSENT_WINDOW, wait_for_code(&listener, &state, &done_page))
        .await
        .map_err(|_| "Google sign-in timed out. Try again.".to_string())??;
    drop(listener);

    let body: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("code", &code)
        .append_pair("client_id", &client.id)
        .append_pair("client_secret", &client.secret)
        .append_pair("redirect_uri", &redirect)
        .append_pair("grant_type", "authorization_code")
        .append_pair("code_verifier", &verifier)
        .finish();
    let body = Zeroizing::new(body);
    let res = http
        .post(TOKEN)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.to_string())
        .send()
        .await
        .map_err(|_| "Couldn't reach Google.".to_string())?;
    if !res.status().is_success() {
        return Err("Google didn't accept the sign-in. Try again.".into());
    }
    let reply: serde_json::Value = res
        .json()
        .await
        .map_err(|_| "Google sent an unreadable answer.".to_string())?;
    // Google's consent screen lets each permission be unticked on its own.
    let granted = reply["scope"]
        .as_str()
        .is_some_and(|scopes| scopes.split(' ').any(|s| s == SCOPE));
    if !granted {
        return Err(
            "Discordia needs access to its own Google Drive folder. Sign in again and leave that box ticked."
                .into(),
        );
    }
    let token = reply["access_token"]
        .as_str()
        .filter(|t| !t.is_empty())
        .ok_or("Google sent an unreadable answer.")?;
    Ok(Zeroizing::new(token.to_owned()))
}

/// Anything on this machine can knock on the port, and browsers open idle
/// connections ahead of use, so each one is read on its own and only the
/// request carrying our `state` ends the wait.
async fn wait_for_code(
    listener: &TcpListener,
    state: &str,
    done_page: &str,
) -> Result<String, String> {
    let mut pending = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                if let Ok((stream, _)) = accepted && pending.len() < 16 {
                    pending.spawn(answer(stream, state.to_owned(), done_page.to_owned()));
                }
            }
            joined = pending.join_next(), if !pending.is_empty() => {
                if let Some(Ok(Some(outcome))) = joined {
                    return outcome;
                }
            }
        }
    }
}

async fn answer(
    mut stream: TcpStream,
    state: String,
    done_page: String,
) -> Option<Result<String, String>> {
    let target = tokio::time::timeout(REQUEST_DEADLINE, request_target(&mut stream))
        .await
        .ok()??;
    match callback(&target, &state) {
        Callback::Code(code) => {
            respond(&mut stream, "200 OK", &done_page).await;
            Some(Ok(code))
        }
        Callback::Refused => {
            respond(&mut stream, "200 OK", &done_page).await;
            Some(Err("Google sign-in was cancelled.".into()))
        }
        Callback::Stray => {
            respond(&mut stream, "404 Not Found", "").await;
            None
        }
    }
}

async fn request_target(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    while !buf.windows(2).any(|w| w == b"\r\n") {
        if buf.len() >= MAX_REQUEST_BYTES {
            return None;
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let line = std::str::from_utf8(&buf).ok()?.lines().next()?;
    let mut parts = line.split(' ');
    (parts.next()? == "GET").then_some(())?;
    parts.next().map(str::to_owned)
}

#[derive(Debug, PartialEq, Eq)]
enum Callback {
    Code(String),
    Refused,
    Stray,
}

fn callback(target: &str, state: &str) -> Callback {
    let Ok(url) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::Stray;
    };
    if url.path() != "/" {
        return Callback::Stray;
    }
    let param = |key: &str| {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    if param("state").as_deref() != Some(state) {
        return Callback::Stray;
    }
    match (param("code"), param("error")) {
        (Some(code), None) if !code.is_empty() => Callback::Code(code),
        _ => Callback::Refused,
    }
}

async fn respond(stream: &mut TcpStream, status: &str, text: &str) {
    let escaped = text
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Discordia</title>\
         <body style=\"font:16px system-ui;display:grid;place-items:center;height:90vh\">\
         <p>{escaped}</p>"
    );
    let reply = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\ncache-control: no-store\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_state_on_the_root_path_ends_the_wait() {
        assert_eq!(
            callback("/?state=s1&code=abc", "s1"),
            Callback::Code("abc".into())
        );
        assert_eq!(callback("/?state=s2&code=abc", "s1"), Callback::Stray);
        assert_eq!(callback("/?code=abc", "s1"), Callback::Stray);
        assert_eq!(callback("/favicon.ico", "s1"), Callback::Stray);
        assert_eq!(
            callback("/?state=s1&error=access_denied", "s1"),
            Callback::Refused
        );
        assert_eq!(callback("/?state=s1&code=", "s1"), Callback::Refused);
    }

    #[tokio::test]
    async fn the_callback_server_answers_strays_and_returns_our_code() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let knock = |path: &'static str| async move {
            let mut s = TcpStream::connect(addr).await.unwrap();
            s.write_all(format!("GET {path} HTTP/1.1\r\nhost: x\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut reply = String::new();
            s.read_to_string(&mut reply).await.unwrap();
            reply
        };
        let wait = tokio::spawn(async move { wait_for_code(&listener, "st", "done <now>").await });
        let _idle = TcpStream::connect(addr).await.unwrap();
        assert!(
            knock("/?state=other&code=x")
                .await
                .starts_with("HTTP/1.1 404")
        );
        let reply = knock("/?state=st&code=the-code").await;
        assert!(reply.contains("done &lt;now&gt;"), "{reply}");
        assert_eq!(wait.await.unwrap(), Ok("the-code".into()));
    }
}
