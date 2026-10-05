//! The ArcThumb settings dialog, built on Slint.
//!
//! The layout and widget tree live in `ui/main.slint`. This module
//! wires the Slint window to the rest of the binary: loads the
//! initial model from the registry, pushes it into the Slint
//! properties, hooks up the menu and button callbacks, and
//! delegates sub-dialogs (About / Update / Donation) to the
//! `dialogs` module and background update polling to
//! `update_check`.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use arcthumb::elevation;
use arcthumb::registry::Scope;
use arcthumb::settings::{SUPPORTED_IMAGE_EXTS, Settings, SortOrder};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, VecModel};

use crate::apply::{self, ApplyAction, RealRegistryOps};
use crate::cache;
use crate::cli;
use crate::dialogs;
use crate::elevate::{self, Elevated};
use crate::extension_model::ExtensionModel;
use crate::locale::{self, LanguageChoice};
use crate::message_box;
use crate::state::{self, EXT_COUNT, UiModel};
use crate::update;
use crate::update_check;

slint::include_modules!();

// `slint::include_modules!()` emits `pub` types directly into this
// module, so sibling modules access them as `crate::ui::ExtensionEntry`
// (and `crate::ui::AboutDialog` etc.) without an explicit re-export.

/// Launch the settings GUI. Blocks on the Slint event loop.
pub fn run_gui() -> Result<(), slint::PlatformError> {
    let window = MainWindow::new()?;
    // Slint learns which translations are bundled when the first
    // component is created, so the language can only be chosen now.
    locale::activate(locale::resolve());
    window.set_language_names(language_names_model(&window));

    let initial_model = UiModel::load();
    let lists = ExtensionLists::from_model(&initial_model);
    lists.bind(&window);

    // Drain pending `changed` handlers *before* we push the loaded
    // values. The Sort and Cover ComboBoxes reset their current-index
    // to 0 whenever their `model` changes (see combobox-base.slint:
    // `changed model => reset-current()`), and the model does change
    // once here — `locale::activate` above switched their `@tr` labels
    // to the chosen language. That reset is
    // queued, not immediate: it would otherwise fire on the first
    // event-loop iteration, *after* push_model, and snap both dropdowns
    // back to the first item. Running the handlers now consumes the
    // reset while the indices are still 0 anyway, so the values we push
    // next are the final word. The two-way (`<=>`) bindings in
    // main.slint are what let push_model drive the index afterwards; a
    // one-way binding would have been severed by the reset's assignment.
    slint::platform::update_timers_and_animations();
    push_model(&window, &initial_model);
    // Same ordering constraint as push_model: the language dropdown is
    // a ComboBox whose model was just localized.
    window.set_language_index(locale::language_override().to_index());
    let state = Rc::new(RefCell::new(initial_model));

    // OK
    {
        let weak = window.as_weak();
        let state = Rc::clone(&state);
        let lists = lists.clone();
        window.on_ok_clicked(move || {
            if let Some(w) = weak.upgrade()
                && apply_changes(&w, &state, &lists)
            {
                let _ = w.hide();
            }
        });
    }

    // Apply
    {
        let weak = window.as_weak();
        let state = Rc::clone(&state);
        let lists = lists.clone();
        window.on_apply_clicked(move || {
            if let Some(w) = weak.upgrade() {
                let _ = apply_changes(&w, &state, &lists);
            }
        });
    }

    // Cancel
    {
        let weak = window.as_weak();
        window.on_cancel_clicked(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
        });
    }

    // Regenerate
    {
        let weak = window.as_weak();
        window.on_regenerate_clicked(move || {
            if let Some(w) = weak.upgrade() {
                handle_regenerate(&w);
            }
        });
    }

    // Help → Support — opens the support page in the browser. The
    // page hosts the platform links, so they can change without an
    // app rebuild.
    {
        let weak = window.as_weak();
        window.on_donate_clicked(move || {
            if let Some(w) = weak.upgrade() {
                update::open_url(&w.global::<Texts>().get_support_url());
            }
        });
    }

    // Help → Check for updates — manual check that ignores the 24-hour
    // throttle and any skipped version.
    {
        let weak = window.as_weak();
        window.on_check_updates_clicked(move || {
            update_check::run_manual_check(weak.clone());
        });
    }

    // Help → About
    window.on_about_clicked(move || {
        dialogs::show_about();
    });

    // File → Exit
    {
        let weak = window.as_weak();
        window.on_exit_clicked(move || {
            if let Some(w) = weak.upgrade() {
                let _ = w.hide();
            }
        });
    }

    // Donation prompt — fires once after the event loop starts so we
    // can show a Slint window (which only paints while the loop is
    // running). `Timer::single_shot` is the associated form that
    // self-manages: do NOT use `Timer::default().start(...)` here,
    // because that returns an owned `Timer` whose `Drop` cancels the
    // timer immediately when the value goes out of scope.
    let donation_version = update::should_show_donation();
    // Recorded on every launch, prompt or not: the next launch compares
    // against it to tell whether the binary was updated in between.
    update::record_run_version();
    if let Some(ver) = donation_version {
        let support_url = window.global::<Texts>().get_support_url().to_string();
        Timer::single_shot(Duration::ZERO, move || {
            dialogs::show_donation_dialog(&ver, support_url);
        });
    }

    // Background update check — non-blocking. The result is marshalled
    // back onto the UI thread via `slint::invoke_from_event_loop`.
    update_check::start_update_check();

    window.run()?;
    Ok(())
}

// =============================================================================
// Extension-list bundle
// =============================================================================

/// The toggle lists ArcThumb exposes in the GUI: the per-archive
/// shell registration list, the per-image-format thumbnail
/// eligibility list, and the per-archive overlay list. Bundled so
/// `run_gui` doesn't have to clone and pass each `ExtensionModel`
/// through every callback.
#[derive(Clone)]
struct ExtensionLists {
    archive: ExtensionModel,
    image: ExtensionModel,
    overlay: ExtensionModel,
}

impl ExtensionLists {
    fn from_model(m: &UiModel) -> Self {
        Self {
            archive: ExtensionModel::from_enabled(&m.ext_enabled),
            image: ExtensionModel::from_names_and_enabled(
                SUPPORTED_IMAGE_EXTS,
                &m.image_ext_enabled,
            ),
            overlay: ExtensionModel::from_enabled(&state::overlay_ext_mask_to_vec(
                m.settings.overlay_exts_mask,
            )),
        }
    }

    fn bind(&self, window: &MainWindow) {
        window.set_extensions(self.archive.as_model());
        window.set_image_extensions(self.image.as_model());
        window.set_overlay_extensions(self.overlay.as_model());
        let archive = self.archive.clone();
        window.on_toggle_extension(move |i| archive.toggle(i as usize));
        let image = self.image.clone();
        window.on_toggle_image_extension(move |i| image.toggle(i as usize));
        let overlay = self.overlay.clone();
        window.on_toggle_overlay_extension(move |i| overlay.toggle(i as usize));
    }

    fn refresh_from(&self, m: &UiModel) {
        self.archive.replace_enabled(&m.ext_enabled);
        self.image.replace_enabled(&m.image_ext_enabled);
        self.overlay
            .replace_enabled(&state::overlay_ext_mask_to_vec(
                m.settings.overlay_exts_mask,
            ));
    }
}

// =============================================================================
// Model ⇄ Slint properties
// =============================================================================

/// Entries of the language dropdown, in `LanguageChoice::to_index`
/// order: the translated "Automatic" label first, then each bundled
/// language in its own language. Built here rather than in `.slint`
/// because the language list is generated at build time from the
/// catalogs under `lang/`.
fn language_names_model(window: &MainWindow) -> ModelRc<SharedString> {
    let mut names = vec![window.global::<Texts>().get_language_auto()];
    names.extend(
        locale::LANGUAGES
            .iter()
            .map(|tag| SharedString::from(locale::native_name(tag))),
    );
    ModelRc::new(VecModel::from(names))
}

/// Push the non-extension parts of `model` into the Slint window.
/// Extensions are handled separately by `ExtensionModel::replace_enabled`
/// because they live in a `VecModel` rather than scalar properties.
fn push_model(window: &MainWindow, model: &UiModel) {
    window.set_sort_index(if matches!(model.settings.sort_order, SortOrder::Natural) {
        0
    } else {
        1
    });
    window.set_cover_mode(state::cover_mode_to_index(model.settings.cover_mode));
    window.set_enable_preview(model.preview_enabled);
    window.set_overlay_border(model.settings.overlay_border);
    window.set_overlay_label(model.settings.overlay_label);
}

fn collect_from_ui(
    window: &MainWindow,
    lists: &ExtensionLists,
) -> (Settings, [bool; EXT_COUNT], bool) {
    let ext_enabled = lists.archive.enabled_array::<EXT_COUNT>();
    let sort_order = if window.get_sort_index() == 0 {
        SortOrder::Natural
    } else {
        SortOrder::Alphabetical
    };
    let image_mask = state::image_ext_vec_to_mask(&lists.image.enabled_vec());
    let settings = Settings {
        sort_order,
        cover_mode: state::cover_mode_from_index(window.get_cover_mode()),
        enabled_image_exts_mask: image_mask,
        overlay_border: window.get_overlay_border(),
        overlay_label: window.get_overlay_label(),
        overlay_exts_mask: state::overlay_ext_vec_to_mask(&lists.overlay.enabled_vec()),
    };
    (settings, ext_enabled, window.get_enable_preview())
}

// =============================================================================
// Apply
// =============================================================================

fn apply_changes(
    window: &MainWindow,
    state: &Rc<RefCell<UiModel>>,
    lists: &ExtensionLists,
) -> bool {
    let texts = window.global::<Texts>();
    let (new_settings, new_ext_enabled, new_preview_enabled) = collect_from_ui(window, lists);

    let plan = apply::compute_apply_plan(
        &state.borrow(),
        new_settings,
        new_ext_enabled,
        new_preview_enabled,
    );
    // Mutate whichever hive the loaded model came from, so an Apply
    // on a per-machine install doesn't silently bifurcate into HKCU.
    let scope = state.borrow().scope;
    let ops = RealRegistryOps::new(scope);
    let (shell, local): (Vec<ApplyAction>, Vec<ApplyAction>) =
        plan.into_iter().partition(ApplyAction::touches_shell);
    // HKLM is read-only for this process unless it was started
    // elevated. Settings live in HKCU and are saved here either way;
    // the registration changes go to an elevated copy of the exe.
    let needs_elevation =
        scope == Scope::PerMachine && !shell.is_empty() && !elevation::is_elevated();
    let mut elevated_ok = true;
    let outcome = if needs_elevation {
        let outcome = apply::apply_plan(&local, &ops);
        if outcome.is_ok() {
            elevated_ok = apply_shell_elevated(scope, &shell, &texts);
        }
        outcome
    } else {
        let mut plan = local;
        plan.extend(shell);
        apply::apply_plan(&plan, &ops)
    };

    if let Some(detail) = &outcome.settings_save_error {
        message_box::error(
            &texts.get_app_title(),
            &format!("{}\n\n{detail}", texts.get_error_save()),
        );
    }
    if !outcome.failed_extensions.is_empty() {
        message_box::error(
            &texts.get_app_title(),
            &format!(
                "{}\n\nfailed: {}",
                texts.get_error_register(),
                outcome.failed_extensions.join(", ")
            ),
        );
    }
    if let Some(detail) = &outcome.preview_error {
        message_box::error(
            &texts.get_app_title(),
            &format!("{}\n\n{detail}", texts.get_error_register()),
        );
    }

    // The UI language is a preference of this tool, not part of the
    // thumbnail settings or the shell registration, so it stays out of
    // the apply plan. It takes effect on the next launch.
    let language = LanguageChoice::from_index(window.get_language_index());
    let mut language_ok = true;
    if language != locale::language_override()
        && let Err(e) = locale::set_language_override(language)
    {
        language_ok = false;
        message_box::error(
            &texts.get_app_title(),
            &format!("{}\n\n{e}", texts.get_error_save()),
        );
    }

    let reloaded = UiModel::load();
    push_model(window, &reloaded);
    lists.refresh_from(&reloaded);
    *state.borrow_mut() = reloaded;

    outcome.is_ok() && elevated_ok && language_ok
}

/// Hand the registration changes to an elevated copy of this exe and
/// report the result to the user. Returns `true` when they were
/// applied.
fn apply_shell_elevated(scope: Scope, shell: &[ApplyAction], texts: &Texts<'_>) -> bool {
    let mut args = vec![
        cli::APPLY_SHELL_FLAG.to_string(),
        cli::scope_arg(scope).to_string(),
    ];
    args.extend(apply::shell_actions_to_args(shell));

    let detail = match elevate::run_self_elevated(&args) {
        Ok(Elevated::Exited(0)) => return true,
        Ok(Elevated::Declined) => {
            message_box::error(
                &texts.get_app_title(),
                &texts.get_error_elevation_declined(),
            );
            return false;
        }
        Ok(Elevated::Exited(code)) => format!("exit code {code}"),
        Err(e) => e.to_string(),
    };
    message_box::error(
        &texts.get_app_title(),
        &format!("{}\n\n{detail}", texts.get_error_register()),
    );
    false
}

// =============================================================================
// Regenerate thumbnails
// =============================================================================

fn handle_regenerate(window: &MainWindow) {
    let texts = window.global::<Texts>();
    if !message_box::confirm_warning(&texts.get_app_title(), &texts.get_regen_confirm()) {
        return;
    }
    match cache::wipe_thumbnail_cache() {
        Ok(report) if report.failed.is_empty() => {
            message_box::info(&texts.get_app_title(), &texts.get_regen_done());
        }
        Ok(_) => {
            message_box::error(&texts.get_app_title(), &texts.get_regen_partial());
        }
        Err(e) => {
            message_box::error(
                &texts.get_app_title(),
                &format!("{}\n\n{e}", texts.get_regen_partial()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    //! Slint glue tests.
    //!
    //! All Slint-touching assertions live inside a single `#[test]`
    //! function on purpose: `i_slint_backend_testing::init_no_event_loop()`
    //! pins the test platform to the thread that first called it,
    //! and the cargo test harness runs tests on independent worker
    //! threads. Splitting these into multiple `#[test]`s causes the
    //! second-and-later tests to land on a different worker and
    //! crash with "The Slint platform was initialized in another
    //! thread".
    //!
    //! Each subsection is wrapped in its own block + comment so
    //! `cargo test` failure messages still pinpoint the failing
    //! assertion. The cost of bundling them is one test entry in
    //! the report; the benefit is reliable execution under the
    //! default parallel test runner.

    use super::*;
    use arcthumb::settings::{CoverMode, SortOrder};
    use slint::Model;

    fn baseline_model() -> UiModel {
        let mut ext = [false; EXT_COUNT];
        // A non-trivial subset so a flipped index is detectable.
        ext[0] = true; // .zip
        ext[2] = true; // .rar
        ext[7] = true; // .epub
        let settings = Settings::default();
        UiModel {
            image_ext_enabled: state::image_ext_mask_to_vec(settings.enabled_image_exts_mask),
            settings,
            scope: arcthumb::registry::Scope::PerUser,
            ext_enabled: ext,
            preview_enabled: true,
        }
    }

    #[test]
    fn slint_glue_round_trips_and_localises() {
        i_slint_backend_testing::init_no_event_loop();

        // ---- push_then_collect_round_trips_full_model -----------
        {
            let window = MainWindow::new().expect("create MainWindow");
            let original = baseline_model();
            let lists = ExtensionLists::from_model(&original);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());

            push_model(&window, &original);
            let (settings, ext_enabled, preview) = collect_from_ui(&window, &lists);

            assert_eq!(settings, original.settings, "settings round-trip");
            assert_eq!(ext_enabled, original.ext_enabled, "ext_enabled round-trip");
            assert_eq!(preview, original.preview_enabled, "preview round-trip");
        }

        // ---- overlay toggles push then collect intact -----------
        {
            let window = MainWindow::new().expect("create MainWindow");
            let settings = Settings {
                overlay_border: true,
                overlay_label: true,
                ..Settings::default()
            };
            let model = UiModel {
                image_ext_enabled: state::image_ext_mask_to_vec(settings.enabled_image_exts_mask),
                settings,
                scope: arcthumb::registry::Scope::PerUser,
                ext_enabled: [true; EXT_COUNT],
                preview_enabled: false,
            };
            let lists = ExtensionLists::from_model(&model);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());

            push_model(&window, &model);
            let (collected, _, _) = collect_from_ui(&window, &lists);
            assert!(collected.overlay_border, "border toggle round-trips");
            assert!(collected.overlay_label, "label toggle round-trips");
        }

        // ---- per-extension overlay list round-trips and toggles --
        {
            let window = MainWindow::new().expect("create MainWindow");
            let all = arcthumb::settings::default_overlay_exts_mask();
            let settings = Settings {
                overlay_label: true,
                overlay_exts_mask: all & !(1u32 << 9), // .mobi off
                ..Settings::default()
            };
            let model = UiModel {
                image_ext_enabled: state::image_ext_mask_to_vec(settings.enabled_image_exts_mask),
                settings,
                scope: arcthumb::registry::Scope::PerUser,
                ext_enabled: [true; EXT_COUNT],
                preview_enabled: false,
            };
            let lists = ExtensionLists::from_model(&model);
            lists.bind(&window);
            push_model(&window, &model);

            let (collected, ext, _) = collect_from_ui(&window, &lists);
            assert_eq!(collected, settings, "overlay list round-trips");

            // Unticking .azw in the overlay grid changes the overlay
            // mask only; the registration list is a separate model.
            lists.overlay.toggle(10);
            let (collected, ext_after, _) = collect_from_ui(&window, &lists);
            assert_eq!(
                collected.overlay_exts_mask,
                all & !(1u32 << 9) & !(1u32 << 10),
                ".mobi and .azw are off"
            );
            assert_eq!(ext_after, ext, "registration list is untouched");

            // A reload from the registry snaps the grid back.
            lists.refresh_from(&model);
            let (collected, _, _) = collect_from_ui(&window, &lists);
            assert_eq!(collected.overlay_exts_mask, settings.overlay_exts_mask);
        }

        // ---- push_then_collect_round_trips_alphabetical_no_cover
        {
            let window = MainWindow::new().expect("create MainWindow");
            let settings = Settings {
                sort_order: SortOrder::Alphabetical,
                cover_mode: CoverMode::Ignore,
                ..Settings::default()
            };
            let model = UiModel {
                image_ext_enabled: state::image_ext_mask_to_vec(settings.enabled_image_exts_mask),
                settings,
                scope: arcthumb::registry::Scope::PerUser,
                ext_enabled: [true; EXT_COUNT],
                preview_enabled: false,
            };
            let lists = ExtensionLists::from_model(&model);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());

            push_model(&window, &model);
            let (settings, ext_enabled, preview) = collect_from_ui(&window, &lists);

            assert_eq!(settings.sort_order, SortOrder::Alphabetical);
            assert_eq!(settings.cover_mode, CoverMode::Ignore);
            assert_eq!(ext_enabled, [true; EXT_COUNT]);
            assert!(!preview);
        }

        // ---- cover_mode dropdown round-trips every CoverMode -------
        {
            for mode in [CoverMode::Prefer, CoverMode::Only, CoverMode::Ignore] {
                let window = MainWindow::new().expect("create MainWindow");
                let settings = Settings {
                    cover_mode: mode,
                    ..Settings::default()
                };
                let model = UiModel {
                    image_ext_enabled: state::image_ext_mask_to_vec(
                        settings.enabled_image_exts_mask,
                    ),
                    settings,
                    scope: arcthumb::registry::Scope::PerUser,
                    ext_enabled: [true; EXT_COUNT],
                    preview_enabled: false,
                };
                let lists = ExtensionLists::from_model(&model);
                window.set_extensions(lists.archive.as_model());
                window.set_image_extensions(lists.image.as_model());
                push_model(&window, &model);
                let (collected, _, _) = collect_from_ui(&window, &lists);
                assert_eq!(collected.cover_mode, mode, "cover_mode round-trip {mode:?}");
            }
        }

        // ---- dropdown indices survive the ComboBox model-change reset
        // Regression guard for #36: the Sort/Cover ComboBoxes reset
        // current-index to 0 when their model changes (their `@tr`
        // labels change with the language). run_gui drains that reset
        // *before* push_model, and the two-way bindings let push_model
        // set the index after. Here we replay that order, then run the
        // change handlers a second time (as the first real event-loop
        // iteration would) to prove the pushed values aren't snapped
        // back to the first item.
        {
            let window = MainWindow::new().expect("create MainWindow");
            locale::activate("ja"); // relabels the dropdowns -> queues reset
            window.set_language_names(language_names_model(&window));
            slint::platform::update_timers_and_animations(); // drain reset

            let settings = Settings {
                sort_order: SortOrder::Alphabetical,
                cover_mode: CoverMode::Only,
                ..Settings::default()
            };
            let model = UiModel {
                image_ext_enabled: state::image_ext_mask_to_vec(settings.enabled_image_exts_mask),
                settings,
                scope: arcthumb::registry::Scope::PerUser,
                ext_enabled: [true; EXT_COUNT],
                preview_enabled: false,
            };
            let lists = ExtensionLists::from_model(&model);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());
            push_model(&window, &model);

            // First event-loop iteration would run these again.
            slint::platform::update_timers_and_animations();

            assert_eq!(window.get_cover_mode(), 1, "cover index survives reset");
            assert_eq!(window.get_sort_index(), 1, "sort index survives reset");
        }

        // ---- toggle_extension_callback_path_via_extension_model
        {
            let window = MainWindow::new().expect("create MainWindow");
            let model = UiModel {
                settings: Settings::default(),
                scope: arcthumb::registry::Scope::PerUser,
                ext_enabled: [false; EXT_COUNT],
                image_ext_enabled: state::image_ext_mask_to_vec(
                    Settings::default().enabled_image_exts_mask,
                ),
                preview_enabled: false,
            };
            let lists = ExtensionLists::from_model(&model);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());
            push_model(&window, &model);
            lists.archive.toggle(5); // .cb7
            lists.archive.toggle(11); // .azw3

            let (_, ext, _) = collect_from_ui(&window, &lists);
            assert!(ext[5], ".cb7 should be on (index 5)");
            assert!(ext[11], ".azw3 should be on (index 11)");
            for (i, on) in ext.iter().enumerate() {
                if i != 5 && i != 11 {
                    assert!(!on, "index {i} should be off");
                }
            }
        }

        // ---- bundled_translation_switches_the_ui_language -------
        // Slint auto-picks a bundled language from the OS format
        // locale when the first component is created, so pin English
        // explicitly before reading anything, the way `run_gui` does
        // through `locale::activate`.
        {
            let window = MainWindow::new().expect("create MainWindow");
            let texts = window.global::<Texts>();

            locale::activate("en");
            assert_eq!(window.get_window_title(), "ArcThumb Configuration");
            assert_eq!(texts.get_regen_confirm().lines().last(), Some("Continue?"));
            assert_eq!(
                texts.invoke_update_up_to_date("1.2.3".into()),
                "You're on the latest version (v1.2.3)."
            );
            assert_eq!(
                texts.get_support_url(),
                "https://citrussoda.com/en/arcthumb/sponsor"
            );

            locale::activate("ja");
            assert_eq!(window.get_window_title(), "ArcThumb 設定");
            assert_eq!(
                texts.get_regen_confirm().lines().last(),
                Some("続行しますか？")
            );
            assert_eq!(
                texts.invoke_update_up_to_date("1.2.3".into()),
                "最新バージョンです (v1.2.3)。"
            );
            assert_eq!(
                texts.get_support_url(),
                "https://citrussoda.com/arcthumb/sponsor"
            );
            // An msgid left untranslated in the catalog shows the
            // English text, not an empty string.
            assert_eq!(texts.get_app_title(), "ArcThumb");

            // Every bundled catalog must at least translate the window
            // title; an untouched one is probably empty or has the
            // wrong msgids.
            for tag in locale::LANGUAGES.iter().skip(1) {
                locale::activate(tag);
                assert_ne!(
                    window.get_window_title(),
                    "ArcThumb Configuration",
                    "{tag}: window title is not translated"
                );
            }

            locale::activate("en");
        }

        // ---- language_dropdown_model_matches_language_choice ----
        // The dropdown entries are built in Rust; their order must be
        // the one `LanguageChoice::{to,from}_index` assumes.
        {
            let window = MainWindow::new().expect("create MainWindow");
            locale::activate("en");
            let names = language_names_model(&window);
            assert_eq!(names.row_count(), locale::LANGUAGES.len() + 1);
            assert_eq!(names.row_data(0).unwrap(), "Automatic (follow Windows)");
            for (i, tag) in locale::LANGUAGES.iter().enumerate() {
                let index = LanguageChoice::Fixed(tag).to_index() as usize;
                assert_eq!(index, i + 1);
                assert_eq!(names.row_data(index).unwrap(), locale::native_name(tag));
            }
        }

        // ---- ok_callback_with_no_changes_produces_empty_plan ---
        // The OK button calls compute_apply_plan against the
        // current `state`. With an unchanged model the plan must
        // be empty so we never touch HKCU on a stray click.
        {
            let window = MainWindow::new().expect("create MainWindow");
            let model = baseline_model();
            let lists = ExtensionLists::from_model(&model);
            window.set_extensions(lists.archive.as_model());
            window.set_image_extensions(lists.image.as_model());
            push_model(&window, &model);
            let (settings, ext_enabled, preview_enabled) = collect_from_ui(&window, &lists);
            let plan = apply::compute_apply_plan(&model, settings, ext_enabled, preview_enabled);
            assert!(
                plan.is_empty(),
                "round-trip with the same model should produce an empty plan, got {plan:?}"
            );
        }
    }
}
