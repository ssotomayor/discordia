use base64::Engine as _;
use dioxus::prelude::*;
use std::{path::Path, sync::Arc};

pub(crate) use dioxus_desktop as platform;
pub(crate) use discordia_protocol as protocol;
#[allow(dead_code)]
#[path = "../../client/src/chat_image.rs"]
mod chat_image;
mod dm_server;
#[cfg(feature = "unified-policy")]
mod i18n {
    pub fn text(value: &str, _args: &[(&str, &str)]) -> String {
        value.to_owned()
    }
}
#[cfg(feature = "unified-policy")]
#[path = "../../client/src/features/image_attachment.rs"]
mod image_attachment;
#[cfg(feature = "progressive-viewer")]
#[allow(dead_code)]
#[path = "../../client/src/features/image_viewer.rs"]
mod image_viewer;
#[cfg(feature = "progressive-viewer")]
mod icons {
    pub const PLUS: &str = "+";
    pub const CLOSE: &str = "×";
}
#[cfg(feature = "progressive-viewer")]
mod mobile_back {
    pub const SETTINGS: u16 = 10;
    pub fn use_handler(_: u16, _: bool, _: impl FnMut(()) + 'static) {}
}
#[cfg(feature = "progressive-viewer")]
mod nostr {
    pub mod dm_image {
        pub type Descriptor = String;
        pub async fn load_original(_: &str, _: &Descriptor) -> Result<String, String> {
            Err("fixture transport".into())
        }
    }
}
#[cfg(feature = "progressive-viewer")]
mod state {
    use super::*;
    #[derive(Default)]
    pub struct AppState {
        pub image_viewer: Option<image_viewer::ImageView>,
        pub original: Option<Arc<String>>,
    }
    impl AppState {
        pub fn retry_image_viewer(&mut self, id: uuid::Uuid) {
            self.original = None;
            if let Some(view) = self.image_viewer.as_mut().filter(|v| v.id == id) {
                view.retry();
            }
        }
        pub fn media_src_shared(&self, _: &str) -> Option<Arc<String>> {
            self.original.clone()
        }
    }
    pub fn use_app_state() -> Signal<AppState> {
        use_context()
    }
}
#[cfg(feature = "thumbnails")]
mod image_preview {
    include!("../../client/src/image_preview.rs");
}

#[allow(dead_code)]
mod image_assets {
    include!(env!("IMAGE_PROBE_ASSETS_SOURCE"));

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

    pub fn fixture_full_src(source: &ImageSource) -> &str {
        #[cfg(feature = "thumbnails")]
        return source.full_src();
        #[cfg(not(feature = "thumbnails"))]
        &source.uri
    }

    pub fn fixture_has_preview(source: &ImageSource) -> bool {
        #[cfg(feature = "thumbnails")]
        return source.has_preview();
        #[cfg(not(feature = "thumbnails"))]
        {
            let _ = source;
            false
        }
    }
}

#[derive(PartialEq)]
struct Resource {
    name: String,
    data: Arc<String>,
}

fn resources() -> Vec<Resource> {
    let directory = std::env::var("IMAGE_PROBE_DATA").expect("IMAGE_PROBE_DATA");
    resources_from(Path::new(&directory))
}

fn resources_from(directory: &Path) -> Vec<Resource> {
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
    #[cfg(feature = "progressive-viewer")]
    let _viewer_state = use_context_provider(|| Signal::new(state::AppState::default()));
    let data = use_hook(|| Arc::new(resources()));
    let mut scenario = use_signal(|| 0usize);
    let mut cycle = use_signal(|| 0usize);
    #[cfg(feature = "unified-policy")]
    let density = image_assets::preview_density();
    #[cfg(not(feature = "unified-policy"))]
    let density = Some(true);
    #[cfg(feature = "unified-policy")]
    let originals = use_resource(move || async move {
        if image_assets::preview_density() != Some(false) && scenario() != 7 {
            return None;
        }
        let directory = std::env::var("IMAGE_PROBE_ORIGINAL_DATA").ok()?;
        tokio::task::spawn_blocking(move || Arc::new(resources_from(Path::new(&directory))))
            .await
            .ok()
    });
    let corpus_bytes = data.iter().map(|r| r.data.len()).sum();
    assert!(
        corpus_bytes <= 128 * 1024 * 1024,
        "fixture exceeds desktop MediaCache budget"
    );
    image_assets::stats_route(corpus_bytes);
    let source_data = data.clone();
    let sources = use_memo(move || {
        let s = scenario();
        let c = cycle();
        #[cfg(feature = "unified-policy")]
        let selected_data = match image_assets::preview_density() {
            Some(true) if s != 7 => source_data.clone(),
            Some(true) | Some(false) => {
                let value = originals.read();
                let Some(Some(data)) = value.as_ref() else {
                    return Vec::new();
                };
                data.clone()
            }
            None => return Vec::new(),
        };
        #[cfg(not(feature = "unified-policy"))]
        let selected_data = source_data.clone();
        selected_data
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
                10 => r.name.starts_with("oriented"),
                11 => r.name.starts_with("alpha"),
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
            for n in 0..12 {
                button { id: "scenario-{n}", onclick: move |_| { cycle.set(0); scenario.set(n); }, "{n}" }
            }
            button { id: "next", onclick: move |_| cycle += 1, "next" }
            if cfg!(feature="progressive-viewer") {
                ViewerControls {data:data.clone()}
            }
        }
        div { id: "probe", "data-scenario": "{scenario}", "data-cycle": "{cycle}", "data-density":"{density:?}",
            div { id: "images", class: if scenario() == 3 { "avatars" } else if scenario() == 7 { "full" } else { "chat" },
                for src in sources() {
                    if image_assets::fixture_has_preview(&src) {
                    picture { key: "{src}", style: "display:contents;",
                        source { style: "display:none;", "media": "(max-resolution: 2dppx)", "srcset": "{src}" }
                        img { src: image_assets::fixture_full_src(&src), alt: "fixture" }
                    }
                    } else {
                        if cfg!(feature="unified-policy") && !matches!(scenario(),3|7) {
                            PolicyImage { key:"{src}",source:src }
                        } else { img { key: "{src}", src: "{src}", alt: "fixture" } }
                    }
                }
            }
            if let Some(src) = background {
                div { id: "background", style: "width:100%;height:700px;background-image:url('{src}');background-size:cover" }
            }
        }
        document::Script { {include_str!("../../client/assets/gif-playback.js")} }
        if cfg!(feature="progressive-viewer") {ProbeViewer {}}
    }
}

#[component]
fn ProbeViewer() -> Element {
    #[cfg(feature = "progressive-viewer")]
    return rsx! {document::Style {{include_str!("../../client/assets/image-viewer.css")}}image_viewer::ImageViewer {}};
    #[cfg(not(feature = "progressive-viewer"))]
    rsx! {}
}
#[component]
fn ViewerControls(data: Arc<Vec<Resource>>) -> Element {
    #[cfg(feature = "progressive-viewer")]
    {
        let mut state = state::use_app_state();
        return rsx! {
            button {id:"viewer-open",onclick:move |_|{
                let source=data.iter().find(|r|r.name=="high0").expect("fixture").data.clone();
                {let mut s=state.write();s.original=None;s.image_viewer=Some(image_viewer::ImageView::new(source,Some(image_viewer::Original::Community("original".into()))));}
            },"open preview"}
            button {id:"viewer-deliver",onclick:move |_|{
                let directory=std::env::var("IMAGE_PROBE_ORIGINAL_DATA").expect("fixture originals");
                spawn(async move {let data=tokio::task::spawn_blocking(move||resources_from(Path::new(&directory)).into_iter().find(|r|r.name=="high0").expect("fixture").data).await.expect("fixture load");state.write().original=Some(data);});
            },"deliver original"}
            button {id:"viewer-fail",onclick:move |_|state.write().original=Some(Arc::new(String::new())),"fail"}
            button {id:"viewer-corrupt",onclick:move |_|state.write().original=Some(Arc::new("data:image/png;base64,aW52YWxpZA==".into())),"corrupt"}
        };
    }
    #[cfg(not(feature = "progressive-viewer"))]
    {
        let _ = data;
        rsx! {}
    }
}

#[component]
fn PolicyImage(source: image_assets::ImageSource) -> Element {
    #[cfg(feature = "unified-policy")]
    return rsx! { image_attachment::ImageAttachment {source,onclick:move |_|{}} };
    #[cfg(not(feature = "unified-policy"))]
    rsx! {img {src:"{source}",alt:"fixture"}}
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
    if std::env::args().nth(1).as_deref() == Some("compare-dm-server") {
        return dm_server::prepare(false);
    }
    if std::env::args().nth(1).as_deref() == Some("prepare-unified") {
        return dm_server::prepare(true);
    }
    if std::env::args().nth(1).as_deref() == Some("compare") {
        let left = image::open(std::env::args().nth(2).ok_or("first PNG required")?)?.to_rgba8();
        let right = image::open(std::env::args().nth(3).ok_or("second PNG required")?)?.to_rgba8();
        if left.dimensions() != right.dimensions() {
            return Err("screenshot dimensions differ".into());
        }
        let mut total = 0u64;
        let mut squared = 0u64;
        let mut max = 0u8;
        let mut changed = 0usize;
        for (a, b) in left
            .as_raw()
            .chunks_exact(4)
            .zip(right.as_raw().chunks_exact(4))
        {
            let mut different = false;
            for channel in 0..3 {
                let delta = a[channel].abs_diff(b[channel]);
                total += u64::from(delta);
                squared += u64::from(delta) * u64::from(delta);
                max = max.max(delta);
                different |= delta != 0;
            }
            changed += usize::from(different);
        }
        let pixels = u64::from(left.width()) * u64::from(left.height());
        println!(
            "{}",
            serde_json::json!({"width":left.width(),"height":left.height(),"rgbMeanAbsoluteError":total as f64/(pixels*3) as f64,"rgbRootMeanSquareError":(squared as f64/(pixels*3) as f64).sqrt(),"maxChannelDifference":max,"changedPixels":changed})
        );
        return Ok(());
    }
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
