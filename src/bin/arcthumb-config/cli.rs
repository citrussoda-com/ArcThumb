//! CLI `--install` / `--uninstall` drivers for the installer.
//!
//! Same split as `apply.rs`: the exit-code and ordering logic is a
//! driver over the [`CliOps`] trait so it can be unit-tested without
//! touching the real registry. [`RealCliOps`] is the thin production
//! implementation that forwards to `arcthumb::registry`.
//!
//! Exit codes (the Inno Setup installer checks `--install` for
//! non-zero — keep stable):
//! - `0` success
//! - `2` arcthumb.dll not found (`--install` only)
//! - `3` CLSID registration failed
//! - `4` extension binding failed
//! - `6` some registry entries could not be removed (`--uninstall` only)
//!
//! (Exit code `5` — GUI init failure — lives in `main.rs`; it is not
//! part of the CLI drivers.)

use std::io;
use std::path::{Path, PathBuf};

use arcthumb::registry::{self, Scope};

use crate::dll_path;

pub const EXIT_OK: i32 = 0;
pub const EXIT_DLL_NOT_FOUND: i32 = 2;
pub const EXIT_CLSID_FAILED: i32 = 3;
pub const EXIT_EXTENSION_FAILED: i32 = 4;
pub const EXIT_UNINSTALL_INCOMPLETE: i32 = 6;

/// The side effects the CLI drivers need. Production uses
/// [`RealCliOps`]; tests inject a recording mock.
pub trait CliOps {
    fn resolve_dll_path(&self) -> Result<PathBuf, String>;
    fn current_scope(&self) -> Scope;
    fn is_clsid_registered(&self, scope: Scope) -> bool;
    fn is_extension_registered(&self, scope: Scope, ext: &'static str) -> bool;
    fn is_preview_enabled(&self, scope: Scope) -> bool;
    fn known_extensions(&self, scope: Scope) -> Option<Vec<String>>;
    fn record_known_extensions(&self, scope: Scope) -> io::Result<()>;
    fn register_clsid(&self, scope: Scope, dll_path: &Path) -> io::Result<()>;
    fn register_preview_clsid(&self, scope: Scope, dll_path: &Path) -> io::Result<()>;
    fn register_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()>;
    fn register_preview_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()>;
    fn unregister_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()>;
    fn unregister_preview_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()>;
    fn unregister_clsid(&self, scope: Scope) -> io::Result<()>;
    fn unregister_preview_clsid(&self, scope: Scope) -> io::Result<()>;
    fn notify_assoc_changed(&self);
}

pub struct RealCliOps;

impl CliOps for RealCliOps {
    fn resolve_dll_path(&self) -> Result<PathBuf, String> {
        dll_path::resolve_dll_path()
    }
    fn current_scope(&self) -> Scope {
        arcthumb::elevation::current_scope()
    }
    fn is_clsid_registered(&self, scope: Scope) -> bool {
        registry::is_clsid_registered(scope)
    }
    fn is_extension_registered(&self, scope: Scope, ext: &'static str) -> bool {
        registry::is_extension_registered(scope, ext)
    }
    fn is_preview_enabled(&self, scope: Scope) -> bool {
        registry::is_preview_enabled(scope)
    }
    fn known_extensions(&self, scope: Scope) -> Option<Vec<String>> {
        registry::read_known_extensions(scope)
    }
    fn record_known_extensions(&self, scope: Scope) -> io::Result<()> {
        registry::write_known_extensions(scope)
    }
    fn register_clsid(&self, scope: Scope, dll_path: &Path) -> io::Result<()> {
        registry::register_clsid(scope, dll_path)
    }
    fn register_preview_clsid(&self, scope: Scope, dll_path: &Path) -> io::Result<()> {
        registry::register_preview_clsid(scope, dll_path)
    }
    fn register_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()> {
        registry::register_extension(scope, ext)
    }
    fn register_preview_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()> {
        registry::register_preview_extension(scope, ext)
    }
    fn unregister_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()> {
        registry::unregister_extension(scope, ext)
    }
    fn unregister_preview_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()> {
        registry::unregister_preview_extension(scope, ext)
    }
    fn unregister_clsid(&self, scope: Scope) -> io::Result<()> {
        registry::unregister_clsid(scope)
    }
    fn unregister_preview_clsid(&self, scope: Scope) -> io::Result<()> {
        registry::unregister_preview_clsid(scope)
    }
    fn notify_assoc_changed(&self) {
        registry::notify_assoc_changed();
    }
}

/// `--install`: write the full shell-extension registration.
///
/// Hive is picked by elevation: HKLM when the process is elevated
/// (admin Inno install mode), HKCU otherwise. This is what makes the
/// shell extension load under High-Integrity Explorer in Windows
/// Sandbox and enterprise lockdowns where HKCU CLSIDs are ignored.
pub fn run_install(ops: &dyn CliOps) -> i32 {
    let dll_path = match ops.resolve_dll_path() {
        Ok(p) => p,
        Err(_) => return EXIT_DLL_NOT_FOUND,
    };
    let scope = ops.current_scope();

    // The installer runs `--install` on upgrades too. Read what the
    // user had before writing anything, so an extension or the preview
    // pane they switched off in the GUI stays off.
    let upgrade = ops.is_clsid_registered(scope);
    let known = ops.known_extensions(scope);
    let was_known = |ext: &str| match &known {
        Some(list) => list.iter().any(|k| k == ext),
        None => registry::PRE_TRACKING_EXTENSIONS.contains(&ext),
    };
    // On an upgrade a missing binding means "switched off" only for an
    // extension the previous build already offered. One that is new in
    // this build was never offered, so it starts enabled.
    let thumbnail_exts: Vec<&'static str> = registry::EXTENSIONS
        .iter()
        .copied()
        .filter(|&ext| !upgrade || ops.is_extension_registered(scope, ext) || !was_known(ext))
        .collect();
    // The preview pane is one switch for every extension.
    let preview = !upgrade || ops.is_preview_enabled(scope);

    // Both COM classes (thumbnail provider + preview handler) are
    // registered together on a fresh install so the user gets both
    // features by default. The GUI's "Enable preview pane" checkbox
    // can later be unchecked to remove just the preview handler.
    if ops.register_clsid(scope, &dll_path).is_err() {
        return EXIT_CLSID_FAILED;
    }
    if preview && ops.register_preview_clsid(scope, &dll_path).is_err() {
        return EXIT_CLSID_FAILED;
    }
    for &ext in registry::EXTENSIONS {
        if thumbnail_exts.contains(&ext) && ops.register_extension(scope, ext).is_err() {
            return EXIT_EXTENSION_FAILED;
        }
        if preview && ops.register_preview_extension(scope, ext).is_err() {
            return EXIT_EXTENSION_FAILED;
        }
    }
    if ops.record_known_extensions(scope).is_err() {
        return EXIT_CLSID_FAILED;
    }
    // Tell Explorer to drop its icon/thumbnail cache so the freshly
    // registered handlers take effect without a reboot — this is what
    // Microsoft's shell extension docs require us to do.
    ops.notify_assoc_changed();
    EXIT_OK
}

/// `--uninstall`: clean BOTH hives best-effort. The user may have
/// switched modes between versions, or an old per-user install may
/// still be lying around when a new per-machine install is being
/// uninstalled.
///
/// A failure never stops the sweep, but it is reported: an entry that
/// is already absent counts as removed, so any error here is a key
/// that exists and could not be deleted (typically access denied on
/// HKLM without elevation).
pub fn run_uninstall(ops: &dyn CliOps) -> i32 {
    let mut complete = true;
    for scope in Scope::ALL.iter().copied() {
        for &ext in registry::EXTENSIONS {
            complete &= ops.unregister_extension(scope, ext).is_ok();
            complete &= ops.unregister_preview_extension(scope, ext).is_ok();
        }
        complete &= ops.unregister_clsid(scope).is_ok();
        complete &= ops.unregister_preview_clsid(scope).is_ok();
    }
    ops.notify_assoc_changed();
    if complete {
        EXIT_OK
    } else {
        EXIT_UNINSTALL_INCOMPLETE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Recording mock. Each side effect is logged as
    /// `"<op>:<scope>[:<arg>]"`; names listed in `fail_on` return an
    /// error after being recorded.
    struct MockCliOps {
        dll_path: Option<PathBuf>,
        scope: Scope,
        calls: RefCell<Vec<String>>,
        fail_on: RefCell<Vec<String>>,
        notify_called: RefCell<bool>,
        /// Registry state an earlier install left behind. `None` means
        /// nothing is installed (the default).
        previous: Option<PreviousInstall>,
    }

    struct PreviousInstall {
        bound: Vec<&'static str>,
        preview: bool,
        known: Option<Vec<String>>,
    }

    impl MockCliOps {
        fn new() -> Self {
            Self {
                dll_path: Some(PathBuf::from(r"C:\fake\arcthumb.dll")),
                scope: Scope::PerUser,
                calls: RefCell::new(Vec::new()),
                fail_on: RefCell::new(Vec::new()),
                notify_called: RefCell::new(false),
                previous: None,
            }
        }

        /// Pretend an install by a build that offered `known` is in
        /// place, with only `bound` still ticked in the GUI.
        fn upgrading_from(
            mut self,
            bound: &[&'static str],
            preview: bool,
            known: Option<&[&str]>,
        ) -> Self {
            self.previous = Some(PreviousInstall {
                bound: bound.to_vec(),
                preview,
                known: known.map(|k| k.iter().map(|e| e.to_string()).collect()),
            });
            self
        }

        fn without_dll(mut self) -> Self {
            self.dll_path = None;
            self
        }

        fn with_scope(mut self, scope: Scope) -> Self {
            self.scope = scope;
            self
        }

        fn fail_on(self, call: &str) -> Self {
            self.fail_on.borrow_mut().push(call.to_string());
            self
        }

        fn record(&self, name: String) -> io::Result<()> {
            let fail = self.fail_on.borrow().contains(&name);
            self.calls.borrow_mut().push(name);
            if fail {
                Err(io::Error::other("mock failure"))
            } else {
                Ok(())
            }
        }
    }

    fn tag(scope: Scope) -> &'static str {
        match scope {
            Scope::PerUser => "user",
            Scope::PerMachine => "machine",
        }
    }

    impl CliOps for MockCliOps {
        fn resolve_dll_path(&self) -> Result<PathBuf, String> {
            self.dll_path.clone().ok_or_else(|| "not found".to_string())
        }
        fn current_scope(&self) -> Scope {
            self.scope
        }
        fn is_clsid_registered(&self, _scope: Scope) -> bool {
            self.previous.is_some()
        }
        fn is_extension_registered(&self, _scope: Scope, ext: &'static str) -> bool {
            self.previous
                .as_ref()
                .is_some_and(|p| p.bound.contains(&ext))
        }
        fn is_preview_enabled(&self, _scope: Scope) -> bool {
            self.previous.as_ref().is_some_and(|p| p.preview)
        }
        fn known_extensions(&self, _scope: Scope) -> Option<Vec<String>> {
            self.previous.as_ref().and_then(|p| p.known.clone())
        }
        fn record_known_extensions(&self, scope: Scope) -> io::Result<()> {
            self.record(format!("record_known_extensions:{}", tag(scope)))
        }
        fn register_clsid(&self, scope: Scope, _dll_path: &Path) -> io::Result<()> {
            self.record(format!("register_clsid:{}", tag(scope)))
        }
        fn register_preview_clsid(&self, scope: Scope, _dll_path: &Path) -> io::Result<()> {
            self.record(format!("register_preview_clsid:{}", tag(scope)))
        }
        fn register_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()> {
            self.record(format!("register_extension:{}:{ext}", tag(scope)))
        }
        fn register_preview_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()> {
            self.record(format!("register_preview_extension:{}:{ext}", tag(scope)))
        }
        fn unregister_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()> {
            self.record(format!("unregister_extension:{}:{ext}", tag(scope)))
        }
        fn unregister_preview_extension(&self, scope: Scope, ext: &'static str) -> io::Result<()> {
            self.record(format!("unregister_preview_extension:{}:{ext}", tag(scope)))
        }
        fn unregister_clsid(&self, scope: Scope) -> io::Result<()> {
            self.record(format!("unregister_clsid:{}", tag(scope)))
        }
        fn unregister_preview_clsid(&self, scope: Scope) -> io::Result<()> {
            self.record(format!("unregister_preview_clsid:{}", tag(scope)))
        }
        fn notify_assoc_changed(&self) {
            *self.notify_called.borrow_mut() = true;
        }
    }

    // ----- run_install ----------------------------------------------------

    #[test]
    fn install_registers_clsids_then_every_extension_and_notifies() {
        let ops = MockCliOps::new();
        assert_eq!(run_install(&ops), EXIT_OK);
        assert!(*ops.notify_called.borrow());

        let calls = ops.calls.borrow();
        // Both CLSIDs first, in thumbnail → preview order.
        assert_eq!(calls[0], "register_clsid:user");
        assert_eq!(calls[1], "register_preview_clsid:user");
        // Then thumbnail + preview bindings for every extension, and
        // the known-extension list last.
        assert_eq!(calls.len(), 2 + registry::EXTENSIONS.len() * 2 + 1);
        assert_eq!(calls.last().unwrap(), "record_known_extensions:user");
        for (i, &ext) in registry::EXTENSIONS.iter().enumerate() {
            assert_eq!(calls[2 + i * 2], format!("register_extension:user:{ext}"));
            assert_eq!(
                calls[3 + i * 2],
                format!("register_preview_extension:user:{ext}")
            );
        }
    }

    #[test]
    fn install_targets_the_scope_reported_by_elevation() {
        let ops = MockCliOps::new().with_scope(Scope::PerMachine);
        assert_eq!(run_install(&ops), EXIT_OK);
        assert!(
            ops.calls.borrow().iter().all(|c| c.contains(":machine")),
            "every registration must hit the elevated hive"
        );
    }

    #[test]
    fn install_returns_2_when_dll_is_missing() {
        let ops = MockCliOps::new().without_dll();
        assert_eq!(run_install(&ops), EXIT_DLL_NOT_FOUND);
        assert!(ops.calls.borrow().is_empty(), "no registry writes");
        assert!(!*ops.notify_called.borrow());
    }

    #[test]
    fn install_returns_3_when_thumbnail_clsid_fails() {
        let ops = MockCliOps::new().fail_on("register_clsid:user");
        assert_eq!(run_install(&ops), EXIT_CLSID_FAILED);
        // Aborts before any extension binding, and Explorer is not
        // told to reload a registration that was never written.
        assert_eq!(ops.calls.borrow().len(), 1);
        assert!(!*ops.notify_called.borrow());
    }

    #[test]
    fn install_returns_3_when_preview_clsid_fails() {
        let ops = MockCliOps::new().fail_on("register_preview_clsid:user");
        assert_eq!(run_install(&ops), EXIT_CLSID_FAILED);
        assert_eq!(ops.calls.borrow().len(), 2);
        assert!(!*ops.notify_called.borrow());
    }

    #[test]
    fn install_returns_4_when_an_extension_binding_fails() {
        let ext = registry::EXTENSIONS[2];
        let ops = MockCliOps::new().fail_on(&format!("register_extension:user:{ext}"));
        assert_eq!(run_install(&ops), EXIT_EXTENSION_FAILED);
        // Stops at the failing extension: both CLSIDs, two full
        // extensions before it, then the failing call itself.
        assert_eq!(ops.calls.borrow().len(), 2 + 2 * 2 + 1);
        assert!(!*ops.notify_called.borrow());
    }

    #[test]
    fn install_returns_4_when_a_preview_extension_binding_fails() {
        let ext = registry::EXTENSIONS[0];
        let ops = MockCliOps::new().fail_on(&format!("register_preview_extension:user:{ext}"));
        assert_eq!(run_install(&ops), EXIT_EXTENSION_FAILED);
        assert!(!*ops.notify_called.borrow());
    }

    #[test]
    fn install_returns_3_when_recording_known_extensions_fails() {
        let ops = MockCliOps::new().fail_on("record_known_extensions:user");
        assert_eq!(run_install(&ops), EXIT_CLSID_FAILED);
        assert!(!*ops.notify_called.borrow());
    }

    // ----- run_install over an existing install ---------------------------

    #[test]
    fn upgrade_keeps_extensions_the_user_switched_off() {
        let all = registry::EXTENSIONS;
        let bound: Vec<&'static str> = all.iter().copied().filter(|&e| e != ".zip").collect();
        let ops = MockCliOps::new().upgrading_from(&bound, true, Some(all));
        assert_eq!(run_install(&ops), EXIT_OK);

        let calls = ops.calls.borrow();
        assert!(!calls.contains(&"register_extension:user:.zip".to_string()));
        for &ext in &bound {
            assert!(calls.contains(&format!("register_extension:user:{ext}")));
        }
        // The DLL path may have moved, so the CLSID is always rewritten.
        assert!(calls.contains(&"register_clsid:user".to_string()));
        // Preview is one switch for every extension, `.zip` included.
        assert!(calls.contains(&"register_preview_extension:user:.zip".to_string()));
        assert!(*ops.notify_called.borrow());
    }

    #[test]
    fn upgrade_keeps_the_preview_pane_switched_off() {
        let all = registry::EXTENSIONS;
        let ops = MockCliOps::new().upgrading_from(all, false, Some(all));
        assert_eq!(run_install(&ops), EXIT_OK);

        let calls = ops.calls.borrow();
        assert!(calls.iter().all(|c| !c.starts_with("register_preview_")));
        assert_eq!(
            calls.len(),
            1 + all.len() + 1,
            "thumbnail CLSID, every thumbnail binding, known list"
        );
    }

    #[test]
    fn upgrade_enables_extensions_the_previous_build_did_not_offer() {
        // The previous build knew everything but `.epub`, so its missing
        // binding is "new format", not "switched off".
        let all = registry::EXTENSIONS;
        let known: Vec<&str> = all.iter().copied().filter(|&e| e != ".epub").collect();
        let bound: Vec<&'static str> = all
            .iter()
            .copied()
            .filter(|&e| e != ".epub" && e != ".rar")
            .collect();
        let ops = MockCliOps::new().upgrading_from(&bound, true, Some(&known));
        assert_eq!(run_install(&ops), EXIT_OK);

        let calls = ops.calls.borrow();
        assert!(calls.contains(&"register_extension:user:.epub".to_string()));
        assert!(!calls.contains(&"register_extension:user:.rar".to_string()));
    }

    #[test]
    fn upgrade_from_a_build_without_a_known_list_uses_the_frozen_one() {
        let bound: Vec<&'static str> = registry::PRE_TRACKING_EXTENSIONS
            .iter()
            .copied()
            .filter(|&e| e != ".cbz")
            .collect();
        let ops = MockCliOps::new().upgrading_from(&bound, true, None);
        assert_eq!(run_install(&ops), EXIT_OK);

        let calls = ops.calls.borrow();
        assert!(!calls.contains(&"register_extension:user:.cbz".to_string()));
        assert!(calls.contains(&"record_known_extensions:user".to_string()));
    }

    // ----- run_uninstall --------------------------------------------------

    #[test]
    fn uninstall_cleans_both_hives_and_notifies() {
        let ops = MockCliOps::new();
        assert_eq!(run_uninstall(&ops), EXIT_OK);
        assert!(*ops.notify_called.borrow());

        let calls = ops.calls.borrow();
        // Per scope: thumbnail + preview unbind per extension, then
        // both CLSID removals. Machine first (Scope::ALL order).
        let per_scope = registry::EXTENSIONS.len() * 2 + 2;
        assert_eq!(calls.len(), per_scope * 2);
        assert!(calls[..per_scope].iter().all(|c| c.contains(":machine")));
        assert!(calls[per_scope..].iter().all(|c| c.contains(":user")));
        for scope in ["machine", "user"] {
            assert!(calls.contains(&format!("unregister_clsid:{scope}")));
            assert!(calls.contains(&format!("unregister_preview_clsid:{scope}")));
            for &ext in registry::EXTENSIONS {
                assert!(calls.contains(&format!("unregister_extension:{scope}:{ext}")));
                assert!(calls.contains(&format!("unregister_preview_extension:{scope}:{ext}")));
            }
        }
    }

    #[test]
    fn uninstall_reports_a_single_failure_without_stopping() {
        let ops = MockCliOps::new().fail_on("unregister_clsid:machine");
        assert_eq!(run_uninstall(&ops), EXIT_UNINSTALL_INCOMPLETE);
        let per_scope = registry::EXTENSIONS.len() * 2 + 2;
        assert_eq!(ops.calls.borrow().len(), per_scope * 2, "nothing skipped");
    }

    #[test]
    fn uninstall_is_best_effort_and_reports_failures() {
        // Fail every single unregister call — typically AccessDenied
        // on HKLM from a non-elevated uninstaller. The driver must
        // keep going, still notify Explorer, and say it was incomplete.
        let ops = MockCliOps::new();
        for scope in ["machine", "user"] {
            ops.fail_on
                .borrow_mut()
                .push(format!("unregister_clsid:{scope}"));
            ops.fail_on
                .borrow_mut()
                .push(format!("unregister_preview_clsid:{scope}"));
            for &ext in registry::EXTENSIONS {
                ops.fail_on
                    .borrow_mut()
                    .push(format!("unregister_extension:{scope}:{ext}"));
                ops.fail_on
                    .borrow_mut()
                    .push(format!("unregister_preview_extension:{scope}:{ext}"));
            }
        }
        assert_eq!(run_uninstall(&ops), EXIT_UNINSTALL_INCOMPLETE);
        let per_scope = registry::EXTENSIONS.len() * 2 + 2;
        assert_eq!(ops.calls.borrow().len(), per_scope * 2, "nothing skipped");
        assert!(*ops.notify_called.borrow());
    }
}
