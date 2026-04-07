//! Find `arcthumb.dll` relative to this config exe.
//!
//! Lookup order:
//! 1. Next to the current executable (the expected deployment layout).
//! 2. The path in `HKCU\Software\Classes\CLSID\{CLSID}\InprocServer32\(Default)`
//!    — useful if the user previously installed via `regsvr32` from a
//!    different location.
//! 3. Otherwise: `Err` with a message the UI shows to the user.

use std::path::PathBuf;

use arcthumb::registry;

pub fn resolve_dll_path() -> Result<PathBuf, String> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("arcthumb.dll");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    if let Some(p) = registry::read_registered_dll_path() {
        if p.is_file() {
            return Ok(p);
        }
    }
    Err("arcthumb.dll not found next to the exe or in the registry".into())
}
