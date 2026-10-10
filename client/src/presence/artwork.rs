use std::path::Path;

#[cfg(windows)]
use base64::Engine as _;

#[cfg(windows)]
pub(super) fn from_executable(path: &Path) -> Option<String> {
    let pixels = executable_icon(path)?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    pixels.write_to(&mut bytes, image::ImageFormat::Png).ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    ))
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
        let mut icon = SHFILEINFOW::default();
        if SHGetFileInfoW(
            PCWSTR(filename.as_ptr()),
            Default::default(),
            Some(&mut icon),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        ) == 0
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
            let drawn = DrawIconEx(dc, 0, 0, icon.hIcon, 64, 64, 0, None, DI_NORMAL).is_ok();
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
        let _ = DestroyIcon(icon.hIcon);
        result
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn local_executable_icon_is_a_small_png() {
        let source = from_executable(&std::env::current_exe().unwrap()).unwrap();
        assert!(source.len() <= 96_000);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(source.split_once(',').unwrap().1)
            .unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (64, 64));
    }
}
