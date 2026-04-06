//! HBITMAP helpers.
//!
//! Phase 1 only has `create_solid_color`, which returns a square 32-bpp
//! top-down DIB section filled with a single ARGB value. Phase 2+ will
//! add helpers to convert an `image::RgbaImage` into an HBITMAP.

use std::ffi::c_void;

use windows::core::{Error, Result};
use windows::Win32::Foundation::E_FAIL;
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP,
};

/// Create a `size x size` 32-bit ARGB bitmap filled with `color`
/// (0xAARRGGBB).
///
/// The returned HBITMAP is owned by the caller. Explorer takes ownership
/// when we return it from `IThumbnailProvider::GetThumbnail` and frees
/// it after drawing.
pub fn create_solid_color(size: i32, color: u32) -> Result<HBITMAP> {
    // BITMAPINFOHEADER describes the pixel layout of the DIB.
    let mut bi = BITMAPINFO::default();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = size;
    // Negative height = top-down rows (origin at top-left), which is
    // what Explorer expects for thumbnail providers.
    bi.bmiHeader.biHeight = -size;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB.0;

    // CreateDIBSection writes a pointer to the pixel memory into `bits`.
    let mut bits: *mut c_void = std::ptr::null_mut();
    let hbmp = unsafe {
        CreateDIBSection(
            None,         // hdc: NULL = use the DIB's own memory, not a device
            &bi,
            DIB_RGB_COLORS,
            &mut bits,
            None,         // hSection: NULL = GDI allocates for us
            0,            // offset: ignored when hSection is NULL
        )?
    };

    if bits.is_null() {
        return Err(Error::from_hresult(E_FAIL));
    }

    // Memory layout for 32bpp BI_RGB is BGRA little-endian, which is
    // the same byte pattern as a 0xAARRGGBB u32 on x86/x64.
    let pixel_count = (size as usize) * (size as usize);
    unsafe {
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, pixel_count);
        pixels.fill(color);
    }

    Ok(hbmp)
}
