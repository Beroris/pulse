//! Icons from executables, as RGBA pixels the UI can draw.

use tm_core::platform::IconPixels;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Shell::SHDefExtractIconW;
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO};
use windows::core::PCWSTR;

use crate::util::wide;

/// The first icon in a file, at `size` pixels square.
pub fn file_icon(path: &str, index: i32, size: u32) -> Option<IconPixels> {
    let p = wide(path);
    let mut large = HICON::default();
    // The low word is the large icon size, the high word the small one.
    unsafe { SHDefExtractIconW(PCWSTR(p.as_ptr()), index, 0, Some(&mut large), None, size & 0xFFFF) }.ok().ok()?;
    if large.is_invalid() {
        return None;
    }
    let px = hicon_pixels(large);
    unsafe {
        let _ = DestroyIcon(large);
    }
    px
}

/// Reads an icon's pixels. Handles both alpha icons and old icons that only have a mask.
pub fn hicon_pixels(icon: HICON) -> Option<IconPixels> {
    let mut ii = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut ii) }.ok()?;
    let result = (|| {
        let dc = unsafe { CreateCompatibleDC(None) };
        if dc.is_invalid() {
            return None;
        }
        let read = |bmp: HBITMAP| -> Option<(u32, u32, Vec<u8>)> {
            let mut bm = BITMAP::default();
            if unsafe { GetObjectW(bmp.into(), std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) } == 0 {
                return None;
            }
            let (w, h) = (bm.bmWidth as u32, bm.bmHeight.unsigned_abs());
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w as i32,
                    biHeight: -(h as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut px = vec![0u8; (w * h * 4) as usize];
            let lines = unsafe { GetDIBits(dc, bmp, 0, h, Some(px.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS) };
            (lines > 0).then_some((w, h, px))
        };
        let out = if !ii.hbmColor.is_invalid() {
            let (w, h, mut bgra) = read(ii.hbmColor)?;
            let has_alpha = bgra.chunks_exact(4).any(|p| p[3] != 0);
            if !has_alpha {
                // No alpha channel: the mask says which pixels are transparent.
                let mask = if ii.hbmMask.is_invalid() { None } else { read(ii.hbmMask) };
                for (i, p) in bgra.chunks_exact_mut(4).enumerate() {
                    let transparent = mask.as_ref().is_some_and(|(_, _, m)| m.get(i * 4).copied().unwrap_or(0) != 0);
                    p[3] = if transparent { 0 } else { 255 };
                }
            }
            for p in bgra.chunks_exact_mut(4) {
                p.swap(0, 2);
            }
            Some(IconPixels { width: w, height: h, rgba: bgra })
        } else {
            // Monochrome icon: the mask holds the AND mask on top and the XOR image below.
            let (w, h2, m) = read(ii.hbmMask)?;
            let h = h2 / 2;
            let mut rgba = vec![0u8; (w * h * 4) as usize];
            for i in 0..(w * h) as usize {
                let and = m[i * 4] != 0;
                let xor = m[(w * h) as usize * 4 + i * 4] != 0;
                let v = if xor { 255 } else { 0 };
                rgba[i * 4..i * 4 + 4].copy_from_slice(&[v, v, v, if and { 0 } else { 255 }]);
            }
            Some(IconPixels { width: w, height: h, rgba })
        };
        unsafe {
            let _ = DeleteDC(dc);
        }
        out
    })();
    unsafe {
        if !ii.hbmColor.is_invalid() {
            let _ = DeleteObject(ii.hbmColor.into());
        }
        if !ii.hbmMask.is_invalid() {
            let _ = DeleteObject(ii.hbmMask.into());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_notepad() {
        let path = format!("{}notepad.exe", crate::util::windows_dir());
        let Some(i) = file_icon(&path, 0, 32) else { return };
        assert_eq!((i.width, i.height), (32, 32));
        assert!(i.rgba.chunks_exact(4).any(|p| p[3] > 0));
    }
}
