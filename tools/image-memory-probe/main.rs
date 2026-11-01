use base64::Engine as _;
use dioxus::prelude::*;
use std::{path::Path, sync::Arc};

pub(crate) use dioxus_desktop as platform;
pub(crate) use discordia_protocol as protocol;
mod chat_image {
    pub const MAX_DM_BYTES: usize = 10 * 1024 * 1024;
}

#[allow(dead_code)]
mod image_assets {
    include!("../../client/src/image_assets.rs");

    pub fn stats_route(corpus_bytes: usize) {
        let assets = try_consume_context::<Assets>().expect("fixture asset context");
        crate::platform::use_asset_handler("probe-stats", move |_, responder| {
            let inner = assets.0.lock();
            let entries = inner.entries.len();
            let buffers = inner
                .entries
                .values()
                .filter(|e| e.decoded.is_some())
                .count();
            let bytes: usize = inner
                .entries
                .values()
                .filter_map(|e| e.decoded.as_ref())
                .map(|d| d.1.len())
                .sum();
            let data = serde_json::json!({"entries":entries,"encodedBuffers":buffers,"encodedBytes":bytes,"corpusBytes":corpus_bytes}).to_string().into_bytes();
            let mut response = crate::platform::wry::http::Response::new(data);
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().expect("header"));
            responder.respond(response);
        });
    }

    pub fn fixture_source(data: Arc<String>, preview: bool) -> ImageSource {
        #[cfg(feature = "thumbnails")]
        if preview {
            return chat_preview_source(data);
        }
        let _ = preview;
        chat_source(data)
    }
}

struct Resource {
    name: String,
    data: Arc<String>,
}

fn resources() -> Vec<Resource> {
    let directory = std::env::var("IMAGE_PROBE_DATA").expect("IMAGE_PROBE_DATA");
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .expect("fixtures")
        .map(|e| e.expect("file").path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("jpg" | "png" | "gif")
            )
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let mime = match p.extension().and_then(|e| e.to_str()) {
                Some("gif") => "image/gif",
                Some("png") => "image/png",
                _ => "image/jpeg",
            };
            Resource {
                name: p.file_stem().expect("stem").to_string_lossy().into_owned(),
                data: Arc::new(format!(
                    "data:{mime};base64,{}",
                    base64::engine::general_purpose::STANDARD
                        .encode(std::fs::read(p).expect("image"))
                )),
            }
        })
        .collect()
}

#[component]
fn Fixture() -> Element {
    image_assets::use_image_assets();
    let data = use_hook(|| Arc::new(resources()));
    let corpus_bytes = data.iter().map(|r| r.data.len()).sum();
    assert!(
        corpus_bytes <= 128 * 1024 * 1024,
        "fixture exceeds desktop MediaCache budget"
    );
    image_assets::stats_route(corpus_bytes);
    let mut scenario = use_signal(|| 0usize);
    let mut cycle = use_signal(|| 0usize);
    let source_data = data.clone();
    let sources = use_memo(move || {
        let s = scenario();
        let c = cycle();
        source_data
            .iter()
            .filter(|r| match s {
                1 => r.name.starts_with("chat"),
                2 | 8 => r.name.starts_with("high"),
                3 => r.name.starts_with("avatar") || r.name.starts_with("emoji"),
                4 => r.name.starts_with("gif"),
                5 => {
                    r.name.starts_with("high")
                        && r.name[4..]
                            .parse::<usize>()
                            .is_ok_and(|i| (i + 8 - c % 8) % 8 < 4)
                }
                6 => {
                    r.name.starts_with("dm")
                        && r.name[2..].parse::<usize>().is_ok_and(|i| i % 3 == c % 3)
                }
                7 => r.name == "high0" && c % 2 == 0,
                _ => false,
            })
            .map(|r| {
                if matches!(s, 1 | 2 | 5 | 8) {
                    image_assets::fixture_source(r.data.clone(), true)
                } else if s == 3 || s == 4 {
                    image_assets::source(r.data.clone())
                } else {
                    image_assets::chat_source(r.data.clone())
                }
            })
            .collect::<Vec<_>>()
    });
    let background = if scenario() == 9 {
        data.iter()
            .find(|r| r.name == format!("background{}", cycle() % 2))
            .map(|r| r.data.clone())
    } else {
        None
    };
    rsx! {
        style { "body{{margin:0;background:#181818;color:#eee;font:14px sans-serif}}nav{{height:48px}}#images{{display:flex;flex-wrap:wrap;gap:8px}}img{{object-fit:contain;width:auto;height:auto;max-width:320px;max-height:240px}}.avatars img{{width:32px;height:32px;object-fit:cover}}.full img{{max-width:none;max-height:none;width:100%}}" }
        nav {
            for n in 0..10 {
                button { id: "scenario-{n}", onclick: move |_| { cycle.set(0); scenario.set(n); }, "{n}" }
            }
            button { id: "next", onclick: move |_| cycle += 1, "next" }
        }
        div { id: "probe", "data-scenario": "{scenario}", "data-cycle": "{cycle}",
            div { id: "images", class: if scenario() == 3 { "avatars" } else if scenario() == 7 { "full" } else { "chat" },
                for src in sources() {
                    img { key: "{src}", src: "{src}", alt: "fixture" }
                }
            }
            if let Some(src) = background {
                div { id: "background", style: "width:100%;height:700px;background-image:url('{src}');background-size:cover" }
            }
        }
        document::Script { {include_str!("../../client/assets/gif-playback.js")} }
    }
}

fn generate(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use image::{Delay, Frame, RgbImage, RgbaImage};
    std::fs::create_dir_all(directory)?;
    for (kind, count, w, h) in [
        ("chat", 8, 1920, 1280),
        ("high", 8, 4000, 3000),
        ("avatar", 24, 1024, 1024),
        ("emoji", 12, 128, 128),
        ("dm", 6, 640, 480),
        ("background", 2, 3840, 2160),
    ] {
        for id in 0..count {
            let image = RgbImage::from_fn(w, h, |x, y| {
                let mut noise = x
                    .wrapping_mul(374761393)
                    .wrapping_add(y.wrapping_mul(668265263))
                    .wrapping_add(id * 12345);
                noise = (noise ^ (noise >> 13)).wrapping_mul(1274126177);
                let n = (noise % 24) as u8;
                image::Rgb([
                    ((x * 180 / w + id * 11) % 220) as u8 + n,
                    ((y * 180 / h + id * 7) % 220) as u8 + n,
                    (((x / 40 + y / 40 + id) % 8) * 24) as u8 + n,
                ])
            });
            let file = std::fs::File::create(directory.join(format!("{kind}{id}.jpg")))?;
            image::codecs::jpeg::JpegEncoder::new_with_quality(file, 92).encode_image(&image)?;
        }
    }
    for id in 0..4 {
        let mut encoder = image::codecs::gif::GifEncoder::new(std::fs::File::create(
            directory.join(format!("gif{id}.gif")),
        )?);
        encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
        for frame in 0..12 {
            let image = RgbaImage::from_fn(192, 128, |x, y| {
                image::Rgba([
                    ((x + frame * 16) % 255) as u8,
                    ((y * 2 + id * 20) % 255) as u8,
                    128,
                    255,
                ])
            });
            encoder.encode_frame(Frame::from_parts(
                image,
                0,
                0,
                Delay::from_numer_denom_ms(80, 1),
            ))?;
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("generate") {
        return generate(Path::new(
            &std::env::args()
                .nth(2)
                .ok_or("fixture directory required")?,
        ));
    }
    dioxus::LaunchBuilder::desktop()
        .with_cfg(
            dioxus_desktop::Config::new().with_menu(None).with_window(
                dioxus_desktop::WindowBuilder::new()
                    .with_title("Discordia image probe")
                    .with_inner_size(dioxus_desktop::tao::dpi::LogicalSize::new(1440., 900.)),
            ),
        )
        .launch(Fixture);
    Ok(())
}
