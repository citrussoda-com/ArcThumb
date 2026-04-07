//! `IPreviewHandler` for ArcThumb.
//!
//! Mirrors the architecture of `com::ArcThumbProvider`, but instead
//! of returning a single HBITMAP it owns a child window inside
//! Explorer's preview pane (`Alt+P`) and paints the cover image into
//! it. The decoder pipeline (archive → first image → decode) is
//! identical — only the rendering target changes.
//!
//! Lifecycle as Explorer / `prevhost.exe` calls it:
//!
//! 1. `IClassFactory::CreateInstance` → `ArcThumbPreviewHandler::default()`
//! 2. `IInitializeWithStream::Initialize(stream)` → stash the stream
//! 3. `IObjectWithSite::SetSite(site)` → stash (we never call back)
//! 4. `IPreviewHandler::SetWindow(parent, rect)` → remember parent + rect
//! 5. `IPreviewHandler::SetRect(rect)` → resize child window if any
//! 6. `IPreviewHandler::DoPreview()` → consume the stream, decode the
//!    cover, create the child window, schedule a paint
//! 7. (`SetRect` may fire many times during drag-resize. Each one
//!    moves the child window and invalidates it; the WM_PAINT handler
//!    re-resizes the cached image.)
//! 8. `IPreviewHandler::Unload()` → destroy the child window, drop
//!    cached state
//! 9. `Release()` → eventually drops the impl struct, which destroys
//!    any window we still own (safety net for hosts that skip Unload)
//!
//! Every COM entry point is wrapped in `catch_unwind` so a panic in
//! the decoder, GDI, or our own code can never escape into
//! `prevhost.exe` and crash it.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::OnceLock;

use windows::core::{implement, w, IUnknown, Interface, Result, GUID, PCWSTR};
use windows::Win32::Foundation::{
    BOOL, CLASS_E_NOAGGREGATION, COLORREF, E_FAIL, E_NOINTERFACE, E_POINTER, HINSTANCE, HWND,
    LPARAM, LRESULT, RECT, S_FALSE, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleDC, CreateSolidBrush, DeleteDC, DeleteObject, EndPaint,
    FillRect, GetSysColor, InvalidateRect, SelectObject, COLOR_WINDOW, HBITMAP, HBRUSH, HGDIOBJ,
    PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl, IStream};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    IObjectWithSite, IObjectWithSite_Impl, IOleWindow, IOleWindow_Impl,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetFocus, SetFocus};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{IPreviewHandler, IPreviewHandler_Impl};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW, LoadCursorW,
    MoveWindow, RegisterClassExW, SetParent, SetWindowLongPtrW, CREATESTRUCTW, CS_HREDRAW,
    CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, MSG, WINDOW_EX_STYLE, WM_DESTROY, WM_ERASEBKGND,
    WM_NCCREATE, WM_PAINT, WNDCLASSEXW, WS_CHILD, WS_VISIBLE,
};

use crate::{alog, archive, bitmap, decode, stream::ComStreamReader};

// =============================================================================
// CLSID + class factory
// =============================================================================

/// CLSID for the ArcThumb preview handler. **Never change** — baked
/// into users' registries on install. Distinct from
/// `CLSID_ARCTHUMB_PROVIDER` (the thumbnail provider) so the two
/// classes register as separate COM objects and can be toggled
/// independently.
pub const CLSID_ARCTHUMB_PREVIEW: GUID =
    GUID::from_u128(0x8C7C1E5F_3D4A_4E2B_9F1A_7B5D6E8F9A0C);

#[implement(IClassFactory)]
pub struct ArcThumbPreviewClassFactory;

impl IClassFactory_Impl for ArcThumbPreviewClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        if punkouter.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        if ppvobject.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe {
            *ppvobject = std::ptr::null_mut();
            let handler = ArcThumbPreviewHandler::default();
            let unknown: IUnknown = handler.into();
            unknown.query(&*riid, ppvobject).ok()
        }
    }

    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        Ok(())
    }
}

// =============================================================================
// ArcThumbPreviewHandler
// =============================================================================

/// The COM object Explorer / prevhost.exe instantiates per file.
///
/// All mutable state lives behind interior-mutability primitives so
/// the COM trait methods can mutate it through `&self`.
#[implement(IPreviewHandler, IInitializeWithStream, IObjectWithSite, IOleWindow)]
#[derive(Default)]
pub struct ArcThumbPreviewHandler {
    /// IStream stashed by `Initialize`. Consumed by `DoPreview`.
    stream: RefCell<Option<IStream>>,
    /// Site interface set by `IObjectWithSite::SetSite`. We never
    /// call back into it but `GetSite` must round-trip it.
    site: RefCell<Option<IUnknown>>,
    /// Parent HWND set by `IPreviewHandler::SetWindow`.
    parent_hwnd: Cell<HWND>,
    /// Last rect set by `SetWindow` / `SetRect`, in parent coords.
    rect: Cell<RECT>,
    /// Our owned child window, created in `DoPreview`. Destroyed in
    /// `Unload` (or in `Drop` as a safety net).
    child_hwnd: Cell<HWND>,
    /// Decoded source image, retained across `SetRect` events so we
    /// don't re-parse the archive on every drag-resize tick.
    source: RefCell<Option<image::DynamicImage>>,
    /// Cached HBITMAP at the last drawn (width, height). Replaced on
    /// resize. Freed via `CachedBitmap::Drop`.
    cache: RefCell<Option<CachedBitmap>>,
}

/// Owned HBITMAP wrapper that frees the GDI handle on Drop.
struct CachedBitmap {
    width: i32,
    height: i32,
    hbitmap: HBITMAP,
}

impl Drop for CachedBitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.hbitmap.0));
        }
    }
}

impl Drop for ArcThumbPreviewHandler {
    /// Safety net: if a host releases us without calling `Unload`,
    /// the child window would leak. We tear it down here too.
    fn drop(&mut self) {
        let hwnd = self.child_hwnd.get();
        if !hwnd.is_invalid() {
            unsafe {
                // Clear our pointer first so a stray WM_PAINT during
                // teardown can't dereference us.
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(hwnd);
            }
        }
    }
}

// =============================================================================
// Panic-guard helper
// =============================================================================

/// Run `f`, returning its `Result<()>` on success or `E_FAIL` on panic.
/// Used by every COM entry point — a panic crossing the C ABI is UB
/// and would take down `prevhost.exe`.
fn guard<F: FnOnce() -> Result<()>>(label: &str, f: F) -> Result<()> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => {
            alog!("PANIC caught in {label}");
            Err(windows::core::Error::from_hresult(E_FAIL))
        }
    }
}

// =============================================================================
// IInitializeWithStream
// =============================================================================

impl IInitializeWithStream_Impl for ArcThumbPreviewHandler_Impl {
    fn Initialize(&self, pstream: Option<&IStream>, _grfmode: u32) -> Result<()> {
        guard("Preview::Initialize", || {
            *self.this.stream.borrow_mut() = pstream.cloned();
            Ok(())
        })
    }
}

// =============================================================================
// IObjectWithSite
// =============================================================================

impl IObjectWithSite_Impl for ArcThumbPreviewHandler_Impl {
    fn SetSite(&self, punksite: Option<&IUnknown>) -> Result<()> {
        guard("Preview::SetSite", || {
            *self.this.site.borrow_mut() = punksite.cloned();
            Ok(())
        })
    }

    fn GetSite(&self, riid: *const GUID, ppvsite: *mut *mut c_void) -> Result<()> {
        guard("Preview::GetSite", || {
            if riid.is_null() || ppvsite.is_null() {
                return Err(E_POINTER.into());
            }
            unsafe {
                *ppvsite = std::ptr::null_mut();
                let site = self.this.site.borrow();
                match site.as_ref() {
                    Some(unk) => unk.query(&*riid, ppvsite).ok(),
                    None => Err(E_NOINTERFACE.into()),
                }
            }
        })
    }
}

// =============================================================================
// IOleWindow
// =============================================================================

impl IOleWindow_Impl for ArcThumbPreviewHandler_Impl {
    fn GetWindow(&self) -> Result<HWND> {
        // No need for catch_unwind here — pure field load.
        Ok(self.this.child_hwnd.get())
    }

    fn ContextSensitiveHelp(&self, _fentermode: BOOL) -> Result<()> {
        // Explorer never calls this with TRUE; we have no help to show.
        Ok(())
    }
}

// =============================================================================
// IPreviewHandler
// =============================================================================

impl IPreviewHandler_Impl for ArcThumbPreviewHandler_Impl {
    fn SetWindow(&self, hwnd: HWND, prc: *const RECT) -> Result<()> {
        guard("Preview::SetWindow", || {
            self.this.parent_hwnd.set(hwnd);
            if !prc.is_null() {
                self.this.rect.set(unsafe { *prc });
            }
            // If the child window already exists (re-parenting case),
            // move it under the new parent and resize.
            let child = self.this.child_hwnd.get();
            if !child.is_invalid() && !hwnd.is_invalid() {
                let r = self.this.rect.get();
                unsafe {
                    let _ = SetParent(child, hwnd);
                    let _ = MoveWindow(
                        child,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        true,
                    );
                }
            }
            Ok(())
        })
    }

    fn SetRect(&self, prc: *const RECT) -> Result<()> {
        guard("Preview::SetRect", || {
            if prc.is_null() {
                return Err(E_POINTER.into());
            }
            let r = unsafe { *prc };
            self.this.rect.set(r);
            let child = self.this.child_hwnd.get();
            if !child.is_invalid() {
                unsafe {
                    let _ = MoveWindow(
                        child,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        true,
                    );
                    let _ = InvalidateRect(child, None, true);
                }
            }
            Ok(())
        })
    }

    fn DoPreview(&self) -> Result<()> {
        guard("Preview::DoPreview", || {
            // 1. Take the stream out so we can consume it.
            let stream = self
                .this
                .stream
                .borrow_mut()
                .take()
                .ok_or_else(|| windows::core::Error::from_hresult(E_FAIL))?;

            // 2. Reuse the existing decoder pipeline.
            let reader = ComStreamReader::new(stream);
            let (name, bytes) = archive::read_first_image(reader)
                .map_err(|e| {
                    alog!("Preview: archive read failed: {e}");
                    windows::core::Error::from_hresult(E_FAIL)
                })?;
            let img = decode::decode_with_limits(&name, &bytes).map_err(|e| {
                alog!("Preview: decode failed: {e}");
                windows::core::Error::from_hresult(E_FAIL)
            })?;
            alog!("Preview: decoded {}x{} from {}", img.width(), img.height(), name);
            *self.this.source.borrow_mut() = Some(img);

            // 3. Create the child window if we don't have one yet.
            if self.this.child_hwnd.get().is_invalid() {
                self.create_child_window()?;
            } else {
                // Re-use existing window — just trigger a repaint.
                unsafe {
                    let _ = InvalidateRect(self.this.child_hwnd.get(), None, true);
                }
            }
            Ok(())
        })
    }

    fn Unload(&self) -> Result<()> {
        // Unload must always succeed; swallow any internal failure.
        let _ = guard("Preview::Unload", || {
            let hwnd = self.this.child_hwnd.replace(HWND::default());
            if !hwnd.is_invalid() {
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                    let _ = DestroyWindow(hwnd);
                }
            }
            *self.this.cache.borrow_mut() = None;
            *self.this.source.borrow_mut() = None;
            *self.this.stream.borrow_mut() = None;
            Ok(())
        });
        Ok(())
    }

    fn SetFocus(&self) -> Result<()> {
        let child = self.this.child_hwnd.get();
        if child.is_invalid() {
            return Err(windows::core::Error::from_hresult(S_FALSE));
        }
        unsafe {
            let _ = SetFocus(child);
        }
        Ok(())
    }

    fn QueryFocus(&self) -> Result<HWND> {
        let focus = unsafe { GetFocus() };
        if focus.is_invalid() {
            Err(windows::core::Error::from_hresult(S_FALSE))
        } else {
            Ok(focus)
        }
    }

    fn TranslateAccelerator(&self, _pmsg: *const MSG) -> Result<()> {
        // We never intercept accelerators. S_FALSE = "not handled".
        Err(windows::core::Error::from_hresult(S_FALSE))
    }
}

// =============================================================================
// Window creation
// =============================================================================

impl ArcThumbPreviewHandler_Impl {
    fn create_child_window(&self) -> Result<()> {
        let parent = self.this.parent_hwnd.get();
        if parent.is_invalid() {
            return Err(windows::core::Error::from_hresult(E_FAIL));
        }
        let atom = register_window_class();
        if atom == 0 {
            return Err(windows::core::Error::from_hresult(E_FAIL));
        }
        let r = self.this.rect.get();
        let width = (r.right - r.left).max(1);
        let height = (r.bottom - r.top).max(1);

        // Pass a pointer to the user struct (`self.this`) so the
        // window proc can recover us via GWLP_USERDATA in WM_NCCREATE.
        let user_ptr: *const ArcThumbPreviewHandler =
            &self.this as *const ArcThumbPreviewHandler;

        let hinstance: HINSTANCE = unsafe {
            GetModuleHandleW(None)
                .map(|h| HINSTANCE(h.0))
                .unwrap_or_default()
        };

        let hwnd = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(atom as usize as *const u16),
                w!(""),
                WS_CHILD | WS_VISIBLE,
                r.left,
                r.top,
                width,
                height,
                parent,
                None,
                hinstance,
                Some(user_ptr as *const c_void),
            )
        }
        .map_err(|e| {
            alog!("Preview: CreateWindowExW failed: {e}");
            windows::core::Error::from_hresult(E_FAIL)
        })?;

        self.this.child_hwnd.set(hwnd);
        unsafe {
            let _ = InvalidateRect(hwnd, None, true);
        }
        Ok(())
    }
}

// =============================================================================
// Window class registration (one per process)
// =============================================================================

fn register_window_class() -> u16 {
    static ATOM: OnceLock<u16> = OnceLock::new();
    *ATOM.get_or_init(|| {
        let hmodule = unsafe { GetModuleHandleW(None).unwrap_or_default() };
        let hinstance = HINSTANCE(hmodule.0);
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() };
        let wcex = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(preview_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: Default::default(),
            hCursor: cursor,
            hbrBackground: HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: w!("ArcThumbPreviewWindow"),
            hIconSm: Default::default(),
        };
        unsafe { RegisterClassExW(&wcex) }
    })
}

// =============================================================================
// Window procedure + paint
// =============================================================================

unsafe extern "system" fn preview_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_NCCREATE => {
                // Stash the user pointer we passed via lpCreateParams.
                let cs = lparam.0 as *const CREATESTRUCTW;
                if !cs.is_null() {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as isize);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_PAINT => {
                let ptr =
                    GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const ArcThumbPreviewHandler;
                // Wrap the body in catch_unwind so a panic in resize/GDI
                // can't escape into prevhost's window message loop.
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    if !ptr.is_null() {
                        paint(hwnd, &*ptr);
                    } else {
                        // No user state — at least clear the background
                        // so the pane isn't garbage.
                        let mut ps = PAINTSTRUCT::default();
                        let hdc = BeginPaint(hwnd, &mut ps);
                        let mut rc = RECT::default();
                        let _ = GetClientRect(hwnd, &mut rc);
                        let brush = system_window_brush();
                        FillRect(hdc, &rc, brush);
                        let _ = DeleteObject(HGDIOBJ(brush.0));
                        let _ = EndPaint(hwnd, &ps);
                    }
                }));
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1), // we erase in WM_PAINT
            WM_DESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Build a brush for the system window-background colour. Caller
/// must `DeleteObject` it after use. We can't use the standard
/// `(COLOR_WINDOW + 1)` HBRUSH trick portably across windows-rs
/// 0.58 — `CreateSolidBrush` is more obviously correct.
fn system_window_brush() -> HBRUSH {
    let color = unsafe { GetSysColor(COLOR_WINDOW) };
    unsafe { CreateSolidBrush(COLORREF(color)) }
}

fn paint(hwnd: HWND, this: &ArcThumbPreviewHandler) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };

    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let cw = client.right - client.left;
    let ch = client.bottom - client.top;

    // Erase background.
    let brush = system_window_brush();
    unsafe { FillRect(hdc, &client, brush) };
    unsafe {
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }

    // Build (or reuse) the cached bitmap for the current size.
    let source = this.source.borrow();
    if let Some(img) = source.as_ref() {
        let (dest_w, dest_h, off_x, off_y) = fit_inside(img.width(), img.height(), cw, ch);
        if dest_w > 0 && dest_h > 0 {
            let mut cache = this.cache.borrow_mut();
            let needs_rebuild = cache
                .as_ref()
                .map(|c| c.width != dest_w || c.height != dest_h)
                .unwrap_or(true);
            if needs_rebuild {
                let resized = img
                    .resize_exact(
                        dest_w as u32,
                        dest_h as u32,
                        image::imageops::FilterType::Triangle,
                    )
                    .to_rgba8();
                if let Ok(hbmp) = bitmap::from_rgba(&resized) {
                    *cache = Some(CachedBitmap {
                        width: dest_w,
                        height: dest_h,
                        hbitmap: hbmp,
                    });
                }
            }
            if let Some(c) = cache.as_ref() {
                unsafe {
                    let mem_dc = CreateCompatibleDC(hdc);
                    let old = SelectObject(mem_dc, HGDIOBJ(c.hbitmap.0));
                    let _ = BitBlt(
                        hdc, off_x, off_y, c.width, c.height, mem_dc, 0, 0, SRCCOPY,
                    );
                    SelectObject(mem_dc, old);
                    let _ = DeleteDC(mem_dc);
                }
            }
        }
    }

    let _ = unsafe { EndPaint(hwnd, &ps) };
}

/// Aspect-fit `(src_w, src_h)` inside a `(box_w, box_h)` rectangle,
/// returning `(dest_w, dest_h, x_offset, y_offset)` for centering.
/// Pure function — easy to unit test.
fn fit_inside(src_w: u32, src_h: u32, box_w: i32, box_h: i32) -> (i32, i32, i32, i32) {
    if src_w == 0 || src_h == 0 || box_w <= 0 || box_h <= 0 {
        return (0, 0, 0, 0);
    }
    let scale_x = box_w as f64 / src_w as f64;
    let scale_y = box_h as f64 / src_h as f64;
    let scale = scale_x.min(scale_y);
    let dest_w = (src_w as f64 * scale).round() as i32;
    let dest_h = (src_h as f64 * scale).round() as i32;
    let off_x = (box_w - dest_w) / 2;
    let off_y = (box_h - dest_h) / 2;
    (dest_w, dest_h, off_x, off_y)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_inside_square_in_square() {
        // 100×100 inside 200×200 → scaled to 200×200, no offset.
        assert_eq!(fit_inside(100, 100, 200, 200), (200, 200, 0, 0));
    }

    #[test]
    fn fit_inside_landscape_in_square() {
        // 100×50 → fills width, top/bottom letterboxed.
        assert_eq!(fit_inside(100, 50, 200, 200), (200, 100, 0, 50));
    }

    #[test]
    fn fit_inside_portrait_in_square() {
        // 50×100 → fills height, left/right pillarboxed.
        assert_eq!(fit_inside(50, 100, 200, 200), (100, 200, 50, 0));
    }

    #[test]
    fn fit_inside_smaller_source_still_scales_up() {
        // 40×20 inside 200×200 → scale=5x → 200×100, offset y=50.
        assert_eq!(fit_inside(40, 20, 200, 200), (200, 100, 0, 50));
    }

    #[test]
    fn fit_inside_zero_source() {
        assert_eq!(fit_inside(0, 100, 200, 200), (0, 0, 0, 0));
        assert_eq!(fit_inside(100, 0, 200, 200), (0, 0, 0, 0));
    }

    #[test]
    fn fit_inside_zero_box() {
        assert_eq!(fit_inside(100, 100, 0, 200), (0, 0, 0, 0));
        assert_eq!(fit_inside(100, 100, 200, 0), (0, 0, 0, 0));
        assert_eq!(fit_inside(100, 100, -1, 200), (0, 0, 0, 0));
    }

    #[test]
    fn fit_inside_non_square_box() {
        // 100×100 inside 400×200 → constrained by height, → 200×200,
        // centered horizontally.
        assert_eq!(fit_inside(100, 100, 400, 200), (200, 200, 100, 0));
    }

    #[test]
    fn fit_inside_centers_when_aspect_matches() {
        // 100×50 inside 200×100 → exact fit.
        assert_eq!(fit_inside(100, 50, 200, 100), (200, 100, 0, 0));
    }
}
