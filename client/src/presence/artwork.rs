use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use image::ImageDecoder;

fn inline_png(pixels: image::RgbaImage) -> Option<String> {
    let mut bytes = Cursor::new(Vec::new());
    pixels.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    ))
}

fn from_file(path: &Path) -> Option<String> {
    const MAX_BYTES: u64 = 8 * 1024 * 1024;
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > MAX_BYTES {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder().ok()?;
    let (w, h) = decoder.dimensions();
    if w == 0 || h == 0 || u64::from(w) * u64::from(h) > 16_000_000 {
        return None;
    }
    let pixels = image::DynamicImage::from_decoder(decoder)
        .ok()?
        .resize(64, 64, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let mut square = image::RgbaImage::new(64, 64);
    image::imageops::overlay(
        &mut square,
        &pixels,
        i64::from((64 - pixels.width()) / 2),
        i64::from((64 - pixels.height()) / 2),
    );
    inline_png(square)
}

fn steam_candidates(root: &Path, app_id: u32) -> Vec<PathBuf> {
    let cache = root.join("appcache/librarycache");
    let mut candidates = Vec::new();
    for suffix in [
        "icon.jpg",
        "icon.png",
        "library_600x900.jpg",
        "library_capsule.jpg",
        "header.jpg",
    ] {
        candidates.push(cache.join(format!("{app_id}_{suffix}")));
    }
    let folder = cache.join(app_id.to_string());
    let mut nested = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&folder) {
        for entry in entries.flatten().take(256) {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                for name in [
                    "icon.jpg",
                    "icon.png",
                    "library_capsule.jpg",
                    "library_600x900.jpg",
                    "library_header.jpg",
                ] {
                    nested.push(path.join(name));
                }
            } else if path
                .extension()
                .is_some_and(|ext| ext == "jpg" || ext == "png")
            {
                nested.push(path);
            }
        }
    }
    nested.sort_by_key(|path| {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let priority = if name.contains("icon")
            || name
                .split('.')
                .next()
                .is_some_and(|stem| stem.len() == 40 && stem.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            0
        } else if name.contains("capsule") || name.contains("600x900") {
            1
        } else {
            2
        };
        (priority, path.clone())
    });
    // The newer per-app hashed cache can contain an icon even when legacy covers remain.
    candidates.splice(2..2, nested);
    candidates
}

pub(super) fn for_game(
    exe: &Path,
    steam_app_id: Option<u32>,
    xbox: Option<&super::xbox::Game>,
    steam_roots: &[PathBuf],
) -> Option<String> {
    from_executable(exe)
        .or_else(|| xbox.and_then(|game| game.artwork.iter().find_map(|path| from_file(path))))
        .or_else(|| {
            super::xbox::package_artwork(exe)
                .iter()
                .find_map(|path| from_file(path))
        })
        .or_else(|| {
            let id = steam_app_id?;
            steam_roots
                .iter()
                .flat_map(|root| steam_candidates(root, id))
                .find_map(|path| from_file(&path))
        })
}

#[cfg(windows)]
pub(super) fn from_executable(path: &Path) -> Option<String> {
    inline_png(executable_icon(path)?)
}

#[cfg(not(windows))]
pub(super) fn from_executable(_path: &Path) -> Option<String> {
    None
}

#[cfg(windows)]
fn executable_icon(path: &Path) -> Option<image::RgbaImage> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::Shell::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::core::PCWSTR;
    let filename: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let mut icon = HICON::default();
        if ExtractIconExW(PCWSTR(filename.as_ptr()), 0, Some(&mut icon), None, 1) != 1
            || icon.is_invalid()
        {
            return None;
        }
        let dc = CreateCompatibleDC(None);
        let result = (|| {
            if dc.is_invalid() {
                return None;
            }
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: 64,
                    biHeight: -64,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap =
                CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
            let old = SelectObject(dc, HGDIOBJ(bitmap.0));
            std::ptr::write_bytes(bits.cast::<u8>(), 0, 64 * 64 * 4);
            let drawn = DrawIconEx(dc, 0, 0, icon, 64, 64, 0, None, DI_NORMAL).is_ok();
            let mut bytes = std::slice::from_raw_parts(bits.cast::<u8>(), 64 * 64 * 4).to_vec();
            SelectObject(dc, old);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            if !drawn {
                return None;
            }
            for pixel in bytes.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            if bytes.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 0) {
                for pixel in bytes.as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
            }
            image::RgbaImage::from_raw(64, 64, bytes)
        })();
        let _ = DeleteDC(dc);
        let _ = DestroyIcon(icon);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn local_executable_icon_is_a_small_png() {
        let source = from_executable(&std::env::current_exe().unwrap()).unwrap();
        assert!(source.len() <= 96_000);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(source.split_once(',').unwrap().1)
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (64, 64));
    }

    fn png(source: &str) -> image::RgbaImage {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(source.split_once(',').unwrap().1)
            .unwrap();
        image::load_from_memory(&bytes).unwrap().to_rgba8()
    }

    #[test]
    fn steam_old_and_hashed_caches_supply_art_for_an_iconless_game() {
        let root = std::env::temp_dir().join(format!("discordia-steam-{}", uuid::Uuid::new_v4()));
        let cache = root.join("appcache/librarycache");
        let nested = cache.join("123/hash");
        std::fs::create_dir_all(&nested).unwrap();
        let legacy = cache.join("123_icon.jpg");
        let pixels = image::RgbaImage::from_pixel(32, 32, image::Rgba([180, 45, 90, 255]));
        pixels
            .save_with_format(&legacy, image::ImageFormat::Png)
            .unwrap();
        let roots = [root.clone()];
        let exe = root.join("iconless.exe");
        let image = for_game(&exe, Some(123), None, &roots).unwrap();
        assert_eq!(png(&image).dimensions(), (64, 64));
        assert_eq!(png(&image).get_pixel(32, 32).0, [180, 45, 90, 255]);
        assert!(for_game(&exe, Some(124), None, &roots).is_none());
        std::fs::remove_file(&legacy).unwrap();
        let modern = nested.join("icon.png");
        pixels.save(&modern).unwrap();
        assert!(for_game(&exe, Some(123), None, &roots).is_some());
        std::fs::remove_file(&modern).unwrap();
        std::fs::remove_dir(nested).unwrap();
        std::fs::remove_dir(cache.join("123")).unwrap();
        std::fs::remove_dir(&cache).unwrap();
        std::fs::remove_dir(root.join("appcache")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn xbox_logo_fallback_preserves_transparency_and_aspect_ratio() {
        let root = std::env::temp_dir().join(format!("discordia-art-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("logo.png");
        image::RgbaImage::from_pixel(128, 64, image::Rgba([255, 100, 0, 128]))
            .save(&path)
            .unwrap();
        let game = super::super::xbox::Game {
            name: Some("Game".into()),
            artwork: vec![root.join("missing.png"), path.clone()],
        };
        let source = for_game(&root.join("iconless.exe"), None, Some(&game), &[]).unwrap();
        assert!(source.len() <= 96_000);
        let image = png(&source);
        assert_eq!(image.dimensions(), (64, 64));
        assert_eq!(image.get_pixel(32, 0).0[3], 0);
        assert_eq!(image.get_pixel(32, 32).0[3], 128);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn malformed_oversized_and_missing_artwork_is_ignored() {
        let path = std::env::temp_dir().join(format!("discordia-art-{}.png", uuid::Uuid::new_v4()));
        assert!(from_file(&path).is_none());
        std::fs::write(&path, b"broken image").unwrap();
        assert!(from_file(&path).is_none());
        std::fs::File::create(&path)
            .unwrap()
            .set_len(8 * 1024 * 1024 + 1)
            .unwrap();
        assert!(from_file(&path).is_none());
        image::RgbaImage::new(8193, 1).save(&path).unwrap();
        assert!(from_file(&path).is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn an_executable_without_resources_does_not_supply_the_windows_default_icon() {
        let path =
            std::env::temp_dir().join(format!("discordia-iconless-{}.exe", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"no icon resources").unwrap();
        assert!(from_executable(&path).is_none());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    #[ignore = "requires the local Steam Call of Duty image cache"]
    fn live_steam_call_of_duty_artwork() {
        let source = for_game(
            &std::env::temp_dir().join("cod.exe"),
            Some(1938090),
            None,
            &super::super::installed::steam_roots(),
        )
        .expect("Steam must have a cached Call of Duty image");
        assert!(source.len() <= 96_000);
        assert_eq!(png(&source).dimensions(), (64, 64));
    }
}
