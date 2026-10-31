use base64::Engine;
use dioxus::prelude::*;
use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Weak},
};

type DecodedBytes = Arc<(&'static str, Vec<u8>)>;

struct AssetEntry {
    owner: Weak<String>,
    decoded: Option<DecodedBytes>,
}

#[derive(Clone, Default)]
struct Assets(Arc<parking_lot::Mutex<HashMap<String, AssetEntry>>>);

#[derive(Clone)]
pub(crate) struct ImageSource {
    uri: Arc<String>,
    _data: Arc<String>,
}

impl PartialEq for ImageSource {
    fn eq(&self, other: &Self) -> bool {
        self.uri == other.uri
    }
}

impl fmt::Display for ImageSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.uri.fmt(f)
    }
}

fn payload(data: &str, max_bytes: usize) -> Option<(&'static str, &'static str, &str)> {
    let (header, encoded) = data.split_once(";base64,")?;
    let (mime, ext) = match header {
        "data:image/png" => ("image/png", "png"),
        "data:image/jpeg" => ("image/jpeg", "jpg"),
        "data:image/gif" => ("image/gif", "gif"),
        "data:image/webp" => ("image/webp", "webp"),
        "data:image/avif" => ("image/avif", "avif"),
        _ => return None,
    };
    (encoded.len() <= max_bytes.div_ceil(3) * 4).then_some((mime, ext, encoded))
}

impl Assets {
    fn source(&self, data: Arc<String>) -> ImageSource {
        self.source_with_limit(data, crate::protocol::MAX_IMAGE_BYTES)
    }

    fn source_with_limit(&self, data: Arc<String>, limit: usize) -> ImageSource {
        let Some((_, ext, _)) = payload(&data, limit) else {
            return ImageSource {
                uri: data.clone(),
                _data: data,
            };
        };
        let mut entries = self.0.lock();
        entries.retain(|_, entry| entry.owner.strong_count() > 0);
        if let Some((key, shared)) = entries.iter().find_map(|(key, entry)| {
            let shared = entry.owner.upgrade()?;
            (shared.as_ref() == data.as_ref()).then_some((key.clone(), shared))
        }) {
            return ImageSource {
                uri: Arc::new(format!("/image-data/{key}")),
                _data: shared,
            };
        }
        let key = format!("{}.{ext}", uuid::Uuid::new_v4());
        entries.insert(
            key.clone(),
            AssetEntry {
                owner: Arc::downgrade(&data),
                decoded: None,
            },
        );
        ImageSource {
            uri: Arc::new(format!("/image-data/{key}")),
            _data: data,
        }
    }

    #[cfg(test)]
    fn resolve(&self, key: &str) -> Option<Arc<String>> {
        self.0.lock().get(key)?.owner.upgrade()
    }

    fn resolve_entry(&self, key: &str) -> Option<(Arc<String>, Option<DecodedBytes>)> {
        let entries = self.0.lock();
        let entry = entries.get(key)?;
        let owner = entry.owner.upgrade()?;
        Some((owner, entry.decoded.clone()))
    }

    fn store_decoded(&self, key: &str, decoded: DecodedBytes) {
        let mut entries = self.0.lock();
        if let Some(entry) = entries.get_mut(key) {
            entry.decoded = Some(decoded);
        }
    }
}

pub(crate) fn source(data: Arc<String>) -> ImageSource {
    match try_consume_context::<Assets>() {
        Some(assets) => assets.source(data),
        None => ImageSource {
            uri: data.clone(),
            _data: data,
        },
    }
}

pub(crate) fn chat_source(data: Arc<String>) -> ImageSource {
    match try_consume_context::<Assets>() {
        Some(assets) => assets.source_with_limit(data, crate::chat_image::MAX_DM_BYTES),
        None => ImageSource {
            uri: data.clone(),
            _data: data,
        },
    }
}

pub(crate) fn use_image_assets() {
    let assets = use_hook(|| dioxus::core::provide_root_context(Assets::default()));
    let assets_handler = assets.clone();
    crate::platform::use_asset_handler("image-data", move |request, responder| {
        let path = request
            .uri()
            .path()
            .strip_prefix("/image-data/")
            .unwrap_or("")
            .to_string();
        let resolved = assets_handler.resolve_entry(&path);
        let assets_task = assets_handler.clone();
        tokio::task::spawn_blocking(move || {
            use crate::platform::wry::http::{
                Response, StatusCode,
                header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue},
            };
            let decoded: Option<DecodedBytes> = match resolved {
                Some((_, Some(cached))) => Some(cached),
                Some((data, None)) => {
                    let decoded = (|| {
                        let (mime, _, encoded) = payload(&data, crate::chat_image::MAX_DM_BYTES)?;
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(encoded)
                            .ok()?;
                        (bytes.len() <= crate::chat_image::MAX_DM_BYTES)
                            .then_some(Arc::new((mime, bytes)))
                    })();
                    if let Some(ref d) = decoded {
                        assets_task.store_decoded(&path, Arc::clone(d));
                    }
                    decoded
                }
                None => None,
            };
            let mut response = match decoded {
                Some(payload) => {
                    let mut response = Response::new(payload.1.clone());
                    response
                        .headers_mut()
                        .insert(CONTENT_TYPE, HeaderValue::from_static(payload.0));
                    response
                        .headers_mut()
                        .insert(CACHE_CONTROL, HeaderValue::from_static("private, max-age=300"));
                    response
                }
                None => {
                    let mut response = Response::new(Vec::new());
                    *response.status_mut() = StatusCode::NOT_FOUND;
                    response
                }
            };
            response.headers_mut().insert(
                "x-content-type-options",
                HeaderValue::from_static("nosniff"),
            );
            responder.respond(response);
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_images_share_a_short_url_and_keep_their_original_bytes() {
        let assets = Assets::default();
        let bytes = Arc::new("data:image/gif;base64,R0lGODlh".to_owned());
        let first = assets.source(bytes.clone());
        let second = assets.source(Arc::new(bytes.as_ref().clone()));
        assert_eq!(first.to_string(), second.to_string());
        assert!(first.to_string().ends_with(".gif"));
        assert!(first.to_string().len() < 100);
        let key = first.uri.strip_prefix("/image-data/").unwrap().to_string();
        assert!(Arc::ptr_eq(&assets.resolve(&key).unwrap(), &bytes));
        drop(first);
        drop(bytes);
        assert_eq!(
            assets.resolve(&key).unwrap().as_str(),
            second._data.as_str()
        );
    }

    #[test]
    fn registered_images_are_released_with_their_last_view() {
        let assets = Assets::default();
        let image = assets.source(Arc::new("data:image/png;base64,aGk=".into()));
        let key = image.uri.strip_prefix("/image-data/").unwrap().to_string();
        assert!(assets.resolve(&key).is_some());
        drop(image);
        assert!(assets.resolve(&key).is_none());
        assert!(assets.resolve("../../profile.json").is_none());
    }

    #[test]
    fn chat_images_can_use_the_larger_upload_budget_without_expanding_artwork() {
        let assets = Assets::default();
        let data = Arc::new(format!(
            "data:image/png;base64,{}",
            "A".repeat(crate::protocol::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4)
        ));
        assert!(assets.source(data.clone()).uri.starts_with("data:"));
        let chat = assets.source_with_limit(data, crate::protocol::MAX_GUILD_UPLOAD_BYTES);
        assert!(chat.uri.starts_with("/image-data/"));
    }

    #[test]
    fn unsupported_or_oversized_payloads_are_not_registered() {
        let assets = Assets::default();
        for data in [
            "https://example.com/avatar.png".to_owned(),
            "data:image/svg+xml;base64,PHN2Zz4=".into(),
            format!(
                "data:image/png;base64,{}",
                "A".repeat(crate::protocol::MAX_IMAGE_BYTES.div_ceil(3) * 4 + 1)
            ),
        ] {
            let image = assets.source(Arc::new(data.clone()));
            assert_eq!(image.to_string(), data);
        }
        assert!(assets.0.lock().is_empty());
    }
}
