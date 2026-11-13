use base64::Engine;
use dioxus::prelude::*;
use std::{
    collections::HashMap,
    fmt,
    hash::{Hash, Hasher},
    sync::{Arc, Weak},
};

type DecodedBytes = Arc<(&'static str, Vec<u8>)>;

struct ViewLease {
    assets: Weak<parking_lot::Mutex<AssetsInner>>,
    key: String,
}

impl Drop for ViewLease {
    fn drop(&mut self) {
        if let Some(assets) = self.assets.upgrade() {
            let mut inner = assets.lock();
            // A replacement view can acquire this URL while the finalizer waits.
            if let Some(entry) = inner.entries.get_mut(&self.key)
                && std::ptr::eq(entry.view.as_ptr(), self)
            {
                entry.decoded = None;
            }
        }
    }
}

struct AssetEntry {
    owner: Weak<String>,
    view: Weak<ViewLease>,
    hash: u64,
    decoded: Option<DecodedBytes>,
}

#[derive(Default)]
struct AssetsInner {
    entries: HashMap<String, AssetEntry>,
    by_hash: HashMap<u64, Vec<String>>,
    // The media cache hands out one `Arc` per blob; matching its address skips
    // hashing up to 350 KB per emoji on every render.
    by_ptr: HashMap<usize, String>,
    clean_counter: usize,
}

impl AssetsInner {
    fn purge_dead_entries(&mut self) {
        let mut dead = Vec::new();
        for (key, entry) in &self.entries {
            if entry.owner.strong_count() == 0 {
                dead.push((key.clone(), entry.hash));
            }
        }
        for (key, hash) in dead {
            self.entries.remove(&key);
            if let Some(keys) = self.by_hash.get_mut(&hash) {
                keys.retain(|k| k != &key);
                if keys.is_empty() {
                    self.by_hash.remove(&hash);
                }
            }
        }
        let entries = &self.entries;
        self.by_ptr.retain(|_, key| entries.contains_key(key));
    }
}

#[derive(Clone, Default)]
struct Assets(Arc<parking_lot::Mutex<AssetsInner>>);

#[derive(Clone)]
pub(crate) struct ImageSource {
    uri: Arc<String>,
    _data: Arc<String>,
    _view: Option<Arc<ViewLease>>,
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

fn hash_content(data: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
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
        if payload(&data, limit).is_some()
            && let Some(found) = self.same_allocation(&data)
        {
            return found;
        }
        let hash = hash_content(&data);
        self.source_with_hash(data, limit, hash)
    }

    fn source_with_hash(&self, data: Arc<String>, limit: usize, hash: u64) -> ImageSource {
        let Some((_, ext, _)) = payload(&data, limit) else {
            return ImageSource {
                uri: data.clone(),
                _data: data,
                _view: None,
            };
        };
        let mut inner = self.0.lock();
        inner.clean_counter += 1;
        if inner.clean_counter >= 64 {
            inner.clean_counter = 0;
            inner.purge_dead_entries();
        }
        let existing = inner.by_hash.get(&hash).and_then(|keys| {
            keys.iter().find_map(|key| {
                inner
                    .entries
                    .get(key)?
                    .owner
                    .upgrade()
                    .filter(|shared| shared.as_ref() == data.as_ref())
                    .map(|shared| (key.clone(), shared))
            })
        });
        if let Some((key, shared)) = existing
            && let Some(entry) = inner.entries.get_mut(&key)
        {
            let view = entry.view.upgrade().unwrap_or_else(|| {
                let view = Arc::new(ViewLease {
                    assets: Arc::downgrade(&self.0),
                    key: key.clone(),
                });
                entry.view = Arc::downgrade(&view);
                view
            });
            return ImageSource {
                uri: Arc::new(format!("/image-data/{key}")),
                _data: shared,
                _view: Some(view),
            };
        }
        let key = format!("{}.{ext}", uuid::Uuid::new_v4());
        let view = Arc::new(ViewLease {
            assets: Arc::downgrade(&self.0),
            key: key.clone(),
        });
        inner.entries.insert(
            key.clone(),
            AssetEntry {
                owner: Arc::downgrade(&data),
                view: Arc::downgrade(&view),
                hash,
                decoded: None,
            },
        );
        inner.by_hash.entry(hash).or_default().push(key.clone());
        inner
            .by_ptr
            .insert(Arc::as_ptr(&data) as usize, key.clone());
        ImageSource {
            uri: Arc::new(format!("/image-data/{key}")),
            _data: data,
            _view: Some(view),
        }
    }

    fn same_allocation(&self, data: &Arc<String>) -> Option<ImageSource> {
        let mut inner = self.0.lock();
        let key = inner.by_ptr.get(&(Arc::as_ptr(data) as usize))?.clone();
        let entry = inner.entries.get_mut(&key)?;
        // The entry's `Weak` pins the allocation, so a matching live pointer is this blob.
        let shared = entry.owner.upgrade().filter(|o| Arc::ptr_eq(o, data))?;
        let view = entry.view.upgrade().unwrap_or_else(|| {
            let view = Arc::new(ViewLease {
                assets: Arc::downgrade(&self.0),
                key: key.clone(),
            });
            entry.view = Arc::downgrade(&view);
            view
        });
        Some(ImageSource {
            uri: Arc::new(format!("/image-data/{key}")),
            _data: shared,
            _view: Some(view),
        })
    }

    #[cfg(test)]
    fn resolve(&self, key: &str) -> Option<Arc<String>> {
        self.0.lock().entries.get(key)?.owner.upgrade()
    }

    fn resolve_entry(&self, key: &str) -> Option<(Arc<String>, Option<DecodedBytes>)> {
        let inner = self.0.lock();
        let entry = inner.entries.get(key)?;
        let owner = entry.owner.upgrade()?;
        Some((owner, entry.decoded.clone()))
    }

    fn store_decoded(&self, key: &str, decoded: DecodedBytes) {
        let mut inner = self.0.lock();
        if let Some(entry) = inner.entries.get_mut(key)
            && entry.view.strong_count() > 0
        {
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
            _view: None,
        },
    }
}

pub(crate) fn chat_source(data: Arc<String>) -> ImageSource {
    match try_consume_context::<Assets>() {
        Some(assets) => assets.source_with_limit(data, crate::chat_image::MAX_DM_BYTES),
        None => ImageSource {
            uri: data.clone(),
            _data: data,
            _view: None,
        },
    }
}

/// A cached blob's page path, never its bytes. The cache's `Arc` keeps the path
/// resolvable after this render; a fresh `Arc` would not outlive it.
pub(crate) fn cached_uri(cache: &crate::media_cache::MediaCache, address: &str) -> Option<String> {
    let data = cache.get_shared(address).filter(|d| !d.is_empty())?;
    Some(source(data).to_string())
}

/// `AppState::media_src` for a page attribute.
pub(crate) fn media_uri(state: &crate::state::AppState, raw: &str) -> Option<String> {
    match raw.strip_prefix("media:") {
        Some(address) => cached_uri(&state.emoji_images, address),
        None => Some(raw.to_owned()),
    }
}

impl ImageSource {
    pub(crate) fn data(&self) -> Arc<String> {
        Arc::clone(&self._data)
    }
}

pub(crate) fn use_image_assets() {
    use_image_density();
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
                    response.headers_mut().insert(
                        CACHE_CONTROL,
                        HeaderValue::from_static("private, max-age=300"),
                    );
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

#[derive(Clone, Copy)]
struct ImageDensity(Signal<Option<bool>>);

pub(crate) fn preview_density() -> Option<bool> {
    try_consume_context::<ImageDensity>()
        .map(|density| (density.0)())
        .unwrap_or(Some(true))
}

fn use_image_density() {
    let mut density = use_hook(|| {
        dioxus::core::provide_root_context(ImageDensity(Signal::new_in_scope(None, ScopeId::ROOT)))
    })
    .0;
    use_effect(move || {
        spawn(async move {
            let mut eval = document::eval(
                r#"return (async()=>{
                if(window.__dxfImageDensityOff)window.__dxfImageDensityOff();
                const query=matchMedia('(max-resolution: 2dppx)');
                let last;const report=()=>{const value=devicePixelRatio<=2;if(value!==last){last=value;dioxus.send(value);}};
                if(query.addEventListener)query.addEventListener('change',report);else query.addListener(report);
                window.addEventListener('resize',report);report();
                let off;
                await new Promise(resolve=>{off=()=>{
                    if(query.removeEventListener)query.removeEventListener('change',report);else query.removeListener(report);resolve();
                    window.removeEventListener('resize',report);
                };window.__dxfImageDensityOff=off;});
                if(window.__dxfImageDensityOff===off)delete window.__dxfImageDensityOff;
            })()"#,
            );
            while let Ok(value) = eval.recv::<bool>().await {
                density.set(Some(value));
            }
            density.set(Some(false));
        });
    });
    use_drop(|| {
        document::eval("window.__dxfImageDensityOff && window.__dxfImageDensityOff()");
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
    fn the_same_cached_blob_keeps_its_url_after_its_views_close() {
        let assets = Assets::default();
        let cached = Arc::new("data:image/webp;base64,UklGRg==".to_owned());
        let uri = assets.source(cached.clone()).to_string();
        assert_eq!(assets.source(cached.clone()).to_string(), uri);
        let key = uri.strip_prefix("/image-data/").unwrap();
        assert!(Arc::ptr_eq(&assets.resolve(key).unwrap(), &cached));
        drop(cached);
        let fresh = Arc::new("data:image/webp;base64,UklGRg==".to_owned());
        assert_ne!(assets.source(fresh).to_string(), uri);
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
    fn last_view_releases_binary_cache_without_changing_a_live_owners_url() {
        let assets = Assets::default();
        let cached = Arc::new("data:image/png;base64,aGk=".to_owned());
        let first = assets.source(cached.clone());
        let second = first.clone();
        let key = first.uri.strip_prefix("/image-data/").unwrap().to_owned();
        assets.store_decoded(&key, Arc::new(("image/png", vec![42; 1024])));
        drop(first);
        assert!(assets.resolve_entry(&key).unwrap().1.is_some());
        let uri = second.to_string();
        drop(second);
        assert!(assets.resolve_entry(&key).unwrap().1.is_none());
        let reopened = assets.source(cached);
        assert_eq!(reopened.to_string(), uri);
        assets.store_decoded(&key, Arc::new(("image/png", vec![7])));
        assert_eq!(assets.resolve_entry(&key).unwrap().1.unwrap().1, [7]);
    }

    #[test]
    fn late_responses_do_not_retain_binary_bytes_after_the_view_closes() {
        let assets = Assets::default();
        let cached = Arc::new("data:image/gif;base64,R0lGODlh".to_owned());
        let source = assets.source(cached.clone());
        let key = source.uri.strip_prefix("/image-data/").unwrap().to_owned();
        drop(source);
        assets.store_decoded(&key, Arc::new(("image/gif", vec![1, 2, 3])));
        assert!(assets.resolve_entry(&key).unwrap().1.is_none());
        let source = assets.source(cached);
        let bytes = Arc::new(("image/gif", vec![1, 2, 3]));
        let weak = Arc::downgrade(&bytes);
        assets.store_decoded(&key, bytes);
        drop(source);
        assert!(weak.upgrade().is_none());
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
        assert!(assets.0.lock().entries.is_empty());
    }

    #[test]
    fn test_weak_cleanup_amortization() {
        let assets = Assets::default();
        {
            let mut temp_sources = Vec::new();
            for i in 0..128 {
                let img = Arc::new(format!("data:image/png;base64,{i:04}temp"));
                temp_sources.push(assets.source(img));
            }
            assert_eq!(assets.0.lock().entries.len(), 128);
        }

        // Trigger amortized purge by running queries
        for _ in 0..64 {
            let dummy = Arc::new("data:image/png;base64,dummy".to_string());
            let _ = assets.source(dummy);
        }

        let inner = assets.0.lock();
        // The 128 temporary entries must be purged
        assert!(inner.entries.len() <= 1);
        assert!(inner.by_hash.len() <= 1);
    }

    #[test]
    fn test_concurrent_image_registration() {
        let assets = Assets::default();
        let mut handles = Vec::new();
        for t in 0..4 {
            let a = assets.clone();
            handles.push(std::thread::spawn(move || {
                let mut kept = Vec::new();
                for i in 0..100 {
                    let img = Arc::new(format!(
                        "data:image/png;base64,{:02}abcd",
                        (t * 10 + i) % 15
                    ));
                    kept.push(a.source(img));
                }
                kept
            }));
        }
        let all_kept: Vec<_> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        assert_eq!(all_kept.len(), 400);
        assert_eq!(assets.0.lock().entries.len(), 15);
        assert_eq!(assets.0.lock().by_hash.len(), 15);

        drop(all_kept);
        assets.0.lock().purge_dead_entries();
        assert!(assets.0.lock().entries.is_empty());
        assert!(assets.0.lock().by_hash.is_empty());
    }

    #[test]
    fn test_hash_collision_exact_equality() {
        let assets = Assets::default();
        let forced_collision_hash = 0xDEAD_BEEF_CAFE_BABE_u64;
        let img1 = Arc::new("data:image/png;base64,image_payload_alpha_one".to_string());
        let img2 = Arc::new("data:image/png;base64,image_payload_beta_two".to_string());

        // Register both distinct payloads with the exact same forced hash
        let src1 = assets.source_with_hash(
            img1.clone(),
            crate::protocol::MAX_IMAGE_BYTES,
            forced_collision_hash,
        );
        let src2 = assets.source_with_hash(
            img2.clone(),
            crate::protocol::MAX_IMAGE_BYTES,
            forced_collision_hash,
        );

        let key1 = src1.uri.strip_prefix("/image-data/").unwrap().to_string();
        let key2 = src2.uri.strip_prefix("/image-data/").unwrap().to_string();

        // 1. Different payloads must receive different keys despite hash collision
        assert_ne!(src1.to_string(), src2.to_string());
        assert_ne!(key1, key2);

        // 2. Both keys must be stored in the same hash bucket
        assert_eq!(
            assets
                .0
                .lock()
                .by_hash
                .get(&forced_collision_hash)
                .unwrap()
                .len(),
            2
        );

        // 3. Resolving each key must return its exact original data
        assert_eq!(assets.resolve(&key1).unwrap().as_str(), img1.as_str());
        assert_eq!(assets.resolve(&key2).unwrap().as_str(), img2.as_str());

        // 4. Querying img1 again with the colliding hash returns src1 (not src2)
        let src1_again = assets.source_with_hash(
            img1.clone(),
            crate::protocol::MAX_IMAGE_BYTES,
            forced_collision_hash,
        );
        assert_eq!(src1_again.to_string(), src1.to_string());

        // 5. Querying img2 again with the colliding hash returns src2 (not src1)
        let src2_again = assets.source_with_hash(
            img2.clone(),
            crate::protocol::MAX_IMAGE_BYTES,
            forced_collision_hash,
        );
        assert_eq!(src2_again.to_string(), src2.to_string());
    }
}
