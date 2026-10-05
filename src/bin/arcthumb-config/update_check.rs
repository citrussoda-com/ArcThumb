//! Background update check driver.
//!
//! Spawns a worker thread on GUI startup that hits the GitHub
//! releases API, and if a newer version exists and the user has
//! not opted out of the reminder, marshals the result back onto
//! the Slint event loop so `dialogs::show_update_dialog` can open
//! a real window.
//!
//! The actual HTTP fetch, throttle logic, and "has the user
//! skipped this version" check all live in `update`. This module
//! is the thin glue that turns a `Some(UpdateInfo)` into a visible
//! prompt on the UI thread.
//!
//! ## Why a separate module
//!
//! Phase 2 of the refactor pulled this function out of `ui.rs`
//! alongside the Slint sub-dialogs so each concern has its own
//! short file.

use slint::{ComponentHandle, Weak};

use crate::dialogs;
use crate::message_box;
use crate::ui::{MainWindow, Texts};
use crate::update;

/// Kick off the background update check. Returns immediately.
///
/// The worker thread is fire-and-forget; its only observable side
/// effect is posting a closure onto the Slint event loop that may
/// open an `UpdateDialog`. If the user has disabled update checks,
/// the throttle window hasn't elapsed, no newer release exists, or
/// the user has already hit "Skip this version", the thread exits
/// silently without touching the UI.
pub fn start_update_check() {
    std::thread::spawn(move || {
        if !update::should_check_now() {
            return;
        }
        let Some(info) = update::check_for_update() else {
            return;
        };
        if update::is_version_skipped(&info.latest_version) {
            return;
        }
        // Marshal the prompt back to the UI thread so the Slint
        // window is owned by the same thread as the rest of the GUI.
        let _ = slint::invoke_from_event_loop(move || {
            dialogs::show_update_dialog(info);
        });
    });
}

thread_local! {
    /// True while a user-initiated check is in flight. Only ever touched
    /// on the UI thread (set before spawning, cleared when the result
    /// posts back via `invoke_from_event_loop`), so a plain `Cell` is
    /// enough — no atomics needed.
    static MANUAL_CHECK_RUNNING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run a user-initiated update check (Help → Check for updates).
///
/// Unlike [`start_update_check`], this ignores the 24-hour throttle and
/// any skipped version — the user explicitly asked, so we always hit
/// the network — and it reports all three outcomes (newer / up to date
/// / failed) instead of staying silent. The fetch runs on a worker
/// thread so the GUI stays responsive; a guard flag drops repeat clicks
/// while a check is already running.
///
/// `window` is only used to reach the translated MessageBox strings
/// (`Texts`) once the result is back on the UI thread; if the window
/// is gone by then there is nobody to tell, so the result is dropped.
pub fn run_manual_check(window: Weak<MainWindow>) {
    if MANUAL_CHECK_RUNNING.with(|c| c.get()) {
        return;
    }
    MANUAL_CHECK_RUNNING.with(|c| c.set(true));

    std::thread::spawn(move || {
        let outcome = update::check_for_update_now();
        let _ = slint::invoke_from_event_loop(move || {
            MANUAL_CHECK_RUNNING.with(|c| c.set(false));
            let Some(window) = window.upgrade() else {
                return;
            };
            let texts = window.global::<Texts>();
            match outcome {
                update::ManualCheck::Available(info) => {
                    dialogs::show_update_dialog(info);
                }
                update::ManualCheck::UpToDate => {
                    let msg = texts.invoke_update_up_to_date(update::current_version().into());
                    message_box::info(&texts.get_update_check_title(), &msg);
                }
                update::ManualCheck::Failed => {
                    message_box::error(
                        &texts.get_update_check_title(),
                        &texts.get_update_check_failed(),
                    );
                }
            }
        });
    });
}
