//! Install / uninstall the shell extension into the Windows registry.
//!
//! We write to `HKCU\Software\Classes` rather than `HKLM` so that the
//! DLL can be registered without admin rights. This also means the
//! thumbnail provider is per-user.
//!
//! Registry layout after a successful full install:
//!
//! ```text
//! HKCU\Software\Classes\CLSID\{CLSID_ARCTHUMB}
//!     (Default)                = "ArcThumb Thumbnail Provider"
//!     InprocServer32\
//!         (Default)            = "C:\path\to\arcthumb.dll"
//!         ThreadingModel       = "Apartment"
//!
//! HKCU\Software\Classes\.zip\ShellEx\{IID_IThumbnailProvider}
//!     (Default)                = "{CLSID_ARCTHUMB}"
//! ```
//!
//! ## Two callers
//!
//! Both the DLL's `DllRegisterServer` and the separate `arcthumb-config`
//! binary share this module. The DLL uses `register()` / `unregister()`
//! which auto-detect their own path via `GetModuleHandleExW`. The config
//! exe uses the individual primitives (`register_clsid(path)`,
//! `register_extension(ext)`, `is_extension_registered(ext)`, …) so it
//! can install selectively and reflect current state in the GUI.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
    GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
};
use winreg::enums::*;
use winreg::RegKey;

/// String form of the ArcThumb thumbnail provider CLSID (defined in
/// `com.rs`). **Never change** — baked into users' registries.
pub const CLSID_STR: &str = "{0F4F5659-D383-4945-A534-01E1EED1D23F}";

/// Standard IID of `IThumbnailProvider`. Explorer looks under
/// `.<ext>\ShellEx\<this IID>` to find the thumbnail handler.
pub const IID_ITHUMBNAILPROVIDER: &str = "{E357FCCD-A995-4576-B01F-234630154E96}";

/// File extensions that ArcThumb handles.
/// The `.cb?` variants are the comic-book archive conventions used by
/// tools like ComicRack — structurally identical to their base format,
/// just with a different extension.
pub const EXTENSIONS: &[&str] = &[
    ".zip", ".cbz",
    ".rar", ".cbr",
    ".7z", ".cb7",
    ".cbt",
];

// =============================================================================
// Private helpers
// =============================================================================

/// Build the registry sub-path for a given extension's ShellEx slot.
fn ext_shellex_path(ext: &str) -> String {
    format!("Software\\Classes\\{ext}\\ShellEx\\{IID_ITHUMBNAILPROVIDER}")
}

/// Build the registry sub-path for the CLSID root.
fn clsid_root_path() -> String {
    format!("Software\\Classes\\CLSID\\{CLSID_STR}")
}

/// Resolve the calling DLL's own path via `GetModuleHandleExW` — only
/// meaningful when this code is running inside `arcthumb.dll`. The
/// config exe must NOT call this; it would return the exe's path.
fn get_dll_path_from_module() -> io::Result<String> {
    unsafe {
        let mut hmodule = HMODULE::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(get_dll_path_from_module as *const () as *const u16),
            &mut hmodule,
        )
        .map_err(|e| io::Error::other(format!("GetModuleHandleExW failed: {e}")))?;

        let mut buf = vec![0u16; 32768];
        let len = GetModuleFileNameW(hmodule, &mut buf) as usize;
        if len == 0 {
            return Err(io::Error::other("GetModuleFileNameW returned 0"));
        }
        Ok(OsString::from_wide(&buf[..len])
            .to_string_lossy()
            .into_owned())
    }
}

// =============================================================================
// Public primitives (used by both the DLL and the config exe)
// =============================================================================

/// Write the CLSID subtree (`HKCU\Software\Classes\CLSID\{CLSID}`)
/// including the `InprocServer32` entry pointing at `dll_path`.
pub fn register_clsid(dll_path: &Path) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (clsid_key, _) = hkcu.create_subkey(clsid_root_path())?;
    clsid_key.set_value("", &"ArcThumb Thumbnail Provider")?;

    let (inproc, _) = clsid_key.create_subkey("InprocServer32")?;
    let dll_path_str = dll_path.to_string_lossy().into_owned();
    inproc.set_value("", &dll_path_str)?;
    inproc.set_value("ThreadingModel", &"Apartment")?;
    Ok(())
}

/// Delete the CLSID subtree. Best effort: succeeds even if the tree
/// was already absent.
pub fn unregister_clsid() -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey_all(clsid_root_path());
    Ok(())
}

/// Wire a single file extension to our CLSID in the ShellEx slot.
/// `ext` must start with a dot, e.g. `".zip"`.
pub fn register_extension(ext: &str) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (key, _) = hkcu.create_subkey(ext_shellex_path(ext))?;
    key.set_value("", &CLSID_STR.to_string())?;
    Ok(())
}

/// Remove the ShellEx binding for a single extension. No error if
/// the key is already gone.
pub fn unregister_extension(ext: &str) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let _ = hkcu.delete_subkey_all(ext_shellex_path(ext));
    Ok(())
}

/// True iff the ShellEx IID subkey currently exists for this extension.
pub fn is_extension_registered(ext: &str) -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(ext_shellex_path(ext)).is_ok()
}

/// True iff the CLSID's `InprocServer32` subkey exists (our canonical
/// "shell extension is installed" signal).
pub fn is_clsid_registered() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(format!("{}\\InprocServer32", clsid_root_path()))
        .is_ok()
}

/// Read back `HKCU\Software\Classes\CLSID\{CLSID}\InprocServer32\(Default)`.
/// Used by the config exe as a fallback when the DLL isn't next to it.
pub fn read_registered_dll_path() -> Option<PathBuf> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey(format!("{}\\InprocServer32", clsid_root_path()))
        .ok()?;
    let path: String = key.get_value("").ok()?;
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

// =============================================================================
// Backward-compatible wrappers used by DllRegisterServer / DllUnregisterServer
// =============================================================================

pub fn register() -> io::Result<()> {
    let dll_path_str = get_dll_path_from_module()?;
    register_clsid(Path::new(&dll_path_str))?;
    for ext in EXTENSIONS {
        register_extension(ext)?;
    }
    Ok(())
}

pub fn unregister() -> io::Result<()> {
    for ext in EXTENSIONS {
        let _ = unregister_extension(ext);
    }
    let _ = unregister_clsid();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_path_format() {
        assert_eq!(
            ext_shellex_path(".zip"),
            "Software\\Classes\\.zip\\ShellEx\\{E357FCCD-A995-4576-B01F-234630154E96}"
        );
    }

    #[test]
    fn clsid_root_format() {
        assert_eq!(
            clsid_root_path(),
            "Software\\Classes\\CLSID\\{0F4F5659-D383-4945-A534-01E1EED1D23F}"
        );
    }
}
