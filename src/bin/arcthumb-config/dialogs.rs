//! Slint sub-dialogs hosted by `arcthumb-config`.
//!
//! Currently three: About, Update, Donation. All three are
//! non-modal Slint windows shown on top of the main settings
//! window, kept alive until the user closes them via one of their
//! buttons.
//!
//! ## Lifetime pattern
//!
//! Slint windows are `!Send`, so we cannot stash them behind a
//! `Mutex`. Each dialog has its own `thread_local!` cell that
//! holds the strong `ComponentHandle` reference while the dialog
//! is visible. When the user clicks a button the callback:
//!
//!   1. Hides the window via the `Weak` handle.
//!   2. Clears the `thread_local` slot, dropping the last strong
//!      reference and letting Slint reclaim the component.
//!
//! The title-bar close button goes through `on_close_requested`
//! instead and has to clear the same slot, otherwise the "already
//! open" check keeps the dialog from ever opening again. There the
//! slot is cleared on the next event-loop turn, so the component is
//! not dropped while Slint is still closing its window.
//!
//! `arcthumb-config` only ever has one UI thread (the Slint event
//! loop runs there), so the thread-locals are both safe and the
//! natural home for dialog handles.
//!
//! ## Why a separate module
//!
//! Phase 2 of the refactor pulled these three dialogs out of
//! `ui.rs` to shrink it below 600 lines. The dialogs themselves
//! share nothing with the main settings logic beyond a couple of
//! `update::*` and `locale::Strings` references, so moving them
//! out is a pure file-level split with no behaviour change.

use std::cell::RefCell;
use std::thread::LocalKey;
use std::time::Duration;

use slint::{CloseRequestResponse, ComponentHandle, SharedString, Timer};

use crate::locale::Strings;
use crate::ui::{AboutDialog, DonationDialog, UpdateDialog};
use crate::update;

thread_local! {
    static ABOUT_DIALOG: RefCell<Option<AboutDialog>> = const { RefCell::new(None) };
    static UPDATE_DIALOG: RefCell<Option<UpdateDialog>> = const { RefCell::new(None) };
    static DONATION_DIALOG: RefCell<Option<DonationDialog>> = const { RefCell::new(None) };
}

/// Drop the dialog held in `slot` once the current event has been
/// handled. Used from `on_close_requested`.
fn clear_slot_soon<T: 'static>(slot: &'static LocalKey<RefCell<Option<T>>>) {
    Timer::single_shot(Duration::ZERO, move || {
        slot.with(|h| *h.borrow_mut() = None);
    });
}

// =============================================================================
// About dialog — Slint window so we can embed `AboutSlint`.
// =============================================================================

pub fn show_about(strings: &Strings) {
    // Already open? Do nothing — Slint will keep the existing window
    // focused. We avoid stacking duplicate dialogs on rapid clicks.
    let already_open = ABOUT_DIALOG.with(|h| h.borrow().is_some());
    if already_open {
        return;
    }

    let dialog = match AboutDialog::new() {
        Ok(d) => d,
        Err(_) => return,
    };
    dialog.set_dialog_title(SharedString::from(strings.about_title));
    dialog.set_version_text(SharedString::from(format!(
        "ArcThumb {}",
        update::current_version()
    )));
    dialog.set_body_text(SharedString::from(strings.about_body));
    dialog.set_btn_close(SharedString::from(strings.btn_close));

    let weak = dialog.as_weak();
    dialog.on_close_clicked(move || {
        if let Some(w) = weak.upgrade() {
            let _ = w.hide();
        }
        ABOUT_DIALOG.with(|h| *h.borrow_mut() = None);
    });
    dialog.window().on_close_requested(|| {
        clear_slot_soon(&ABOUT_DIALOG);
        CloseRequestResponse::HideWindow
    });

    if dialog.show().is_ok() {
        ABOUT_DIALOG.with(|h| *h.borrow_mut() = Some(dialog));
    }
}

// =============================================================================
// Update dialog — Slint window with a "Skip this version" checkbox.
// =============================================================================

pub fn show_update_dialog(info: update::UpdateInfo, strings: &'static Strings) {
    let already_open = UPDATE_DIALOG.with(|h| h.borrow().is_some());
    if already_open {
        return;
    }

    let dialog = match UpdateDialog::new() {
        Ok(d) => d,
        Err(_) => return,
    };

    let header = strings
        .update_available
        .replacen("{}", &info.latest_version, 1)
        .replacen("{}", update::current_version(), 1);

    dialog.set_dialog_title(SharedString::from(strings.update_title));
    dialog.set_message_text(SharedString::from(header));
    dialog.set_skip_checkbox_label(SharedString::from(strings.update_skip_checkbox));
    dialog.set_btn_open(SharedString::from(strings.update_btn_open));
    dialog.set_btn_later(SharedString::from(strings.update_btn_later));

    // Open download page. Honors the skip checkbox so the user can
    // both grab the new version and tell us not to remind them again.
    {
        let weak = dialog.as_weak();
        let release_url = info.release_url.clone();
        let latest_version = info.latest_version.clone();
        dialog.on_open_clicked(move || {
            if let Some(d) = weak.upgrade() {
                if d.get_skip_checked() {
                    update::skip_version(&latest_version);
                }
                let _ = d.hide();
            }
            update::open_url(&release_url);
            UPDATE_DIALOG.with(|h| *h.borrow_mut() = None);
        });
    }

    // Remind me later. Same checkbox handling, no URL.
    {
        let weak = dialog.as_weak();
        let latest_version = info.latest_version.clone();
        dialog.on_later_clicked(move || {
            if let Some(d) = weak.upgrade() {
                if d.get_skip_checked() {
                    update::skip_version(&latest_version);
                }
                let _ = d.hide();
            }
            UPDATE_DIALOG.with(|h| *h.borrow_mut() = None);
        });
    }

    // Title-bar close. Same as "remind me later".
    {
        let weak = dialog.as_weak();
        let latest_version = info.latest_version.clone();
        dialog.window().on_close_requested(move || {
            if let Some(d) = weak.upgrade()
                && d.get_skip_checked()
            {
                update::skip_version(&latest_version);
            }
            clear_slot_soon(&UPDATE_DIALOG);
            CloseRequestResponse::HideWindow
        });
    }

    if dialog.show().is_ok() {
        UPDATE_DIALOG.with(|h| *h.borrow_mut() = Some(dialog));
    }
}

// =============================================================================
// Donation dialog — post-update prompt linking to the support page.
// =============================================================================

/// Post-update prompt shown once after the user installs a newer
/// build. It nudges toward the support page and carries the "don't
/// show again" checkbox that governs whether this prompt fires again.
/// The button opens `support_url`; the platform links (GitHub
/// Sponsors, Buy Me a Coffee) live on that page, so the binary never
/// bakes in a donation URL that could go stale.
pub fn show_donation_dialog(version: &str, strings: &'static Strings) {
    let already_open = DONATION_DIALOG.with(|h| h.borrow().is_some());
    if already_open {
        return;
    }

    let dialog = match DonationDialog::new() {
        Ok(d) => d,
        Err(_) => return,
    };

    let body = strings.donation_prompt.replacen("{}", version, 1);

    dialog.set_dialog_title(SharedString::from(strings.donation_title));
    dialog.set_message_text(SharedString::from(body));
    dialog.set_dont_show_label(SharedString::from(strings.donation_dont_show_checkbox));
    dialog.set_btn_support(SharedString::from(strings.donation_btn_support));
    dialog.set_btn_later(SharedString::from(strings.donation_btn_later));

    // Open the support page. The "don't show again" checkbox is a hard
    // dismissal, so honor it before leaving.
    {
        let weak = dialog.as_weak();
        dialog.on_support_clicked(move || {
            if let Some(d) = weak.upgrade() {
                if d.get_dont_show_checked() {
                    update::dismiss_donation();
                }
                let _ = d.hide();
            }
            update::open_url(strings.support_url);
            DONATION_DIALOG.with(|h| *h.borrow_mut() = None);
        });
    }

    // Maybe next time. Counts as a skip unless "don't show again" is
    // ticked, in which case it's a hard dismissal instead.
    {
        let weak = dialog.as_weak();
        dialog.on_later_clicked(move || {
            if let Some(d) = weak.upgrade() {
                if d.get_dont_show_checked() {
                    update::dismiss_donation();
                } else {
                    update::record_donation_skip();
                }
                let _ = d.hide();
            }
            DONATION_DIALOG.with(|h| *h.borrow_mut() = None);
        });
    }

    // Title-bar close. Same as "maybe next time".
    {
        let weak = dialog.as_weak();
        dialog.window().on_close_requested(move || {
            if let Some(d) = weak.upgrade() {
                if d.get_dont_show_checked() {
                    update::dismiss_donation();
                } else {
                    update::record_donation_skip();
                }
            }
            clear_slot_soon(&DONATION_DIALOG);
            CloseRequestResponse::HideWindow
        });
    }

    if dialog.show().is_ok() {
        DONATION_DIALOG.with(|h| *h.borrow_mut() = Some(dialog));
    }
}
