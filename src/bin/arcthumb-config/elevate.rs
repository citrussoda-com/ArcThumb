//! Run this exe again with administrator rights.
//!
//! The GUI runs unelevated. That is enough for a per-user install,
//! but a per-machine install keeps its registration under HKLM, which
//! an unelevated process cannot write. Instead of elevating the whole
//! GUI, Apply hands just the registration changes to a short-lived
//! elevated copy of this exe (`--apply-shell`, see `cli.rs`) and waits
//! for its exit code. The GUI itself never holds an elevated token, so
//! nothing it starts later (Explorer, after a cache wipe) inherits one.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;

use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{PCWSTR, w};

/// How long to wait for the elevated copy. It only writes a handful of
/// registry keys, so anything near this is a hang.
const HELPER_TIMEOUT_MS: u32 = 60_000;

pub enum Elevated {
    /// The elevated copy ran and exited with this code.
    Exited(u32),
    /// The user dismissed the UAC prompt.
    Declined,
}

/// Start this exe elevated with `args`, wait for it, and report how it
/// ended. Shows the UAC prompt.
///
/// `args` are joined with spaces as they are, so they must not contain
/// whitespace or quotes. The callers pass fixed tokens only.
pub fn run_self_elevated(args: &[String]) -> io::Result<Elevated> {
    debug_assert!(
        args.iter()
            .all(|a| !a.is_empty() && !a.contains([' ', '\t', '"']))
    );
    let exe = wide(std::env::current_exe()?.as_os_str());
    let params = wide(OsStr::new(&args.join(" ")));

    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(exe.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };

    unsafe {
        if let Err(e) = ShellExecuteExW(&mut info) {
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Ok(Elevated::Declined);
            }
            return Err(io::Error::other(e));
        }
        if info.hProcess.is_invalid() {
            return Err(io::Error::other("elevated process handle unavailable"));
        }

        let waited = WaitForSingleObject(info.hProcess, HELPER_TIMEOUT_MS);
        let mut code = 0u32;
        let result = if waited != WAIT_OBJECT_0 {
            Err(io::Error::other("elevated process did not finish in time"))
        } else {
            GetExitCodeProcess(info.hProcess, &mut code)
                .map(|()| Elevated::Exited(code))
                .map_err(io::Error::other)
        };
        let _ = CloseHandle(info.hProcess);
        result
    }
}

fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}
