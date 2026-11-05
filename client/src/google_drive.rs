//! Google sign-in for one thing: the Drive folder only this app can see.
//! The token is used for one action and dropped; nothing about the account is kept.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use zeroize::Zeroizing;

#[cfg(not(target_os = "android"))]
mod loopback;

const FILES: &str = "https://www.googleapis.com/drive/v3/files";
const UPLOAD: &str = "https://www.googleapis.com/upload/drive/v3/files";
#[cfg(not(target_os = "android"))]
const SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";
const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LISTED: usize = 100;

/// Android asks Play services, which knows the app by its signing certificate;
/// desktop needs the OAuth client compiled in.
pub fn available() -> bool {
    #[cfg(target_os = "android")]
    return true;
    #[cfg(not(target_os = "android"))]
    loopback::configured()
}

pub struct Session {
    token: Zeroizing<String>,
    http: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub id: String,
    pub name: String,
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// `done_page` is what the browser tab shows afterwards on desktop.
pub async fn sign_in(done_page: String) -> Result<Session, String> {
    let http = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|_| "Couldn't reach Google.".to_string())?;
    #[cfg(target_os = "android")]
    let token = {
        let _ = done_page;
        crate::android::drive_access_token().await?
    };
    #[cfg(not(target_os = "android"))]
    let token = loopback::token(&http, done_page).await?;
    Ok(Session { token, http })
}

fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn drive_error(status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        401 | 403 => "Google Drive refused the request. Sign in again.".into(),
        507 => "Your Google Drive is full.".into(),
        _ => "Couldn't reach Google Drive.".into(),
    }
}

impl Session {
    pub async fn list(&self) -> Result<Vec<Remote>, String> {
        let res = self
            .http
            .get(FILES)
            .bearer_auth(self.token.as_str())
            .query(&[
                ("spaces", "appDataFolder"),
                ("fields", "files(id,name)"),
                ("pageSize", "100"),
                ("q", "trashed = false"),
            ])
            .send()
            .await
            .map_err(|_| "Couldn't reach Google Drive.".to_string())?;
        if !res.status().is_success() {
            return Err(drive_error(res.status()));
        }
        let body: serde_json::Value = res
            .json()
            .await
            .map_err(|_| "Google Drive sent an unreadable answer.".to_string())?;
        Ok(body["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| {
                let id = f["id"].as_str().filter(|id| valid_id(id))?;
                Some(Remote {
                    id: id.to_owned(),
                    name: f["name"].as_str()?.to_owned(),
                })
            })
            .take(MAX_LISTED)
            .collect())
    }

    pub async fn download(&self, remote: &Remote, max_bytes: usize) -> Result<String, String> {
        let mut res = self
            .http
            .get(format!("{FILES}/{}", remote.id))
            .bearer_auth(self.token.as_str())
            .query(&[("alt", "media")])
            .send()
            .await
            .map_err(|_| "Couldn't reach Google Drive.".to_string())?;
        if !res.status().is_success() {
            return Err(drive_error(res.status()));
        }
        let mut body = Zeroizing::new(Vec::new());
        while let Some(chunk) = res
            .chunk()
            .await
            .map_err(|_| "Couldn't reach Google Drive.".to_string())?
        {
            if body.len() + chunk.len() > max_bytes {
                return Err("A file in Google Drive is too large to be a backup.".into());
            }
            body.extend_from_slice(&chunk);
        }
        String::from_utf8(body.to_vec())
            .map_err(|_| "A file in Google Drive is not a backup.".to_string())
    }

    /// Replaces the file named `name` if there is one, so a second backup of
    /// the same key leaves one copy, not two.
    pub async fn put(&self, name: &str, content: &str) -> Result<(), String> {
        let existing = self.list().await?.into_iter().find(|r| r.name == name);
        let request = match existing {
            Some(remote) => self
                .http
                .patch(format!("{UPLOAD}/{}", remote.id))
                .query(&[("uploadType", "media")])
                .header("content-type", "application/json")
                .body(content.to_owned()),
            None => {
                let boundary = format!("discordia-{}", random_token(12));
                let metadata = serde_json::json!({ "name": name, "parents": ["appDataFolder"] });
                let body = format!(
                    "--{boundary}\r\ncontent-type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n\
                     --{boundary}\r\ncontent-type: application/json\r\n\r\n{content}\r\n--{boundary}--\r\n"
                );
                self.http
                    .post(UPLOAD)
                    .query(&[("uploadType", "multipart")])
                    .header(
                        "content-type",
                        format!("multipart/related; boundary={boundary}"),
                    )
                    .body(body)
            }
        };
        let res = request
            .bearer_auth(self.token.as_str())
            .send()
            .await
            .map_err(|_| "Couldn't reach Google Drive.".to_string())?;
        if !res.status().is_success() {
            return Err(drive_error(res.status()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_ids_go_into_a_path_only_when_plain() {
        assert!(valid_id("1aB-_9"));
        assert!(!valid_id("../x"));
        assert!(!valid_id("a?b"));
        assert!(!valid_id(""));
    }
}
