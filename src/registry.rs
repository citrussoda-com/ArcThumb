//! Install / uninstall the shell extension into the Windows registry.
//!
//! ArcThumb supports two install scopes:
//!
//! * [`Scope::PerUser`] writes to `HKCU\Software\Classes`. No admin
//!   rights required, registration is per-user. Default for the
//!   non-elevated installer path.
//! * [`Scope::PerMachine`] writes to `HKLM\Software\Classes`. Requires
//!   admin rights. Required when Explorer runs at High Mandatory
//!   Integrity (Windows Sandbox, some enterprise lockdowns), because
//!   Microsoft's COM-hijacking defence makes that Explorer ignore HKCU
//!   CLSID entries entirely.
//!
//! The path layout under each hive's `Software\Classes` subtree is
//! identical, so all the path builders are scope-agnostic — only the
//! root key changes.
//!
//! Registry layout after a successful full install:
//!
//! ```text
//! HK??\Software\Classes\CLSID\{CLSID_ARCTHUMB}
//!     (Default)                = "ArcThumb Thumbnail Provider"
//!     InprocServer32\
//!         (Default)            = "C:\path\to\arcthumb.dll"
//!         ThreadingModel       = "Apartment"
//!
//! HK??\Software\Classes\.zip\ShellEx\{IID_IThumbnailProvider}
//!     (Default)                = "{CLSID_ARCTHUMB}"
//! ```
//!
//! ## Two callers
//!
//! Both the DLL's `DllRegisterServer` and the separate `arcthumb-config`
//! binary share this module. The DLL uses `register()` / `unregister()`
//! which auto-detect their own path via `GetModuleHandleExW` and pick a
//! scope from the current process's elevation. The config exe uses the
//! individual primitives (`register_clsid(scope, path)`,
//! `register_extension(scope, ext)`, `is_extension_registered(scope, ext)`,
//! …) so it can install selectively and reflect current state in the GUI.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    GetModuleFileNameW, GetModuleHandleExW,
};
use windows::core::PCWSTR;
use winreg::RegKey;
use winreg::enums::*;

use crate::elevation;

// =============================================================================
// Constants
// =============================================================================

/// String form of the ArcThumb thumbnail provider CLSID (defined in
/// `com.rs`). **Never change** — baked into users' registries.
pub const CLSID_STR: &str = "{0F4F5659-D383-4945-A534-01E1EED1D23F}";

/// String form of the ArcThumb preview handler CLSID (defined in
/// `preview.rs`). **Never change** — baked into users' registries.
pub const PREVIEW_CLSID_STR: &str = "{8C7C1E5F-3D4A-4E2B-9F1A-7B5D6E8F9A0C}";

/// Standard IID of `IThumbnailProvider`. Explorer looks under
/// `.<ext>\ShellEx\<this IID>` to find the thumbnail handler.
const IID_ITHUMBNAILPROVIDER: &str = "{E357FCCD-A995-4576-B01F-234630154E96}";

/// Standard IID of `IPreviewHandler`. Explorer looks under
/// `.<ext>\ShellEx\<this IID>` to find the preview handler.
const IID_IPREVIEWHANDLER: &str = "{8895B1C6-B41F-4C1C-A562-0D564250836F}";

/// AppID of the standard preview-host surrogate. Setting this on
/// the CLSID key tells COM to load our DLL inside `prevhost.exe`
/// (per-user, no admin needed; isolation handled by Windows).
const PREVHOST_APPID: &str = "{534A1E02-D58F-44f0-B58B-36CBED287C7C}";

/// File extensions that ArcThumb handles.
pub const EXTENSIONS: &[&str] = &[
    ".zip", ".cbz", ".rar", ".cbr", ".7z", ".cb7", ".cbt", ".epub", ".fb2", ".mobi", ".azw",
    ".azw3",
];

/// Extensions bound by every build that predates the `KnownExtensions`
/// value. An install with no recorded list is assumed to have known
/// exactly these. **Frozen**: new formats go in [`EXTENSIONS`] only.
pub const PRE_TRACKING_EXTENSIONS: &[&str] = &[
    ".zip", ".cbz", ".rar", ".cbr", ".7z", ".cb7", ".cbt", ".epub", ".fb2", ".mobi", ".azw",
    ".azw3",
];

/// Value in a `.ext\ShellEx\{IID}` key holding the CLSID that was
/// bound there before ArcThumb took the slot.
const PREVIOUS_HANDLER_VALUE: &str = "ArcThumbPrevious";

/// Value on the thumbnail CLSID key holding the space-separated
/// extension list the installed build knew about.
const KNOWN_EXTENSIONS_VALUE: &str = "KnownExtensions";

/// Production parent key for shell extension registrations.
const CLASSES_BASE: &str = "Software\\Classes";

/// Production parent key for COM CLSIDs.
const CLSID_BASE: &str = "Software\\Classes\\CLSID";

// =============================================================================
// Scope — per-user vs per-machine install hive selection
// =============================================================================

/// Which registry hive to read or write.
///
/// `PerUser` targets `HKCU\Software\Classes` and works without admin
/// rights. `PerMachine` targets `HKLM\Software\Classes` and requires
/// elevation; chosen automatically when the installer runs as admin.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    PerUser,
    PerMachine,
}

impl Scope {
    /// Both hives, in the order to **try for reads** (machine first
    /// since HKLM-installed entries take precedence at COM lookup time
    /// for elevated callers).
    pub const ALL: &'static [Scope] = &[Scope::PerMachine, Scope::PerUser];

    fn root_key(self) -> RegKey {
        match self {
            Scope::PerUser => RegKey::predef(HKEY_CURRENT_USER),
            Scope::PerMachine => RegKey::predef(HKEY_LOCAL_MACHINE),
        }
    }
}

// =============================================================================
// HandlerKind — captures the difference between thumbnail and preview
// =============================================================================

/// Describes one of the two COM handlers ArcThumb registers.
/// All handler-specific logic is parameterized over this, eliminating
/// the thumbnail/preview code duplication.
struct HandlerKind {
    clsid_str: &'static str,
    iid_shellex: &'static str,
    display_name: &'static str,
    app_id: Option<&'static str>,
}

const THUMBNAIL: HandlerKind = HandlerKind {
    clsid_str: CLSID_STR,
    iid_shellex: IID_ITHUMBNAILPROVIDER,
    display_name: "ArcThumb Thumbnail Provider",
    app_id: None,
};

const PREVIEW: HandlerKind = HandlerKind {
    clsid_str: PREVIEW_CLSID_STR,
    iid_shellex: IID_IPREVIEWHANDLER,
    display_name: "ArcThumb Preview Handler",
    app_id: Some(PREVHOST_APPID),
};

// =============================================================================
// Path builders
// =============================================================================

/// Build the registry sub-path for a given extension's ShellEx slot.
fn ext_shellex_path_under(classes_base: &str, ext: &str, iid: &str) -> String {
    format!("{classes_base}\\{ext}\\ShellEx\\{iid}")
}

/// Build the registry sub-path for the CLSID root.
fn clsid_root_path_under(clsid_base: &str, clsid_str: &str) -> String {
    format!("{clsid_base}\\{clsid_str}")
}

impl HandlerKind {
    fn clsid_root(&self) -> String {
        clsid_root_path_under(CLSID_BASE, self.clsid_str)
    }

    fn ext_shellex_path(&self, ext: &str) -> String {
        ext_shellex_path_under(CLASSES_BASE, ext, self.iid_shellex)
    }
}

// =============================================================================
// Registry primitives (parameterized by scope + sub-path for testability)
// =============================================================================

fn register_clsid_at(
    root: &RegKey,
    clsid_root: &str,
    dll_path: &Path,
    handler: &HandlerKind,
) -> io::Result<()> {
    let (clsid_key, _) = root.create_subkey(clsid_root)?;
    clsid_key.set_value("", &handler.display_name)?;

    if let Some(app_id) = handler.app_id {
        clsid_key.set_value("AppID", &app_id.to_string())?;
    }

    let (inproc, _) = clsid_key.create_subkey("InprocServer32")?;
    let dll_path_str = dll_path.to_string_lossy().into_owned();
    inproc.set_value("", &dll_path_str)?;
    inproc.set_value("ThreadingModel", &"Apartment")?;
    Ok(())
}

fn unregister_clsid_at(root: &RegKey, clsid_root: &str) -> io::Result<()> {
    // NotFound is idempotent: the caller asked for the key to be gone
    // and it already is. Anything else (AccessDenied being the
    // important case for HKLM from a non-elevated GUI) bubbles up so
    // Apply can show an error instead of silently lying to the user.
    match root.delete_subkey_all(clsid_root) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The CLSID bound in a ShellEx slot, if the key exists and names one.
fn bound_handler(key: &RegKey) -> Option<String> {
    key.get_value::<String, _>("")
        .ok()
        .filter(|v| !v.is_empty())
}

/// Bind `clsid_str` in a ShellEx slot. A slot holds one handler, so if
/// another program's is already there it is remembered in
/// [`PREVIOUS_HANDLER_VALUE`] and put back by
/// [`unregister_extension_at`].
fn register_extension_at(root: &RegKey, shellex_path: &str, clsid_str: &str) -> io::Result<()> {
    let (key, _) = root.create_subkey(shellex_path)?;
    if let Some(other) = bound_handler(&key).filter(|v| !v.eq_ignore_ascii_case(clsid_str)) {
        key.set_value(PREVIOUS_HANDLER_VALUE, &other)?;
    }
    key.set_value("", &clsid_str.to_string())?;
    Ok(())
}

/// Undo [`register_extension_at`]. Only a slot that still names
/// `clsid_str` is touched: the handler that was there before comes
/// back, or the key goes away if there was none. A slot some other
/// program has taken over since is left alone.
fn unregister_extension_at(root: &RegKey, shellex_path: &str, clsid_str: &str) -> io::Result<()> {
    let key = match root.open_subkey_with_flags(shellex_path, KEY_READ | KEY_WRITE) {
        Ok(key) => key,
        // NotFound is idempotent: the binding is already gone.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let previous = key.get_value::<String, _>(PREVIOUS_HANDLER_VALUE).ok();

    if bound_handler(&key).is_some_and(|v| !v.eq_ignore_ascii_case(clsid_str)) {
        // Not ours any more. The remembered handler is stale too.
        if previous.is_some() {
            key.delete_value(PREVIOUS_HANDLER_VALUE)?;
        }
        return Ok(());
    }

    match previous {
        Some(previous) => {
            key.set_value("", &previous)?;
            key.delete_value(PREVIOUS_HANDLER_VALUE)
        }
        None => {
            drop(key);
            match root.delete_subkey_all(shellex_path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }
}

/// True iff the ShellEx slot exists and names `clsid_str`.
fn is_extension_bound_at(root: &RegKey, shellex_path: &str, clsid_str: &str) -> bool {
    root.open_subkey(shellex_path)
        .ok()
        .and_then(|key| bound_handler(&key))
        .is_some_and(|v| v.eq_ignore_ascii_case(clsid_str))
}

fn is_subkey_present(root: &RegKey, path: &str) -> bool {
    root.open_subkey(path).is_ok()
}

fn read_inproc_default(root: &RegKey, clsid_root: &str) -> Option<PathBuf> {
    let key = root
        .open_subkey(format!("{clsid_root}\\InprocServer32"))
        .ok()?;
    let path: String = key.get_value("").ok()?;
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

/// Resolve the calling DLL's own path via `GetModuleHandleExW` — only
/// meaningful when this code is running inside `arcthumb.dll`. The
/// config exe must NOT call this; it would return the exe's path.
fn get_dll_path_from_module() -> io::Result<String> {
    unsafe {
        let mut hmodule = HMODULE::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(get_dll_path_from_module as *const () as *const u16),
            &mut hmodule,
        )
        .map_err(|e| io::Error::other(format!("GetModuleHandleExW failed: {e}")))?;

        let mut buf = vec![0u16; 32768];
        let len = GetModuleFileNameW(Some(hmodule), &mut buf) as usize;
        if len == 0 {
            return Err(io::Error::other("GetModuleFileNameW returned 0"));
        }
        Ok(OsString::from_wide(&buf[..len])
            .to_string_lossy()
            .into_owned())
    }
}

// =============================================================================
// Public API — thumbnail provider
// =============================================================================

/// Write the CLSID subtree (`HK??\Software\Classes\CLSID\{CLSID}`)
/// including the `InprocServer32` entry pointing at `dll_path`.
pub fn register_clsid(scope: Scope, dll_path: &Path) -> io::Result<()> {
    register_clsid_at(
        &scope.root_key(),
        &THUMBNAIL.clsid_root(),
        dll_path,
        &THUMBNAIL,
    )
}

/// Delete the CLSID subtree. Best effort: succeeds even if the tree
/// was already absent.
pub fn unregister_clsid(scope: Scope) -> io::Result<()> {
    unregister_clsid_at(&scope.root_key(), &THUMBNAIL.clsid_root())
}

/// Wire a single file extension to our CLSID in the ShellEx slot.
/// `ext` must start with a dot, e.g. `".zip"`.
pub fn register_extension(scope: Scope, ext: &str) -> io::Result<()> {
    register_extension_at(
        &scope.root_key(),
        &THUMBNAIL.ext_shellex_path(ext),
        CLSID_STR,
    )
}

/// Remove our ShellEx binding for a single extension, restoring the
/// handler it replaced if there was one. No error if the key is
/// already gone or now belongs to another program.
pub fn unregister_extension(scope: Scope, ext: &str) -> io::Result<()> {
    unregister_extension_at(
        &scope.root_key(),
        &THUMBNAIL.ext_shellex_path(ext),
        CLSID_STR,
    )
}

/// True iff this extension's thumbnail slot in the given scope names
/// our CLSID. A slot bound to another program's handler is not ours.
pub fn is_extension_registered(scope: Scope, ext: &str) -> bool {
    is_extension_bound_at(
        &scope.root_key(),
        &THUMBNAIL.ext_shellex_path(ext),
        CLSID_STR,
    )
}

/// True iff the CLSID's `InprocServer32` subkey exists in the given
/// scope (our canonical "shell extension is installed" signal).
pub fn is_clsid_registered(scope: Scope) -> bool {
    is_subkey_present(
        &scope.root_key(),
        &format!("{}\\InprocServer32", THUMBNAIL.clsid_root()),
    )
}

/// Record, on the thumbnail CLSID key, the extension list this build
/// knows about. The next `--install` reads it back to tell "the user
/// switched this extension off" (known, binding absent) from "this
/// extension is new in the build being installed" (not known yet).
/// Lives under the CLSID key so uninstall removes it with the rest.
pub fn write_known_extensions(scope: Scope) -> io::Result<()> {
    write_known_extensions_at(&scope.root_key(), &THUMBNAIL.clsid_root())
}

/// The list written by [`write_known_extensions`], or `None` when the
/// value is missing (nothing installed, or installed by a build that
/// predates it).
pub fn read_known_extensions(scope: Scope) -> Option<Vec<String>> {
    read_known_extensions_at(&scope.root_key(), &THUMBNAIL.clsid_root())
}

fn write_known_extensions_at(root: &RegKey, clsid_root: &str) -> io::Result<()> {
    let (key, _) = root.create_subkey(clsid_root)?;
    key.set_value(KNOWN_EXTENSIONS_VALUE, &EXTENSIONS.join(" "))
}

fn read_known_extensions_at(root: &RegKey, clsid_root: &str) -> Option<Vec<String>> {
    let key = root.open_subkey(clsid_root).ok()?;
    let list: String = key.get_value(KNOWN_EXTENSIONS_VALUE).ok()?;
    Some(list.split_whitespace().map(str::to_string).collect())
}

/// Read back `HK??\Software\Classes\CLSID\{CLSID}\InprocServer32\(Default)`.
/// Tries each scope in `scopes` in order and returns the first hit.
/// Used by the config exe as a fallback when the DLL isn't next to it.
pub fn read_registered_dll_path(scopes: &[Scope]) -> Option<PathBuf> {
    for scope in scopes {
        if let Some(p) = read_inproc_default(&scope.root_key(), &THUMBNAIL.clsid_root()) {
            return Some(p);
        }
    }
    None
}

// =============================================================================
// Public API — preview handler
// =============================================================================

/// Write the preview-handler CLSID subtree, including the AppID
/// pointing at prevhost.exe and the InprocServer32 entry.
pub fn register_preview_clsid(scope: Scope, dll_path: &Path) -> io::Result<()> {
    register_clsid_at(&scope.root_key(), &PREVIEW.clsid_root(), dll_path, &PREVIEW)
}

/// Delete the preview-handler CLSID subtree. Best effort.
pub fn unregister_preview_clsid(scope: Scope) -> io::Result<()> {
    unregister_clsid_at(&scope.root_key(), &PREVIEW.clsid_root())
}

/// Wire one extension to the preview-handler CLSID via its
/// `IPreviewHandler` ShellEx slot.
pub fn register_preview_extension(scope: Scope, ext: &str) -> io::Result<()> {
    register_extension_at(
        &scope.root_key(),
        &PREVIEW.ext_shellex_path(ext),
        PREVIEW_CLSID_STR,
    )
}

/// Remove the preview-handler ShellEx binding for an extension.
pub fn unregister_preview_extension(scope: Scope, ext: &str) -> io::Result<()> {
    unregister_extension_at(
        &scope.root_key(),
        &PREVIEW.ext_shellex_path(ext),
        PREVIEW_CLSID_STR,
    )
}

/// True iff the preview-handler `InprocServer32` subkey exists in the
/// given scope. Used as the source of truth for the GUI's
/// "Enable preview pane" toggle.
pub fn is_preview_enabled(scope: Scope) -> bool {
    is_subkey_present(
        &scope.root_key(),
        &format!("{}\\InprocServer32", PREVIEW.clsid_root()),
    )
}

/// Pick the first scope (in `Scope::ALL` order) where the thumbnail
/// CLSID is registered. Used by the GUI to decide which hive's state
/// to display when both could in theory be present.
pub fn detect_installed_scope() -> Option<Scope> {
    Scope::ALL.iter().copied().find(|&s| is_clsid_registered(s))
}

// =============================================================================
// Backward-compatible wrappers used by DllRegisterServer / DllUnregisterServer
// =============================================================================

/// Register the DLL into whichever hive matches the current process's
/// elevation: HKLM when admin, HKCU otherwise. Called by `regsvr32`.
pub fn register() -> io::Result<()> {
    let scope = elevation::current_scope();
    let dll_path_str = get_dll_path_from_module()?;
    let dll_path = Path::new(&dll_path_str);
    register_clsid(scope, dll_path)?;
    register_preview_clsid(scope, dll_path)?;
    for ext in EXTENSIONS {
        register_extension(scope, ext)?;
        register_preview_extension(scope, ext)?;
    }
    notify_assoc_changed();
    Ok(())
}

/// Unregister from BOTH hives best-effort. We can't always tell which
/// one was used at install time (and a user may have switched modes
/// between versions), so we always try to clean both.
pub fn unregister() -> io::Result<()> {
    for scope in Scope::ALL.iter().copied() {
        for ext in EXTENSIONS {
            let _ = unregister_extension(scope, ext);
            let _ = unregister_preview_extension(scope, ext);
        }
        let _ = unregister_clsid(scope);
        let _ = unregister_preview_clsid(scope);
    }
    notify_assoc_changed();
    Ok(())
}

/// Tell the Shell that file-type associations changed so it invalidates
/// its icon and thumbnail caches and picks up our newly registered (or
/// removed) handlers without a reboot.
pub fn notify_assoc_changed() {
    use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------
    // Path format tests
    // ---------------------------------------------------------------

    #[test]
    fn thumbnail_extension_path_format() {
        assert_eq!(
            THUMBNAIL.ext_shellex_path(".zip"),
            "Software\\Classes\\.zip\\ShellEx\\{E357FCCD-A995-4576-B01F-234630154E96}"
        );
    }

    #[test]
    fn thumbnail_clsid_root_format() {
        assert_eq!(
            THUMBNAIL.clsid_root(),
            "Software\\Classes\\CLSID\\{0F4F5659-D383-4945-A534-01E1EED1D23F}"
        );
    }

    #[test]
    fn preview_extension_path_format() {
        assert_eq!(
            PREVIEW.ext_shellex_path(".zip"),
            "Software\\Classes\\.zip\\ShellEx\\{8895B1C6-B41F-4C1C-A562-0D564250836F}"
        );
    }

    #[test]
    fn preview_clsid_root_format() {
        assert_eq!(
            PREVIEW.clsid_root(),
            "Software\\Classes\\CLSID\\{8C7C1E5F-3D4A-4E2B-9F1A-7B5D6E8F9A0C}"
        );
    }

    #[test]
    fn ext_shellex_path_under_uses_provided_root() {
        let p = ext_shellex_path_under("Foo\\Bar", ".zip", IID_ITHUMBNAILPROVIDER);
        assert_eq!(
            p,
            "Foo\\Bar\\.zip\\ShellEx\\{E357FCCD-A995-4576-B01F-234630154E96}"
        );
    }

    #[test]
    fn clsid_root_path_under_uses_provided_root() {
        let p = clsid_root_path_under("Foo\\CLSID", "{XYZ}");
        assert_eq!(p, "Foo\\CLSID\\{XYZ}");
    }

    #[test]
    fn extensions_constant_is_non_empty_and_dotted() {
        assert!(!EXTENSIONS.is_empty());
        for ext in EXTENSIONS {
            assert!(ext.starts_with('.'), "{ext} must start with .");
            assert!(ext.len() >= 3, "{ext} too short");
        }
    }

    #[test]
    fn scope_per_user_root_is_hkcu() {
        // RegKey doesn't expose its predef HIVE; assert by writing a
        // throwaway key under each scope and reading back via the
        // matching predef.
        let sandbox = unique_sandbox("scope_per_user_root");
        let _guard = SandboxGuard(sandbox.clone());
        let path = format!("{sandbox}\\probe");

        Scope::PerUser
            .root_key()
            .create_subkey(&path)
            .expect("create under HKCU");

        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        assert!(hkcu.open_subkey(&path).is_ok(), "PerUser must hit HKCU");
    }

    #[test]
    fn scope_all_lists_machine_before_user() {
        // Order matters: read fallbacks must prefer HKLM (admin
        // installs win at COM lookup time for elevated callers).
        assert_eq!(Scope::ALL, &[Scope::PerMachine, Scope::PerUser]);
    }

    // ---------------------------------------------------------------
    // Hermetic registry roundtrips (HKCU only — HKLM needs admin)
    // ---------------------------------------------------------------

    fn unique_sandbox(tag: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("Software\\ArcThumbTest_{tag}_{pid}_{nanos}")
    }

    struct SandboxGuard(String);
    impl Drop for SandboxGuard {
        fn drop(&mut self) {
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let _ = hkcu.delete_subkey_all(&self.0);
        }
    }

    fn hkcu() -> RegKey {
        Scope::PerUser.root_key()
    }

    #[test]
    fn extension_register_roundtrip() {
        let sandbox = unique_sandbox("ext");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".zip", THUMBNAIL.iid_shellex);

        assert!(!is_subkey_present(&hkcu(), &path));

        register_extension_at(&hkcu(), &path, CLSID_STR).expect("register");
        assert!(is_subkey_present(&hkcu(), &path));

        let key = hkcu().open_subkey(&path).expect("open after register");
        let val: String = key.get_value("").expect("read default value");
        assert_eq!(val, CLSID_STR);

        unregister_extension_at(&hkcu(), &path, CLSID_STR).expect("unregister");
        assert!(!is_subkey_present(&hkcu(), &path));
    }

    #[test]
    fn thumbnail_clsid_register_roundtrip() {
        let sandbox = unique_sandbox("thumb_clsid");
        let _guard = SandboxGuard(sandbox.clone());
        let clsid_root = format!("{sandbox}\\{{TEST-CLSID}}");

        assert!(!is_subkey_present(&hkcu(), &clsid_root));

        let dll_path = std::path::PathBuf::from(r"C:\fake\arcthumb.dll");
        register_clsid_at(&hkcu(), &clsid_root, &dll_path, &THUMBNAIL).expect("register");

        let inproc_path = format!("{clsid_root}\\InprocServer32");
        assert!(is_subkey_present(&hkcu(), &inproc_path));

        let read_back = read_inproc_default(&hkcu(), &clsid_root).expect("read back");
        assert_eq!(read_back, dll_path);

        let inproc = hkcu().open_subkey(&inproc_path).expect("open inproc");
        let threading: String = inproc
            .get_value("ThreadingModel")
            .expect("read ThreadingModel");
        assert_eq!(threading, "Apartment");

        let clsid_key = hkcu().open_subkey(&clsid_root).expect("open clsid");
        assert!(
            clsid_key.get_value::<String, _>("AppID").is_err(),
            "thumbnail should not have AppID"
        );

        unregister_clsid_at(&hkcu(), &clsid_root).expect("unregister");
        assert!(!is_subkey_present(&hkcu(), &clsid_root));
    }

    #[test]
    fn preview_clsid_register_roundtrip() {
        let sandbox = unique_sandbox("preview_clsid");
        let _guard = SandboxGuard(sandbox.clone());
        let clsid_root = format!("{sandbox}\\{{TEST-PREVIEW}}");

        assert!(!is_subkey_present(&hkcu(), &clsid_root));

        let dll_path = std::path::PathBuf::from(r"C:\fake\arcthumb.dll");
        register_clsid_at(&hkcu(), &clsid_root, &dll_path, &PREVIEW).expect("register");

        let read_back = read_inproc_default(&hkcu(), &clsid_root).expect("read back");
        assert_eq!(read_back, dll_path);

        let inproc = hkcu()
            .open_subkey(format!("{clsid_root}\\InprocServer32"))
            .expect("open inproc");
        let threading: String = inproc
            .get_value("ThreadingModel")
            .expect("read ThreadingModel");
        assert_eq!(threading, "Apartment");

        let clsid_key = hkcu().open_subkey(&clsid_root).expect("open clsid");
        let app_id: String = clsid_key.get_value("AppID").expect("read AppID");
        assert_eq!(app_id, PREVHOST_APPID);

        unregister_clsid_at(&hkcu(), &clsid_root).expect("unregister");
        assert!(!is_subkey_present(&hkcu(), &clsid_root));
    }

    #[test]
    fn known_extensions_roundtrip_and_survive_reregistration() {
        let sandbox = unique_sandbox("known_exts");
        let _guard = SandboxGuard(sandbox.clone());
        let clsid_root = format!("{sandbox}\\{{TEST-CLSID}}");

        // Missing key and missing value both read as "not recorded".
        assert!(read_known_extensions_at(&hkcu(), &clsid_root).is_none());
        let dll_path = std::path::PathBuf::from(r"C:\fake\arcthumb.dll");
        register_clsid_at(&hkcu(), &clsid_root, &dll_path, &THUMBNAIL).expect("register");
        assert!(read_known_extensions_at(&hkcu(), &clsid_root).is_none());

        write_known_extensions_at(&hkcu(), &clsid_root).expect("write");
        let read_back = read_known_extensions_at(&hkcu(), &clsid_root).expect("read back");
        assert_eq!(read_back, EXTENSIONS);

        // An upgrade rewrites the CLSID key in place; the list stays.
        register_clsid_at(&hkcu(), &clsid_root, &dll_path, &THUMBNAIL).expect("re-register");
        assert!(read_known_extensions_at(&hkcu(), &clsid_root).is_some());

        // Uninstall takes it away with the key.
        unregister_clsid_at(&hkcu(), &clsid_root).expect("unregister");
        assert!(read_known_extensions_at(&hkcu(), &clsid_root).is_none());
    }

    #[test]
    fn pre_tracking_extensions_are_all_still_supported() {
        for ext in PRE_TRACKING_EXTENSIONS {
            assert!(EXTENSIONS.contains(ext), "{ext} dropped from EXTENSIONS");
        }
    }

    const OTHER_CLSID: &str = "{11111111-2222-3333-4444-555555555555}";

    fn slot_default(path: &str) -> Option<String> {
        hkcu()
            .open_subkey(path)
            .ok()
            .and_then(|k| bound_handler(&k))
    }

    #[test]
    fn registering_over_another_handler_restores_it_on_unregister() {
        let sandbox = unique_sandbox("takeover");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".epub", THUMBNAIL.iid_shellex);

        // Another program got there first.
        let (key, _) = hkcu().create_subkey(&path).unwrap();
        key.set_value("", &OTHER_CLSID).unwrap();
        assert!(!is_extension_bound_at(&hkcu(), &path, CLSID_STR));

        register_extension_at(&hkcu(), &path, CLSID_STR).expect("register");
        assert!(is_extension_bound_at(&hkcu(), &path, CLSID_STR));
        // Registering twice must not overwrite the remembered handler
        // with our own CLSID.
        register_extension_at(&hkcu(), &path, CLSID_STR).expect("register again");

        unregister_extension_at(&hkcu(), &path, CLSID_STR).expect("unregister");
        assert_eq!(slot_default(&path).as_deref(), Some(OTHER_CLSID));
        let key = hkcu().open_subkey(&path).unwrap();
        assert!(
            key.get_value::<String, _>(PREVIOUS_HANDLER_VALUE).is_err(),
            "the backup value is removed once restored"
        );
    }

    #[test]
    fn unregister_leaves_a_slot_another_handler_took_over() {
        let sandbox = unique_sandbox("taken");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".cbz", THUMBNAIL.iid_shellex);

        register_extension_at(&hkcu(), &path, CLSID_STR).expect("register");
        // Another program installs afterwards and takes the slot.
        let key = hkcu()
            .open_subkey_with_flags(&path, KEY_READ | KEY_WRITE)
            .unwrap();
        key.set_value("", &OTHER_CLSID).unwrap();
        drop(key);

        assert!(!is_extension_bound_at(&hkcu(), &path, CLSID_STR));
        unregister_extension_at(&hkcu(), &path, CLSID_STR).expect("unregister");
        assert_eq!(slot_default(&path).as_deref(), Some(OTHER_CLSID));
    }

    #[test]
    fn bound_check_ignores_clsid_case() {
        let sandbox = unique_sandbox("case");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".zip", THUMBNAIL.iid_shellex);
        let (key, _) = hkcu().create_subkey(&path).unwrap();
        key.set_value("", &CLSID_STR.to_ascii_lowercase()).unwrap();
        assert!(is_extension_bound_at(&hkcu(), &path, CLSID_STR));
    }

    #[test]
    fn unregister_missing_extension_is_noop() {
        let sandbox = unique_sandbox("missing_ext");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".doesnotexist", THUMBNAIL.iid_shellex);
        unregister_extension_at(&hkcu(), &path, CLSID_STR).expect("noop unregister");
    }

    #[test]
    fn unregister_missing_clsid_is_noop() {
        let sandbox = unique_sandbox("missing_clsid");
        let _guard = SandboxGuard(sandbox.clone());
        let clsid_root = format!("{sandbox}\\{{NOPE}}");
        unregister_clsid_at(&hkcu(), &clsid_root).expect("noop unregister");
    }

    #[test]
    fn read_inproc_default_returns_none_when_missing() {
        let sandbox = unique_sandbox("read_missing");
        let _guard = SandboxGuard(sandbox.clone());
        let clsid_root = format!("{sandbox}\\{{ABSENT}}");
        assert!(read_inproc_default(&hkcu(), &clsid_root).is_none());
    }

    #[test]
    fn full_install_uninstall_for_all_extensions() {
        let sandbox = unique_sandbox("full");
        let _guard = SandboxGuard(sandbox.clone());

        for handler in [&THUMBNAIL, &PREVIEW] {
            let clsid_root = clsid_root_path_under(&sandbox, handler.clsid_str);
            register_clsid_at(&hkcu(), &clsid_root, Path::new(r"C:\fake.dll"), handler)
                .expect("clsid register");

            for ext in EXTENSIONS {
                let path = ext_shellex_path_under(&sandbox, ext, handler.iid_shellex);
                register_extension_at(&hkcu(), &path, handler.clsid_str).expect(ext);
                assert!(
                    is_subkey_present(&hkcu(), &path),
                    "{ext} not present after register"
                );
            }

            for ext in EXTENSIONS {
                let path = ext_shellex_path_under(&sandbox, ext, handler.iid_shellex);
                unregister_extension_at(&hkcu(), &path, handler.clsid_str).expect(ext);
                assert!(
                    !is_subkey_present(&hkcu(), &path),
                    "{ext} still present after unregister"
                );
            }
            unregister_clsid_at(&hkcu(), &clsid_root).expect("clsid unregister");
        }
    }

    #[test]
    fn preview_extension_register_roundtrip() {
        let sandbox = unique_sandbox("preview_ext");
        let _guard = SandboxGuard(sandbox.clone());
        let path = ext_shellex_path_under(&sandbox, ".epub", PREVIEW.iid_shellex);

        assert!(!is_subkey_present(&hkcu(), &path));

        register_extension_at(&hkcu(), &path, PREVIEW_CLSID_STR).expect("register");
        assert!(is_subkey_present(&hkcu(), &path));

        let key = hkcu().open_subkey(&path).expect("open after register");
        let val: String = key.get_value("").expect("read default");
        assert_eq!(val, PREVIEW_CLSID_STR);

        unregister_extension_at(&hkcu(), &path, PREVIEW_CLSID_STR).expect("unregister");
        assert!(!is_subkey_present(&hkcu(), &path));
    }
}
