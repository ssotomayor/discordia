use base64::Engine as _;
use discordia_server::{
    media::{DEFAULT_MAX_BYTES, MediaStore},
    protocol::{ClientMessage, ImagePreviewBlob, ServerMessage},
};
use std::{collections::HashSet, path::PathBuf, time::Duration};

fn directory() -> PathBuf {
    std::env::temp_dir().join(format!("discordia-image-previews-{}", uuid::Uuid::new_v4()))
}
fn photo(seed: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(1600, 1200, |x, y| {
        let n = x
            .wrapping_mul(374761393)
            .wrapping_add(y.wrapping_mul(668265263))
            .wrapping_add(seed);
        let n = (n ^ (n >> 13)).wrapping_mul(1274126177);
        image::Rgb([
            (n % 256) as u8,
            ((n >> 8) % 256) as u8,
            ((n >> 16) % 256) as u8,
        ])
    });
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 92)
        .encode_image(&image)
        .unwrap();
    bytes
}
fn data(bytes: &[u8]) -> String {
    format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}
fn bytes(url: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(url.split_once(',').unwrap().1)
        .unwrap()
}

#[test]
fn cached_previews_survive_restart_and_share_the_existing_quota() {
    let dir = directory();
    let media = MediaStore::open(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
    let input = photo(1);
    let address = media.store_chat_image(&data(&input), 10_000_000).unwrap();
    let before = media.used_bytes();
    let (preview, original) = media.preview(&address, 8 * 1024 * 1024).unwrap();
    assert!(!original);
    assert_eq!(
        image::load_from_memory(&bytes(&preview)).unwrap().width(),
        640
    );
    assert_eq!(bytes(&media.inline(&address).unwrap()), input);
    let used = media.used_bytes();
    assert_eq!(used, before + bytes(&preview).len() as u64);
    assert_eq!(
        media.preview(&address, 8 * 1024 * 1024),
        Some((preview.clone(), false))
    );
    assert_eq!(media.used_bytes(), used);
    let reopened = MediaStore::open(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
    assert_eq!(reopened.used_bytes(), used);
    assert_eq!(
        reopened.preview(&address, 8 * 1024 * 1024),
        Some((preview, false))
    );
    let references = HashSet::from([address.strip_prefix("media:").unwrap().to_owned()]);
    assert_eq!(reopened.sweep(&references, Duration::ZERO).deleted, 0);
    assert_eq!(reopened.sweep(&HashSet::new(), Duration::ZERO).deleted, 2);
    assert_eq!(reopened.used_bytes(), 0);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn previews_are_evicted_before_rejecting_an_original_upload() {
    let a = photo(2);
    let b = photo(3);
    let p = discordia_image_policy::create(&a)
        .unwrap()
        .unwrap()
        .bytes
        .len() as u64;
    let limit = (a.len() + b.len()) as u64 + p / 2;
    assert!(p < limit / 8);
    let dir = directory();
    let media = MediaStore::open(dir.clone(), limit).unwrap();
    let original = media.store_chat_image(&data(&a), 10_000_000).unwrap();
    media.preview(&original, 8 * 1024 * 1024).unwrap();
    assert!(media.used_bytes() > a.len() as u64);
    let second = media.store_chat_image(&data(&b), 10_000_000).unwrap();
    assert_eq!(media.used_bytes(), (a.len() + b.len()) as u64);
    assert_eq!(bytes(&media.inline(&original).unwrap()), a);
    assert_eq!(bytes(&media.inline(&second).unwrap()), b);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn preview_names_do_not_admit_files_paths_or_case_aliases_and_obey_response_budget() {
    let dir = directory();
    let media = MediaStore::open(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
    let address = media
        .store_chat_image(&data(&photo(4)), 10_000_000)
        .unwrap();
    for invalid in [
        "media:../outside.jpg".to_owned(),
        address.to_uppercase(),
        format!("media:{}.bin", "a".repeat(64)),
        format!("media:{}.dxa", "a".repeat(64)),
    ] {
        assert!(media.preview(&invalid, usize::MAX).is_none());
    }
    assert!(media.preview(&address, 1).is_none());
    let (url, _) = media.preview(&address, 8 * 1024 * 1024).unwrap();
    assert!(media.preview(&address, url.len() - 1).is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn gateway_negotiates_previews_and_still_serves_the_unchanged_original() {
    use discordia_bot::{Bot, BotIdentity};
    let dir = directory();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = discordia_server::ServerConfig {
        livekit: discordia_server::livekit::LiveKitConfig::from_env(&dir),
        operators: Default::default(),
        identities: discordia_server::local_identities(addr.port()),
        media_max_bytes: DEFAULT_MAX_BYTES,
        data_dir: dir.clone(),
        admission: Default::default(),
    };
    let media = MediaStore::open(dir.join("media"), DEFAULT_MAX_BYTES).unwrap();
    let input = photo(5);
    let address = media.store_chat_image(&data(&input), 10_000_000).unwrap();
    let name = address.strip_prefix("media:").unwrap().to_owned();
    let mut animated = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut animated);
        for value in [0, 255] {
            encoder
                .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                    16,
                    16,
                    image::Rgba([value, 80, 40, 255]),
                )))
                .unwrap();
        }
    }
    animated.resize(7 * 1024 * 1024, 0);
    let animation = media
        .store_chat_image(
            &format!(
                "data:image/gif;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(&animated)
            ),
            10_000_000,
        )
        .unwrap();
    let (router, shutdown) = discordia_server::build_gateway(cfg).await.unwrap();
    let host = discordia_server::serve_router(listener, router);
    let mut bot = Bot::connect_as_user(
        &format!("ws://{addr}/gateway"),
        &BotIdentity::generate(),
        "Images",
    )
    .await
    .unwrap();
    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::Ready { image_previews, .. } = bot.next_event().await.unwrap() {
                break image_previews;
            }
        }
    })
    .await
    .unwrap();
    assert!(ready);
    bot.send(&ClientMessage::FetchImagePreviews {
        images: vec![name.clone(), name.clone()],
    })
    .await
    .unwrap();
    let blobs: Vec<ImagePreviewBlob> = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::ImagePreviews { blobs } = bot.next_event().await.unwrap() {
                break blobs;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(blobs.len(), 1);
    assert_eq!(blobs[0].image, name);
    assert!(!blobs[0].original);
    assert_eq!(
        image::load_from_memory(&bytes(&blobs[0].data_url))
            .unwrap()
            .width(),
        640
    );
    bot.send(&ClientMessage::FetchEmoji { images: vec![name] })
        .await
        .unwrap();
    let returned = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::EmojiBlobs { blobs } = bot.next_event().await.unwrap() {
                break blobs;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(bytes(&returned[0].data_url), input);
    bot.send(&ClientMessage::FetchImagePreviews {
        images: vec![animation.strip_prefix("media:").unwrap().into()],
    })
    .await
    .unwrap();
    let returned = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerMessage::ImagePreviews { blobs } = bot.next_event().await.unwrap() {
                break blobs;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(returned.len(), 1);
    assert!(returned[0].original);
    assert_eq!(bytes(&returned[0].data_url), animated);
    shutdown.close_all();
    drop(bot);
    tokio::time::sleep(Duration::from_millis(100)).await;
    host.abort();
}

#[test]
#[ignore = "requires the generated Windows image benchmark corpus"]
fn measure_cold_and_cached_server_preview() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let out = root.join("target/unified-images");
    let run = std::env::var("IMAGE_POLICY_BENCH_RUN")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    assert!((1..=5).contains(&run));
    let input = std::fs::read(out.join("fixtures/jpeg-large/server-original/high0.jpg")).unwrap();
    let dir = directory();
    let media = MediaStore::open(dir.clone(), DEFAULT_MAX_BYTES).unwrap();
    let address = media.store_chat_image(&data(&input), 10_000_000).unwrap();
    let begin = std::time::Instant::now();
    let cold = media.preview(&address, 16 * 1024 * 1024).unwrap();
    let cold_ms = begin.elapsed().as_secs_f64() * 1000.;
    let begin = std::time::Instant::now();
    let cached = media.preview(&address, 16 * 1024 * 1024).unwrap();
    let cached_ms = begin.elapsed().as_secs_f64() * 1000.;
    assert_eq!(cold, cached);
    assert!(!cold.1);
    assert_eq!(bytes(&media.inline(&address).unwrap()), input);
    let row = serde_json::json!({"run":run,"coldMs":cold_ms,"cachedMs":cached_ms,"usedBytes":media.used_bytes(),"originalBytes":input.len(),"previewBytes":bytes(&cold.0).len(),"scope":"MediaStore::preview wall time; excludes upload, network, socket and semaphore waiting"});
    std::fs::write(
        out.join(format!("server-timing-{run}.json")),
        serde_json::to_vec_pretty(&row).unwrap(),
    )
    .unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}
