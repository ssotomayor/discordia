//! Everything Google-specific: the service-account JWT, the OAuth exchange and
//! FCM HTTP v1. The base URLs come in from outside so tests can play Google.

use std::path::Path;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use discordia_protocol::push::PushData;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde::Deserialize;

use crate::sender::{BoxFuture, Delivery, Sender, kind, short};

pub const FCM_BASE: &str = "https://fcm.googleapis.com";
pub const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";

/// A token that would expire mid-request is renewed instead.
const EARLY: Duration = Duration::from_secs(120);
const TTL: &str = "86400s";
const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct ServiceAccountFile {
    project_id: String,
    client_email: String,
    private_key: String,
    token_uri: String,
}

#[derive(Debug, Clone, Copy)]
pub struct Retry {
    /// Tries in all, the first included.
    pub attempts: u32,
    pub backoff: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            attempts: 4,
            backoff: Duration::from_millis(500),
        }
    }
}

pub struct FcmSender {
    http: reqwest::Client,
    project_id: String,
    client_email: String,
    token_uri: String,
    key: RsaKeyPair,
    fcm_base: String,
    retry: Retry,
    // Held across the exchange so concurrent sends mint one token, not one each.
    access: tokio::sync::Mutex<Option<(String, Instant)>>,
}

fn pem_to_der(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("-----"))
        .collect();
    STANDARD
        .decode(body)
        .map_err(|_| "the service account's private_key is not PEM".to_string())
}

impl FcmSender {
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("FCM credentials {}: {e}", path.display()))?;
        Self::from_json(&text, FCM_BASE, Retry::default())
            .map_err(|e| format!("FCM credentials {}: {e}", path.display()))
    }

    pub fn from_json(json: &str, fcm_base: &str, retry: Retry) -> Result<Self, String> {
        let file: ServiceAccountFile =
            serde_json::from_str(json).map_err(|e| format!("not a service account key: {e}"))?;
        let key = RsaKeyPair::from_pkcs8(&pem_to_der(&file.private_key)?)
            .map_err(|e| format!("the private_key is not a PKCS#8 RSA key: {e}"))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            http,
            project_id: file.project_id,
            client_email: file.client_email,
            token_uri: file.token_uri,
            key,
            fcm_base: fcm_base.trim_end_matches('/').to_string(),
            retry,
            access: tokio::sync::Mutex::new(None),
        })
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    fn assertion(&self, now: u64) -> Result<String, String> {
        let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = serde_json::json!({
            "iss": self.client_email,
            "scope": SCOPE,
            "aud": self.token_uri,
            "iat": now,
            "exp": now + 3600,
        });
        let claims = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signing_input = format!("{header}.{claims}");
        let mut sig = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &ring::rand::SystemRandom::new(),
                signing_input.as_bytes(),
                &mut sig,
            )
            .map_err(|_| "RS256 signing failed".to_string())?;
        Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }

    async fn access_token(&self) -> Result<String, String> {
        let mut cached = self.access.lock().await;
        if let Some((token, until)) = cached.as_ref()
            && Instant::now() < *until
        {
            return Ok(token.clone());
        }
        #[derive(Deserialize)]
        struct Granted {
            access_token: String,
            expires_in: u64,
        }
        let assertion = self.assertion(crate::now_unix())?;
        let resp = self
            .http
            .post(&self.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("OAuth exchange: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            // Google's error here names the problem and carries no secret.
            let body = resp.text().await.unwrap_or_default();
            return Err(format!(
                "OAuth exchange: {status}: {}",
                truncate(&body, 300)
            ));
        }
        let granted: Granted = resp
            .json()
            .await
            .map_err(|e| format!("OAuth exchange: {e}"))?;
        let until = Instant::now() + Duration::from_secs(granted.expires_in).saturating_sub(EARLY);
        *cached = Some((granted.access_token.clone(), until));
        Ok(granted.access_token)
    }

    async fn forget_token(&self) {
        *self.access.lock().await = None;
    }

    async fn deliver(&self, token: &str, data: &PushData, validate_only: bool) -> Delivery {
        let url = format!(
            "{}/v1/projects/{}/messages:send",
            self.fcm_base, self.project_id
        );
        let body = serde_json::json!({
            "validate_only": validate_only,
            "message": {
                "token": token,
                "data": data,
                "android": { "priority": "HIGH", "ttl": TTL },
            },
        });
        let mut reauthorised = false;
        let mut attempt = 0;
        loop {
            attempt += 1;
            let access = match self.access_token().await {
                Ok(a) => a,
                Err(e) => return Delivery::Failed(e),
            };
            let result = self
                .http
                .post(&url)
                .bearer_auth(&access)
                .json(&body)
                .send()
                .await;
            let (status, retry_after, text) = match result {
                Ok(resp) => {
                    let status = resp.status();
                    let retry_after = resp
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let text = resp
                        .text()
                        .await
                        .unwrap_or_default()
                        .replace(token, "<token>");
                    (Some(status), retry_after, text)
                }
                Err(e) => (None, None, e.to_string()),
            };
            let verdict = match status {
                Some(s) if s.is_success() => return Delivery::Sent,
                Some(s) => classify(s.as_u16(), &text),
                None => Verdict::Retry,
            };
            match verdict {
                Verdict::TokenGone => return Delivery::TokenGone,
                Verdict::Reauthorise if !reauthorised => {
                    reauthorised = true;
                    self.forget_token().await;
                    continue;
                }
                Verdict::Retry if attempt < self.retry.attempts => {
                    let backoff = self.retry.backoff * 2u32.pow(attempt - 1);
                    let wait = retry_after.map_or(backoff, |r| r.min(MAX_RETRY_AFTER).max(backoff));
                    tokio::time::sleep(wait).await;
                    continue;
                }
                _ => {
                    let what = status.map_or_else(|| "transport".to_string(), |s| s.to_string());
                    return Delivery::Failed(format!("FCM {what}: {}", truncate(&text, 300)));
                }
            }
        }
    }
}

impl Sender for FcmSender {
    fn send<'a>(
        &'a self,
        token: &'a str,
        data: &'a PushData,
        validate_only: bool,
    ) -> BoxFuture<'a, Delivery> {
        Box::pin(async move {
            let outcome = self.deliver(token, data, validate_only).await;
            if let Delivery::Failed(e) = &outcome {
                tracing::warn!(token = short(token), kind = kind(data), error = %e, "FCM send failed");
            }
            outcome
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    TokenGone,
    Reauthorise,
    Retry,
    Drop,
}

/// An FCM v1 error: `error.status`, and an `FcmError` detail whose
/// `errorCode` is the specific reason.
fn classify(status: u16, body: &str) -> Verdict {
    #[derive(Deserialize, Default)]
    struct Envelope {
        #[serde(default)]
        error: Error,
    }
    #[derive(Deserialize, Default)]
    struct Error {
        #[serde(default)]
        status: String,
        #[serde(default)]
        message: String,
        #[serde(default)]
        details: Vec<serde_json::Value>,
    }
    let error = serde_json::from_str::<Envelope>(body)
        .unwrap_or_default()
        .error;
    let code = error
        .details
        .iter()
        .find_map(|d| d.get("errorCode").and_then(|c| c.as_str()))
        .unwrap_or(&error.status);
    // INVALID_ARGUMENT also means a malformed message, which is ours, not the token's.
    let names_token = error.details.iter().any(|d| {
        d.get("fieldViolations")
            .and_then(|v| v.as_array())
            .is_some_and(|v| {
                v.iter()
                    .any(|f| f.get("field").and_then(|f| f.as_str()) == Some("message.token"))
            })
    }) || error
        .message
        .to_ascii_lowercase()
        .contains("registration token");
    match (status, code) {
        (_, "UNREGISTERED") => Verdict::TokenGone,
        (_, "INVALID_ARGUMENT") if names_token => Verdict::TokenGone,
        (401, _) => Verdict::Reauthorise,
        (429, _) | (500..=599, _) => Verdict::Retry,
        (_, "QUOTA_EXCEEDED" | "UNAVAILABLE" | "INTERNAL") => Verdict::Retry,
        _ => Verdict::Drop,
    }
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_sorted_by_what_can_be_done_about_them() {
        let unregistered = r#"{"error":{"code":404,"status":"NOT_FOUND","message":"Requested entity was not found.","details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"UNREGISTERED"}]}}"#;
        assert_eq!(classify(404, unregistered), Verdict::TokenGone);

        let bad_token = r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"The registration token is not a valid FCM registration token","details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"INVALID_ARGUMENT"},{"@type":"type.googleapis.com/google.rpc.BadRequest","fieldViolations":[{"field":"message.token","description":"Invalid registration token"}]}]}}"#;
        assert_eq!(classify(400, bad_token), Verdict::TokenGone);

        let bad_message = r#"{"error":{"code":400,"status":"INVALID_ARGUMENT","message":"Invalid value at 'message.data'","details":[{"@type":"type.googleapis.com/google.firebase.fcm.v1.FcmError","errorCode":"INVALID_ARGUMENT"}]}}"#;
        assert_eq!(classify(400, bad_message), Verdict::Drop);

        assert_eq!(classify(503, ""), Verdict::Retry);
        assert_eq!(
            classify(429, r#"{"error":{"status":"RESOURCE_EXHAUSTED"}}"#),
            Verdict::Retry
        );
        assert_eq!(classify(401, ""), Verdict::Reauthorise);
        let mismatch = r#"{"error":{"code":403,"status":"PERMISSION_DENIED","details":[{"errorCode":"SENDER_ID_MISMATCH"}]}}"#;
        assert_eq!(classify(403, mismatch), Verdict::Drop);
    }

    #[test]
    fn a_pem_key_must_be_pkcs8_rsa() {
        let json = serde_json::json!({
            "project_id": "p", "client_email": "e", "token_uri": "http://x",
            "private_key": "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
        });
        let err = FcmSender::from_json(&json.to_string(), FCM_BASE, Retry::default())
            .err()
            .unwrap();
        assert!(err.contains("PKCS#8"), "{err}");
    }
}
