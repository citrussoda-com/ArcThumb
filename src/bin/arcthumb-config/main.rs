//! ArcThumb Configuration — dual-mode binary.
//!
//! ## GUI mode (default)
//!
//! Running with no arguments launches a Slint-based settings window
//! where the user can enable/disable individual file extensions and
//! tweak the thumbnail selection behaviour (sort order, cover-name
//! preference).
//!
//! ## CLI mode
//!
//! ```text
//! arcthumb-config.exe --install [--scope user|machine]
//!     Write the full shell-extension registration. Without `--scope`
//!     the hive is picked by elevation: HKLM when the process is
//!     elevated (per-machine install), HKCU otherwise (per-user
//!     install). The Inno Setup installer calls this as a post-install
//!     step and always passes `--scope`.
//!
//! arcthumb-config.exe --uninstall
//!     Remove every ShellEx binding and the CLSID key from BOTH
//!     hives (best effort) so a per-user → per-machine switch or
//!     vice versa doesn't leave stale entries behind.
//!     Called by the uninstaller as a pre-uninstall step.
//!
//! arcthumb-config.exe --apply-shell <user|machine> <changes…>
//!     Internal. The GUI starts an elevated copy of itself with this
//!     to change a per-machine registration; see `elevate.rs`.
//! ```
//!
//! Exit codes:
//! - `0` success
//! - `2` DLL not found (for --install)
//! - `3` CLSID registration failed
//! - `4` extension binding failed
//! - `5` GUI init failed (very rare)
//! - `6` some registry entries could not be removed (for --uninstall)
//! - `7` a registration change failed (for --apply-shell)
//! - `8` unrecognised arguments

// Hide the console on release builds. Debug builds keep the console
// so `cargo run` output is visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod apply;
mod cache;
mod cli;
mod dialogs;
mod dll_path;
mod elevate;
mod extension_model;
mod locale;
mod message_box;
mod state;
mod ui;
mod update;
mod update_check;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("--install") => {
            attach_console();
            let code = match cli::parse_install_args(&args[2..]) {
                Ok(Some(scope)) => cli::run_install_in(&cli::RealCliOps, scope),
                Ok(None) => cli::run_install(&cli::RealCliOps),
                Err(()) => cli::EXIT_USAGE,
            };
            match code {
                cli::EXIT_OK => println!("ArcThumb installed successfully."),
                cli::EXIT_DLL_NOT_FOUND => eprintln!("Error: arcthumb.dll not found."),
                cli::EXIT_CLSID_FAILED => eprintln!("Error: CLSID registration failed."),
                cli::EXIT_EXTENSION_FAILED => eprintln!("Error: extension binding failed."),
                cli::EXIT_USAGE => {
                    eprintln!("Usage: arcthumb-config --install [--scope user|machine]")
                }
                _ => eprintln!("Error: unknown failure (exit code {code})."),
            }
            std::process::exit(code);
        }
        Some("--uninstall") => {
            attach_console();
            let code = cli::run_uninstall(&cli::RealCliOps);
            match code {
                cli::EXIT_OK => println!("ArcThumb uninstalled."),
                _ => eprintln!(
                    "Error: some registry entries could not be removed. \
                     Run again from an elevated prompt."
                ),
            }
            std::process::exit(code);
        }
        Some(cli::APPLY_SHELL_FLAG) => {
            std::process::exit(cli::run_apply_shell(&args[2..]));
        }
        _ => {
            // Surface the failure with a native MessageBox before
            // exiting. Release builds run as `windows_subsystem =
            // "windows"`, so without this the user sees nothing —
            // not even a console line — and reports the binary as
            // broken. Reported in microsoft/winget-pkgs#364519.
            if let Err(e) = ui::run_gui() {
                let strings = locale::current();
                message_box::error(
                    strings.error_title,
                    &format!("{}\n\n{e}", strings.error_gui_init),
                );
                std::process::exit(5);
            }
        }
    }
}

/// Attach to the parent process's console so `println!` / `eprintln!`
/// are visible when `--install` / `--uninstall` are run from PowerShell
/// or cmd.exe. Release builds are `windows_subsystem = "windows"`, so
/// without this their status output goes nowhere. No-op on debug builds
/// (they already own a console) and when there is no parent console to
/// attach to (double-click launch).
fn attach_console() {
    #[cfg(not(debug_assertions))]
    unsafe {
        // AttachConsole(ATTACH_PARENT_PROCESS). Called via raw FFI to
        // avoid pulling Win32_System_Console into the crate's `windows`
        // feature set just for this one call.
        unsafe extern "system" {
            fn AttachConsole(dw_process_id: u32) -> i32;
        }
        let _ = AttachConsole(u32::MAX);
    }
}
