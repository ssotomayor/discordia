use crate::source::Source;
use reqwest::{Client, Response, redirect::Policy};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

const MAX_METADATA_BYTES: usize = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    MetadataAvailable,
    UnsupportedProbe,
    ProviderRejected,
    RateLimited,
    RedirectBlocked,
    TimedOut,
    NetworkError,
    InvalidResponse,
    ResponseTooLarge,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub source: Source,
    pub status: Status,
    pub title: Option<String>,
    pub http_status: Option<u16>,
    pub elapsed_ms: u64,
    pub audio_verified: bool,
    pub playlist_entries_verified: bool,
}

pub struct MetadataProbe {
    client: Client,
}

impl MetadataProbe {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Client::builder()
                .https_only(true)
                .no_proxy()
                .redirect(Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .timeout(REQUEST_TIMEOUT)
                .user_agent("Discordia-Music-Source-Probe/0.1")
                .build()?,
        })
    }

    pub async fn inspect(&self, source: Source) -> Report {
        let started = Instant::now();
        let mut report = Report {
            source,
            status: Status::UnsupportedProbe,
            title: None,
            http_status: None,
            elapsed_ms: 0,
            audio_verified: false,
            playlist_entries_verified: false,
        };
        if let Some(endpoint) = report.source.metadata_endpoint() {
            // Provider HTML and caller-supplied URLs never become network destinations.
            let request = self
                .client
                .get(endpoint)
                .query(&[("url", report.source.canonical_url()), ("format", "json")]);
            match tokio::time::timeout(REQUEST_TIMEOUT, async {
                let response = request.send().await.map_err(network_status)?;
                let status = response.status();
                report.http_status = Some(status.as_u16());
                if status.is_redirection() {
                    return Err(Status::RedirectBlocked);
                }
                if status.as_u16() == 429 {
                    return Err(Status::RateLimited);
                }
                if !status.is_success() {
                    return Err(Status::ProviderRejected);
                }
                read_metadata(response).await
            })
            .await
            {
                Ok(Ok(title)) => {
                    report.title = Some(title);
                    report.status = Status::MetadataAvailable;
                }
                Ok(Err(status)) => report.status = status,
                Err(_) => report.status = Status::TimedOut,
            }
        }
        report.elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        report
    }
}

fn network_status(error: reqwest::Error) -> Status {
    if error.is_timeout() {
        Status::TimedOut
    } else {
        Status::NetworkError
    }
}

async fn read_metadata(mut response: Response) -> Result<String, Status> {
    if response
        .content_length()
        .is_some_and(|len| len > MAX_METADATA_BYTES as u64)
    {
        return Err(Status::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network_status)? {
        if chunk.len() > MAX_METADATA_BYTES - bytes.len() {
            return Err(Status::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    parse_title(&bytes)
}

#[derive(Deserialize)]
struct Metadata {
    title: String,
}

fn parse_title(bytes: &[u8]) -> Result<String, Status> {
    let metadata: Metadata = serde_json::from_slice(bytes).map_err(|_| Status::InvalidResponse)?;
    let title = metadata.title.trim();
    if title.is_empty() || title.chars().any(char::is_control) {
        return Err(Status::InvalidResponse);
    }
    Ok(title.chars().take(512).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    async fn fixture(response: Vec<u8>) -> (Response, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                assert!(request.len() < 2048);
                request.push(socket.read_u8().await.unwrap());
            }
            socket.write_all(&response).await.unwrap();
        });
        let response = Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap();
        (response, task)
    }

    #[tokio::test]
    async fn consumes_bounded_metadata_without_using_embed_html() {
        let body = br#"{"title":" A song ","html":"<script>untrusted()</script>","thumbnail_url":"http://127.0.0.1/private"}"#;
        let mut wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        wire.extend_from_slice(body);
        let (response, task) = fixture(wire).await;
        assert_eq!(read_metadata(response).await.unwrap(), "A song");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn bounds_chunked_response_without_content_length() {
        let body = vec![b'x'; MAX_METADATA_BYTES + 1];
        let mut wire = format!(
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n",
            body.len()
        )
        .into_bytes();
        wire.extend_from_slice(&body);
        wire.extend_from_slice(b"\r\n0\r\n\r\n");
        let (response, task) = fixture(wire).await;
        assert_eq!(
            read_metadata(response).await.unwrap_err(),
            Status::ResponseTooLarge
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_excessive_content_length_before_reading_body() {
        let wire = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_METADATA_BYTES + 1
        )
        .into_bytes();
        let (response, task) = fixture(wire).await;
        assert_eq!(
            read_metadata(response).await.unwrap_err(),
            Status::ResponseTooLarge
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn unsupported_probe_never_claims_playback() {
        let report = MetadataProbe::new()
            .unwrap()
            .inspect(Source::parse("https://tidal.com/track/123").unwrap())
            .await;
        assert_eq!(report.status, Status::UnsupportedProbe);
        assert!(!report.audio_verified);
        assert!(!report.playlist_entries_verified);
        assert!(report.http_status.is_none());
    }

    #[test]
    fn malformed_metadata_is_not_success() {
        for body in [
            b"<html>blocked</html>".as_slice(),
            br#"{"html":"embed"}"#,
            br#"{"title":" "}"#,
            br#"{"title":"line\nbreak"}"#,
        ] {
            assert_eq!(parse_title(body).unwrap_err(), Status::InvalidResponse);
        }
    }
}
