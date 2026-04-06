//! HBITMAP helpers.
//!
//! `from_rgba` converts an `image::RgbaImage` (straight RGBA) into a
//! top-down 32bpp DIB section with premultiplied BGRA — the format
//! Explorer wants when we return `WTSAT_ARGB`.

use std::ffi::c_void;

use windows::core::{Error, Result};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};

/// Convert an `image::RgbaImage` to an HBITMAP suitable for returning
/// from `IThumbnailProvider::GetThumbnail` with `WTSAT_ARGB`.
///
/// - Converts RGBA → premultiplied BGRA (Windows convention).
/// - Creates a top-down 32bpp DIB section sized exactly to the image.
///
/// The returned HBITMAP is owned by the caller (Explorer will free it).
pub fn from_rgba(img: &image::RgbaImage) -> Result<HBITMAP> {
    let (width, height) = img.dimensions();
    if width == 0 || height == 0 {
        return Err(Error::from_hresult(E_FAIL));
    }

    let mut bi = BITMAPINFO::default();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = width as i32;
    bi.bmiHeader.biHeight = -(height as i32); // top-down
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB.0;

    let mut bits: *mut c_void = std::ptr::null_mut();
    let hbmp = unsafe {
        CreateDIBSection(None, &bi, DIB_RGB_COLORS, &mut bits, None, 0)?
    };
    if bits.is_null() {
        return Err(Error::from_hresult(E_FAIL));
    }

    // Copy + convert pixel layout.
    let src = img.as_raw(); // RGBA bytes
    let pixel_count = (width as usize) * (height as usize);
    unsafe {
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, pixel_count * 4);
        for i in 0..pixel_count {
            let r = src[i * 4];
            let g = src[i * 4 + 1];
            let b = src[i * 4 + 2];
            let a = src[i * 4 + 3];
            // Premultiply alpha so Explorer can composite correctly.
            dst[i * 4] = premul(b, a);
            dst[i * 4 + 1] = premul(g, a);
            dst[i * 4 + 2] = premul(r, a);
            dst[i * 4 + 3] = a;
        }
    }

    Ok(hbmp)
}

/// Integer premultiply: `(c * a + 127) / 255`, rounded.
#[inline]
fn premul(c: u8, a: u8) -> u8 {
    ((c as u16 * a as u16 + 127) / 255) as u8
}
