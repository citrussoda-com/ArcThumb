//! COM objects: `ArcThumbClassFactory` and `ArcThumbProvider`.
//!
//! - `ArcThumbClassFactory` implements `IClassFactory`. It is the thing
//!   `DllGetClassObject` hands back, and its only job is to create
//!   fresh `ArcThumbProvider` instances on demand.
//!
//! - `ArcThumbProvider` implements `IInitializeWithStream` (Explorer
//!   gives us a stream over the target file) and `IThumbnailProvider`
//!   (Explorer asks us for an HBITMAP of a given size).
//!
//! Phase 1 ignores the stream entirely and always returns a solid-color
//! dummy bitmap. Phase 2 will actually parse the ZIP from the stream
//! and decode the first image.

use std::cell::RefCell;
use std::ffi::c_void;

use windows::core::{implement, IUnknown, Interface, Result, GUID};
use windows::Win32::Foundation::{BOOL, CLASS_E_NOAGGREGATION, E_POINTER};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl, IStream};
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithStream, IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{
    IThumbnailProvider, IThumbnailProvider_Impl, WTSAT_RGB, WTS_ALPHATYPE,
};

use crate::bitmap;

/// CLSID for the ArcThumb thumbnail provider COM class.
/// **DO NOT CHANGE** — baked into users' registries on install.
pub const CLSID_ARCTHUMB_PROVIDER: GUID =
    GUID::from_u128(0x0F4F5659_D383_4945_A534_01E1EED1D23F);

// =============================================================================
// IClassFactory
// =============================================================================

#[implement(IClassFactory)]
pub struct ArcThumbClassFactory;

impl IClassFactory_Impl for ArcThumbClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        // COM aggregation is an advanced feature we don't support.
        if punkouter.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        if ppvobject.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }

        unsafe {
            *ppvobject = std::ptr::null_mut();
            // Create a fresh provider and hand it to the caller under
            // whatever interface they asked for (QueryInterface).
            let provider = ArcThumbProvider::default();
            let unknown: IUnknown = provider.into();
            unknown.query(&*riid, ppvobject).ok()
        }
    }

    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        // No-op: we don't care whether the server is locked.
        Ok(())
    }
}

// =============================================================================
// ArcThumbProvider — IThumbnailProvider + IInitializeWithStream
// =============================================================================

/// The COM object Explorer actually talks to for each thumbnail request.
///
/// `stream` is populated by `IInitializeWithStream::Initialize`, then
/// consumed (eventually) by `IThumbnailProvider::GetThumbnail`. Phase 1
/// stores it but never reads from it.
#[implement(IThumbnailProvider, IInitializeWithStream)]
#[derive(Default)]
pub struct ArcThumbProvider {
    stream: RefCell<Option<IStream>>,
}

impl IInitializeWithStream_Impl for ArcThumbProvider_Impl {
    fn Initialize(&self, pstream: Option<&IStream>, _grfmode: u32) -> Result<()> {
        *self.this.stream.borrow_mut() = pstream.cloned();
        Ok(())
    }
}

impl IThumbnailProvider_Impl for ArcThumbProvider_Impl {
    fn GetThumbnail(
        &self,
        cx: u32,
        phbmp: *mut HBITMAP,
        pdwalpha: *mut WTS_ALPHATYPE,
    ) -> Result<()> {
        if phbmp.is_null() || pdwalpha.is_null() {
            return Err(E_POINTER.into());
        }

        // Clamp to a sane range so malicious or weird requests can't
        // make us allocate gigabytes.
        let size = cx.clamp(16, 1024) as i32;

        // Phase 1: ignore the stream, always return solid cyan-ish blue.
        // 0xAARRGGBB = fully opaque, R=0x33, G=0x99, B=0xFF.
        let hbmp = bitmap::create_solid_color(size, 0xFF3399FF)?;

        unsafe {
            *phbmp = hbmp;
            *pdwalpha = WTSAT_RGB;
        }
        Ok(())
    }
}
