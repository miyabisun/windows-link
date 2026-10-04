//! Button icons: the picture Explorer shows for a file (an exe's icon, a shortcut's
//! icon), as PNG.

use std::path::Path;

/// Icons are served at this size; the panel scales them down.
pub const SIZE: i32 = 256;

/// Rows of premultiplied BGRA (Windows bitmaps) to straight RGBA (PNG). A bitmap
/// without any alpha is treated as opaque.
pub fn to_rgba(bgra: &[u8]) -> Vec<u8> {
    let opaque = bgra.chunks_exact(4).all(|p| p[3] == 0);
    bgra.chunks_exact(4)
        .flat_map(|p| {
            let alpha = if opaque { 255 } else { p[3] };
            let unmultiply = |c: u8| {
                if alpha == 0 || alpha == 255 {
                    c
                } else {
                    u8::try_from((u16::from(c) * 255 / u16::from(alpha)).min(255)).unwrap_or(255)
                }
            };
            [unmultiply(p[2]), unmultiply(p[1]), unmultiply(p[0]), alpha]
        })
        .collect()
}

pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|err| err.to_string())?;
    writer
        .write_image_data(rgba)
        .map_err(|err| err.to_string())?;
    writer.finish().map_err(|err| err.to_string())?;
    Ok(out)
}

/// The Windows icon of `path` as a PNG of about `SIZE` pixels. Blocking; uses COM.
pub fn icon_png(path: &Path) -> Result<Vec<u8>, String> {
    use std::{ffi::c_void, os::windows::ffi::OsStrExt};

    use windows::{
        Win32::{
            Foundation::SIZE,
            Graphics::Gdi::{
                BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC,
                GetDIBits, GetObjectW, ReleaseDC,
            },
            System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx},
            UI::Shell::{
                IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK,
                SIIGBF_ICONONLY,
            },
        },
        core::PCWSTR,
    };

    let shell = path.to_string_lossy().to_lowercase().starts_with("shell:");
    if !shell && !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)
                .map_err(|err| format!("{}: {err}", path.display()))?;
        let bitmap = factory
            .GetImage(
                SIZE { cx: SIZE, cy: SIZE },
                SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
            )
            .map_err(|err| format!("{}: {err}", path.display()))?;
        let mut info = BITMAP::default();
        let got = GetObjectW(
            bitmap.into(),
            i32::try_from(std::mem::size_of::<BITMAP>()).unwrap_or(0),
            Some((&raw mut info).cast::<c_void>()),
        );
        let (width, height) = (info.bmWidth, info.bmHeight.abs());
        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(std::mem::size_of::<BITMAPINFOHEADER>()).unwrap_or(0),
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bgra = vec![0u8; usize::try_from(width * height * 4).unwrap_or(0)];
        let dc = GetDC(None);
        let lines = GetDIBits(
            dc,
            bitmap,
            0,
            u32::try_from(height).unwrap_or(0),
            Some(bgra.as_mut_ptr().cast::<c_void>()),
            &raw mut header,
            DIB_RGB_COLORS,
        );
        ReleaseDC(None, dc);
        let _ = DeleteObject(bitmap.into());
        if got == 0 || lines == 0 || width <= 0 {
            return Err(format!("cannot read the icon of {}", path.display()));
        }
        encode_png(
            u32::try_from(width).unwrap_or(0),
            u32::try_from(height).unwrap_or(0),
            &to_rgba(&bgra),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{encode_png, to_rgba};

    #[test]
    fn turns_premultiplied_bgra_into_straight_rgba() {
        // Opaque red, half-transparent premultiplied blue (0x80 → 0xff), fully clear.
        let bgra = [0, 0, 255, 255, 128, 0, 0, 128, 0, 0, 0, 0];
        assert_eq!(to_rgba(&bgra), [255, 0, 0, 255, 0, 0, 255, 128, 0, 0, 0, 0]);
    }

    #[test]
    fn a_bitmap_without_alpha_is_opaque() {
        let bgra = [10, 20, 30, 0, 40, 50, 60, 0];
        assert_eq!(to_rgba(&bgra), [30, 20, 10, 255, 60, 50, 40, 255]);
    }

    #[test]
    fn encodes_a_png() {
        let png = encode_png(1, 1, &[1, 2, 3, 4]).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}
