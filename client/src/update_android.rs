#[cfg(target_os = "android")]
use std::path::Path;
use std::path::PathBuf;
#[cfg(test)]
use std::time::Duration;

use tokio::io::AsyncWriteExt;

const MAX_APK_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: usize = 16 * 1024;

struct Partial(PathBuf);

impl Drop for Partial {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(super) async fn fetch_verified(
    tag: &str,
    asset: &str,
    asset_url: &str,
    signature_url: &str,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf, String> {
    if asset != "Discordia-android-arm64.apk" {
        return Err("Unexpected Android update asset.".into());
    }
    let client = super::download_client(super::DOWNLOAD_IDLE_TIMEOUT)
        .build()
        .map_err(super::download_error)?;
    let mut response = client
        .get(signature_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(super::download_error)?;
    let mut signature = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(super::download_error)? {
        if signature.len() + chunk.len() > MAX_SIGNATURE_BYTES {
            return Err("Update signature is too large.".into());
        }
        signature.extend_from_slice(&chunk);
    }
    let signature = std::str::from_utf8(&signature).map_err(|e| e.to_string())?;
    let (key, signature) =
        super::signing_material(signature, tag, asset).map_err(|e| e.to_string())?;
    let response = client
        .get(asset_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(super::download_error)?;
    let directory = crate::identity::config_dir().join("updates");
    download_response(response, directory, &key, &signature, &mut progress).await
}

async fn download_response(
    mut response: reqwest::Response,
    directory: PathBuf,
    key: &minisign_verify::PublicKey,
    signature: &minisign_verify::Signature,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf, String> {
    let mut verifier = key.verify_stream(signature).map_err(|e| e.to_string())?;
    let total = response.content_length();
    if total.is_some_and(|n| n > MAX_APK_BYTES) {
        return Err("Android update is too large.".into());
    }
    tokio::fs::create_dir_all(&directory)
        .await
        .map_err(|e| e.to_string())?;
    let mut entries = tokio::fs::read_dir(&directory)
        .await
        .map_err(|e| e.to_string())?;
    while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "partial")
            && entry
                .file_type()
                .await
                .map_err(|e| e.to_string())?
                .is_file()
        {
            tokio::fs::remove_file(entry.path())
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    let partial = Partial(directory.join(format!("{}.partial", uuid::Uuid::new_v4())));
    let path = directory.join("Discordia-android-arm64.apk");
    let mut file = tokio::fs::File::create(&partial.0)
        .await
        .map_err(|e| e.to_string())?;
    let mut got = 0_u64;
    progress(got, total);
    while let Some(chunk) = response.chunk().await.map_err(super::download_error)? {
        got = got
            .checked_add(chunk.len() as u64)
            .ok_or("Android update is too large.")?;
        if got > MAX_APK_BYTES {
            return Err("Android update is too large.".into());
        }
        verifier.update(&chunk);
        file.write_all(&chunk).await.map_err(|e| e.to_string())?;
        progress(got, total);
    }
    verifier
        .finalize()
        .map_err(|e| format!("Update signature verification failed: {e}"))?;
    file.sync_all().await.map_err(|e| e.to_string())?;
    drop(file);
    tokio::fs::rename(&partial.0, &path)
        .await
        .map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(target_os = "android")]
pub(super) async fn install(path: &Path, tag: &str) -> Result<bool, String> {
    let version = crate::version::release_number(tag)
        .filter(|v| *v <= 2_100_000_000)
        .ok_or("Invalid Android update version.")?;
    crate::android::install_update(path, version).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const DATA: &[u8] = b"APK download test fixture";
    const KEY: &str = "RWR1cGRhdGVzdAOhB7/zzhC+HXDdGOdLwJln5NYwm6UNXx3chmQSVTG4";
    const SIGNATURE: &str = "untrusted comment: disposable test fixture\nRUR1cGRhdGVzdNtV8uCHWxm/L7B3uB3hJRjWwMh3G7B5tMD5YMQ527YWBPkZVnG/53mSmzHfRqV8sHUy8ork+rPbxvODODoc+wo=\ntrusted comment: discordia v0.1.0-pre.999 Discordia-android-arm64.apk\n1AW5xx7yJQWtcEhT52SE5hGkpfxlIezIFuIjZ6ZAW6orTyf6vSCccSxi5KGQIdhsUPCoHsm09fqLadURclcrCg==";

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("discordia-update-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn files(&self) -> Vec<PathBuf> {
            std::fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect()
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            for file in self.files() {
                let _ = std::fs::remove_file(file);
            }
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    async fn response(data: &[u8], declared: u64) -> reqwest::Response {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let body = data.to_vec();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            let _ = socket.write_all(&body).await;
        });
        reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .get(format!("http://{address}/update"))
            .send()
            .await
            .unwrap()
    }

    async fn stalled_response(
        idle: Duration,
    ) -> (
        reqwest::Response,
        tokio::sync::oneshot::Sender<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (release, released) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", DATA.len()).as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&DATA[..4]).await.unwrap();
            let _ = released.await;
        });
        let response = super::super::download_client(idle)
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/update"))
            .send()
            .await
            .unwrap();
        (response, release, worker)
    }

    #[tokio::test]
    async fn a_stalled_partial_download_times_out_cleans_up_and_can_retry() {
        let directory = Directory::new();
        let key = minisign_verify::PublicKey::from_base64(KEY).unwrap();
        let signature = minisign_verify::Signature::decode(SIGNATURE).unwrap();
        let (stalled, release, worker) = stalled_response(Duration::from_millis(500)).await;
        let mut last = 0;
        let error = download_response(stalled, directory.0.clone(), &key, &signature, |got, _| {
            last = got
        })
        .await
        .unwrap_err();
        assert_eq!(last, 4, "the connection must stall after real progress");
        assert!(error.contains("stopped receiving data"), "{error}");
        assert!(directory.files().is_empty());
        release.send(()).unwrap();
        worker.await.unwrap();
        let file = download_response(
            response(DATA, DATA.len() as u64).await,
            directory.0.clone(),
            &key,
            &signature,
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(file).unwrap(), DATA);
    }

    #[tokio::test]
    async fn cancelling_a_partial_download_removes_the_unverified_file() {
        let directory = Directory::new();
        let key = minisign_verify::PublicKey::from_base64(KEY).unwrap();
        let signature = minisign_verify::Signature::decode(SIGNATURE).unwrap();
        let (response, release, worker) = stalled_response(Duration::from_secs(10)).await;
        let (progress, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        {
            let mut download = Box::pin(download_response(
                response,
                directory.0.clone(),
                &key,
                &signature,
                move |got, _| {
                    progress.send(got).unwrap();
                },
            ));
            loop {
                tokio::select! {
                    result = &mut download => panic!("download must remain pending: {result:?}"),
                    got = receiver.recv() => if got == Some(4) { break; },
                }
            }
        }
        assert!(directory.files().is_empty());
        release.send(()).unwrap();
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn verified_android_download_is_saved_and_tampering_keeps_the_previous_file() {
        let directory = Directory::new();
        std::fs::write(directory.0.join("orphan.partial"), b"interrupted download").unwrap();
        let key = minisign_verify::PublicKey::from_base64(KEY).unwrap();
        let signature = minisign_verify::Signature::decode(SIGNATURE).unwrap();
        let mut progress = Vec::new();
        let file = download_response(
            response(DATA, DATA.len() as u64).await,
            directory.0.clone(),
            &key,
            &signature,
            |got, total| progress.push((got, total)),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), DATA);
        assert_eq!(
            progress.last(),
            Some(&(DATA.len() as u64, Some(DATA.len() as u64)))
        );
        let error = download_response(
            response(b"tampered", 8).await,
            directory.0.clone(),
            &key,
            &signature,
            |_, _| {},
        )
        .await
        .unwrap_err();
        assert!(error.contains("verification failed"), "{error}");
        assert_eq!(std::fs::read(&file).unwrap(), DATA);
        assert_eq!(directory.files(), vec![file]);
    }

    #[tokio::test]
    async fn oversized_and_truncated_downloads_leave_no_installable_file() {
        let key = minisign_verify::PublicKey::from_base64(KEY).unwrap();
        let signature = minisign_verify::Signature::decode(SIGNATURE).unwrap();
        for (body, declared) in [
            (b"".as_slice(), MAX_APK_BYTES + 1),
            (DATA, DATA.len() as u64 + 10),
        ] {
            let directory = Directory::new();
            assert!(
                download_response(
                    response(body, declared).await,
                    directory.0.clone(),
                    &key,
                    &signature,
                    |_, _| {}
                )
                .await
                .is_err()
            );
            assert!(directory.files().is_empty());
        }
    }

    #[tokio::test]
    async fn another_platform_cannot_enter_the_android_downloader() {
        assert!(
            fetch_verified(
                "v0.1.0-pre.999",
                "Discordia-windows-setup.exe",
                "invalid",
                "invalid",
                |_, _| {}
            )
            .await
            .unwrap_err()
            .contains("Unexpected Android update asset")
        );
    }
}
