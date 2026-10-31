use base64::Engine;
use dioxus::prelude::*;
use std::{
    collections::HashMap,
    fmt,
    hash::{Hash, Hasher},
    sync::{Arc, Weak},
};

type DecodedBytes = Arc<(&'static str, Vec<u8>)>;

struct AssetEntry {
    owner: Weak<String>,
    hash: u64,
    decoded: Option<DecodedBytes>,
}

#[derive(Default)]
struct AssetsInner {
    entries: HashMap<String, AssetEntry>,
    by_hash: HashMap<u64, Vec<String>>,
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
    }
}

#[derive(Clone, Default)]
struct Assets(Arc<parking_lot::Mutex<AssetsInner>>);

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
        let hash = hash_content(&data);
        self.source_with_hash(data, limit, hash)
    }

    fn source_with_hash(&self, data: Arc<String>, limit: usize, hash: u64) -> ImageSource {
        let Some((_, ext, _)) = payload(&data, limit) else {
            return ImageSource {
                uri: data.clone(),
                _data: data,
            };
        };
        let mut inner = self.0.lock();
        inner.clean_counter += 1;
        if inner.clean_counter >= 64 {
            inner.clean_counter = 0;
            inner.purge_dead_entries();
        }
        if let Some(keys) = inner.by_hash.get(&hash) {
            for key in keys {
                if let Some(shared) = inner
                    .entries
                    .get(key)
                    .and_then(|entry| entry.owner.upgrade())
                    .filter(|shared| shared.as_ref() == data.as_ref())
                {
                    return ImageSource {
                        uri: Arc::new(format!("/image-data/{key}")),
                        _data: shared,
                    };
                }
            }
        }
        let key = format!("{}.{ext}", uuid::Uuid::new_v4());
        inner.entries.insert(
            key.clone(),
            AssetEntry {
                owner: Arc::downgrade(&data),
                hash,
                decoded: None,
            },
        );
        inner.by_hash.entry(hash).or_default().push(key.clone());
        ImageSource {
            uri: Arc::new(format!("/image-data/{key}")),
            _data: data,
        }
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
        if let Some(entry) = inner.entries.get_mut(key) {
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
        assert!(assets.0.lock().entries.is_empty());
    }

    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    struct ScenarioConfig {
        n_distinct: usize,
        payload_kb: usize,
        hit_ratio_pct: usize, // e.g. 100, 90, 50
        n_threads: usize,
        total_ops: usize,
    }

    #[allow(dead_code)]
    struct ScenarioResult {
        total_time_ms: f64,
        lock_us_per_op: f64,
        throughput_ops_sec: f64,
    }

    fn run_legacy_bench(cfg: ScenarioConfig, images: &[Arc<String>]) -> ScenarioResult {
        let legacy_entries = Arc::new(parking_lot::Mutex::new(
            HashMap::<String, Weak<String>>::new(),
        ));
        let lock_nanos = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let start = std::time::Instant::now();
        let ops_per_thread = cfg.total_ops / cfg.n_threads;

        let mut handles = Vec::new();
        for t in 0..cfg.n_threads {
            let entries = legacy_entries.clone();
            let lock_dur = lock_nanos.clone();
            let images = images.to_vec();
            handles.push(std::thread::spawn(move || {
                let mut kept = Vec::with_capacity(ops_per_thread);
                for i in 0..ops_per_thread {
                    let is_hit = (i % 100) < cfg.hit_ratio_pct;
                    let img = if is_hit && !images.is_empty() {
                        images[(t * ops_per_thread + i) % images.len()].clone()
                    } else {
                        Arc::new(format!(
                            "data:image/png;base64,{t}_{i}_unique_{}",
                            "X".repeat(cfg.payload_kb * 1024)
                        ))
                    };
                    let t0 = std::time::Instant::now();
                    let mut map = entries.lock();
                    map.retain(|_, owner| owner.strong_count() > 0);
                    let found = map.iter().find_map(|(k, owner)| {
                        let shared = owner.upgrade()?;
                        (shared.as_ref() == img.as_ref()).then_some((k.clone(), shared))
                    });
                    let (key, _shared) = match found {
                        Some((k, s)) => (k, s),
                        None => {
                            let k = format!("{}.png", uuid::Uuid::new_v4());
                            map.insert(k.clone(), Arc::downgrade(&img));
                            (k, img.clone())
                        }
                    };
                    lock_dur.fetch_add(
                        t0.elapsed().as_nanos() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    drop(map);
                    kept.push(key);
                }
                kept
            }));
        }
        let all_keys: Vec<_> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let total = start.elapsed();
        let total_lock_ns = lock_nanos.load(std::sync::atomic::Ordering::Relaxed);
        ScenarioResult {
            total_time_ms: total.as_secs_f64() * 1000.0,
            lock_us_per_op: (total_lock_ns as f64 / 1000.0) / all_keys.len() as f64,
            throughput_ops_sec: all_keys.len() as f64 / total.as_secs_f64(),
        }
    }

    fn run_new_bench(cfg: ScenarioConfig, images: &[Arc<String>]) -> ScenarioResult {
        let assets = Assets::default();
        let lock_nanos = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let start = std::time::Instant::now();
        let ops_per_thread = cfg.total_ops / cfg.n_threads;

        let mut handles = Vec::new();
        for t in 0..cfg.n_threads {
            let assets = assets.clone();
            let lock_dur = lock_nanos.clone();
            let images = images.to_vec();
            handles.push(std::thread::spawn(move || {
                let mut kept = Vec::with_capacity(ops_per_thread);
                for i in 0..ops_per_thread {
                    let is_hit = (i % 100) < cfg.hit_ratio_pct;
                    let img = if is_hit && !images.is_empty() {
                        images[(t * ops_per_thread + i) % images.len()].clone()
                    } else {
                        Arc::new(format!(
                            "data:image/png;base64,{t}_{i}_unique_{}",
                            "X".repeat(cfg.payload_kb * 1024)
                        ))
                    };
                    let hash = hash_content(&img);
                    let t0 = std::time::Instant::now();
                    let src = assets.source_with_hash(img, crate::protocol::MAX_IMAGE_BYTES, hash);
                    lock_dur.fetch_add(
                        t0.elapsed().as_nanos() as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    kept.push(src);
                }
                kept
            }));
        }
        let all_keys: Vec<_> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let total = start.elapsed();
        let total_lock_ns = lock_nanos.load(std::sync::atomic::Ordering::Relaxed);
        ScenarioResult {
            total_time_ms: total.as_secs_f64() * 1000.0,
            lock_us_per_op: (total_lock_ns as f64 / 1000.0) / all_keys.len() as f64,
            throughput_ops_sec: all_keys.len() as f64 / total.as_secs_f64(),
        }
    }

    #[test]
    fn benchmark_image_assets_comprehensive_matrix() {
        eprintln!(
            "\n=========================================================================================="
        );
        eprintln!(
            " IMAGEASSETS DETAILED BENCHMARK MATRIX (Legacy Linear Scan vs New Hash Indexed)"
        );
        eprintln!(
            "=========================================================================================="
        );

        // 1. Varying N distinct images (1, 20, 100, 500, 2000) at 64 KB, 90% hits, 1 thread, 1000 ops
        eprintln!(
            "\n--- DIMENSION 1: Varying N Entries (Payload: 64 KB, Hits: 90%, Sequential, 1000 ops) ---"
        );
        eprintln!(
            "N_Entries | Legacy Total (ms) | Legacy Lock (µs) | New Total (ms) | New Lock (µs) | Lock Speedup | Total Speedup"
        );
        eprintln!(
            "----------+-------------------+------------------+----------------+---------------+--------------+--------------"
        );
        for &n in &[1, 20, 100, 500, 2000] {
            let payload_base = "A".repeat(64 * 1024);
            let images: Vec<Arc<String>> = (0..n)
                .map(|i| Arc::new(format!("data:image/png;base64,{i:06}_{payload_base}")))
                .collect();
            let cfg = ScenarioConfig {
                n_distinct: n,
                payload_kb: 64,
                hit_ratio_pct: 90,
                n_threads: 1,
                total_ops: 1000,
            };
            let leg = run_legacy_bench(cfg, &images);
            let nw = run_new_bench(cfg, &images);
            eprintln!(
                "{:<9} | {:<17.2} | {:<16.2} | {:<14.2} | {:<13.2} | {:<12.1}x | {:.2}x",
                n,
                leg.total_time_ms,
                leg.lock_us_per_op,
                nw.total_time_ms,
                nw.lock_us_per_op,
                leg.lock_us_per_op / nw.lock_us_per_op.max(0.001),
                leg.total_time_ms / nw.total_time_ms.max(0.001),
            );
        }

        // 2. Varying Payload size (1 KB, 64 KB, 500 KB) at N=100, 90% hits, 1 thread, 1000 ops
        eprintln!(
            "\n--- DIMENSION 2: Varying Payload Size (N=100, Hits: 90%, Sequential, 1000 ops) ---"
        );
        eprintln!(
            "Payload   | Legacy Total (ms) | Legacy Lock (µs) | New Total (ms) | New Lock (µs) | Lock Speedup | Total Speedup"
        );
        eprintln!(
            "----------+-------------------+------------------+----------------+---------------+--------------+--------------"
        );
        for &kb in &[1, 64, 500] {
            let payload_base = "B".repeat(kb * 1024);
            let images: Vec<Arc<String>> = (0..100)
                .map(|i| Arc::new(format!("data:image/png;base64,{i:04}_{payload_base}")))
                .collect();
            let cfg = ScenarioConfig {
                n_distinct: 100,
                payload_kb: kb,
                hit_ratio_pct: 90,
                n_threads: 1,
                total_ops: 1000,
            };
            let leg = run_legacy_bench(cfg, &images);
            let nw = run_new_bench(cfg, &images);
            eprintln!(
                "{:<9} | {:<17.2} | {:<16.2} | {:<14.2} | {:<13.2} | {:<12.1}x | {:.2}x",
                format!("{kb} KB"),
                leg.total_time_ms,
                leg.lock_us_per_op,
                nw.total_time_ms,
                nw.lock_us_per_op,
                leg.lock_us_per_op / nw.lock_us_per_op.max(0.001),
                leg.total_time_ms / nw.total_time_ms.max(0.001),
            );
        }

        // 3. Varying Hit Ratio (100%, 90%, 50%) at N=100, 64 KB, 1 thread, 1000 ops
        eprintln!(
            "\n--- DIMENSION 3: Varying Hit Ratio (N=100, Payload: 64 KB, Sequential, 1000 ops) ---"
        );
        eprintln!(
            "Hit Ratio | Legacy Total (ms) | Legacy Lock (µs) | New Total (ms) | New Lock (µs) | Lock Speedup | Total Speedup"
        );
        eprintln!(
            "----------+-------------------+------------------+----------------+---------------+--------------+--------------"
        );
        for &hits in &[100, 90, 50] {
            let payload_base = "C".repeat(64 * 1024);
            let images: Vec<Arc<String>> = (0..100)
                .map(|i| Arc::new(format!("data:image/png;base64,{i:04}_{payload_base}")))
                .collect();
            let cfg = ScenarioConfig {
                n_distinct: 100,
                payload_kb: 64,
                hit_ratio_pct: hits,
                n_threads: 1,
                total_ops: 1000,
            };
            let leg = run_legacy_bench(cfg, &images);
            let nw = run_new_bench(cfg, &images);
            eprintln!(
                "{:<9} | {:<17.2} | {:<16.2} | {:<14.2} | {:<13.2} | {:<12.1}x | {:.2}x",
                format!("{hits}%"),
                leg.total_time_ms,
                leg.lock_us_per_op,
                nw.total_time_ms,
                nw.lock_us_per_op,
                leg.lock_us_per_op / nw.lock_us_per_op.max(0.001),
                leg.total_time_ms / nw.total_time_ms.max(0.001),
            );
        }

        // 4. Concurrency: 1 Thread vs 4 Threads (N=100, 64 KB, 90% hits, 2000 total ops)
        eprintln!(
            "\n--- DIMENSION 4: Concurrency (N=100, Payload: 64 KB, Hits: 90%, 2000 ops) ---"
        );
        eprintln!(
            "Threads   | Legacy Total (ms) | Legacy Lock (µs) | New Total (ms) | New Lock (µs) | Lock Speedup | Total Speedup"
        );
        eprintln!(
            "----------+-------------------+------------------+----------------+---------------+--------------+--------------"
        );
        for &threads in &[1, 4] {
            let payload_base = "D".repeat(64 * 1024);
            let images: Vec<Arc<String>> = (0..100)
                .map(|i| Arc::new(format!("data:image/png;base64,{i:04}_{payload_base}")))
                .collect();
            let cfg = ScenarioConfig {
                n_distinct: 100,
                payload_kb: 64,
                hit_ratio_pct: 90,
                n_threads: threads,
                total_ops: 2000,
            };
            let leg = run_legacy_bench(cfg, &images);
            let nw = run_new_bench(cfg, &images);
            eprintln!(
                "{:<9} | {:<17.2} | {:<16.2} | {:<14.2} | {:<13.2} | {:<12.1}x | {:.2}x",
                format!("{threads} thr"),
                leg.total_time_ms,
                leg.lock_us_per_op,
                nw.total_time_ms,
                nw.lock_us_per_op,
                leg.lock_us_per_op / nw.lock_us_per_op.max(0.001),
                leg.total_time_ms / nw.total_time_ms.max(0.001),
            );
        }
        eprintln!(
            "==========================================================================================\n"
        );
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
