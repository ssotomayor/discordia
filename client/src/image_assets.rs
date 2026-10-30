use base64::Engine;
use dioxus::prelude::*;
use std::{
    cell::RefCell,
    collections::HashMap,
    fmt,
    rc::Rc,
    sync::{Arc, Weak},
};

#[derive(Clone, Default)]
struct Assets(Rc<RefCell<HashMap<String, Weak<String>>>>);

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
        let mut entries = self.0.borrow_mut();
        entries.retain(|_, owner| owner.strong_count() > 0);
        if let Some((key, shared)) = entries.iter().find_map(|(key, owner)| {
            let shared = owner.upgrade()?;
            (shared.as_ref() == data.as_ref()).then_some((key, shared))
        }) {
            return ImageSource {
                uri: Arc::new(format!("/image-data/{key}")),
                _data: shared,
            };
        }
        let key = format!("{}.{ext}", uuid::Uuid::new_v4());
        entries.insert(key.clone(), Arc::downgrade(&data));
        ImageSource {
            uri: Arc::new(format!("/image-data/{key}")),
            _data: data,
        }
    }

    fn resolve(&self, key: &str) -> Option<Arc<String>> {
        self.0.borrow().get(key)?.upgrade()
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
    crate::platform::use_asset_handler("image-data", move |request, responder| {
        let data = request
            .uri()
            .path()
            .strip_prefix("/image-data/")
            .and_then(|key| assets.resolve(key));
        tokio::task::spawn_blocking(move || {
            use crate::platform::wry::http::{
                Response, StatusCode,
                header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue},
            };
            let decoded = data.as_deref().and_then(|data| {
                let (mime, _, encoded) = payload(data, crate::chat_image::MAX_DM_BYTES)?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .ok()?;
                (bytes.len() <= crate::chat_image::MAX_DM_BYTES).then_some((mime, bytes))
            });
            let mut response = match decoded {
                Some((mime, bytes)) => {
                    let mut response = Response::new(bytes);
                    response
                        .headers_mut()
                        .insert(CONTENT_TYPE, HeaderValue::from_static(mime));
                    response
                        .headers_mut()
                        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
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
        assert!(assets.0.borrow().is_empty());
    }
}
