//! UI strings for the config GUI, with English + Japanese translations.
//!
//! Selection order:
//! 1. `HKCU\Software\ArcThumb\Language` registry override (`"en"` | `"ja"`).
//! 2. OS default locale via `GetUserDefaultLocaleName` — starts with `"ja"` → Japanese.
//! 3. English fallback.
//!
//! Strings are handed to the Slint UI at startup via `in` properties.
//! A future refactor may move them into `.slint` `@tr("...")` with
//! gettext once the gettext toolchain (`xgettext`/`msgfmt`) is wired
//! into the build.

use winreg::RegKey;
use winreg::enums::*;

pub struct Strings {
    pub window_title: &'static str,
    pub menu_file: &'static str,
    pub menu_file_exit: &'static str,
    pub menu_help: &'static str,
    pub menu_help_check_updates: &'static str,
    pub menu_help_donate: &'static str,
    pub menu_help_about: &'static str,
    pub group_extensions: &'static str,
    pub group_image_exts: &'static str,
    pub group_sort: &'static str,
    pub sort_natural: &'static str,
    pub sort_alphabetical: &'static str,
    pub group_cover: &'static str,
    pub cover_prefer: &'static str,
    pub cover_only: &'static str,
    pub cover_ignore: &'static str,
    pub group_other: &'static str,
    pub cb_enable_preview: &'static str,
    pub cb_overlay_border: &'static str,
    pub cb_overlay_label: &'static str,
    pub btn_ok: &'static str,
    pub btn_cancel: &'static str,
    pub btn_apply: &'static str,
    pub btn_regenerate: &'static str,
    pub btn_close: &'static str,
    pub about_title: &'static str,
    pub about_body: &'static str,
    pub regen_confirm: &'static str,
    pub regen_done: &'static str,
    pub regen_partial: &'static str,
    pub error_title: &'static str,
    pub error_save: &'static str,
    pub error_register: &'static str,
    pub error_elevation_declined: &'static str,
    pub error_gui_init: &'static str,
    // Update check dialog
    pub update_title: &'static str,
    pub update_available: &'static str,
    pub update_skip_checkbox: &'static str,
    pub update_btn_open: &'static str,
    pub update_btn_later: &'static str,
    // Manual "check for updates", triggered from the Help menu.
    pub update_check_title: &'static str,
    // `{}` is replaced with the running version.
    pub update_up_to_date: &'static str,
    pub update_check_failed: &'static str,
    // Donation dialog
    pub donation_title: &'static str,
    pub donation_prompt: &'static str,
    pub donation_dont_show_checkbox: &'static str,
    pub donation_btn_support: &'static str,
    pub donation_btn_later: &'static str,
    // The support page opened from Help → Support and from the
    // post-update prompt. Locale-specific path so an English UI lands
    // on the English page and a Japanese UI on the Japanese one. This
    // is the only donation URL baked into the binary — the platform
    // links (GitHub Sponsors, Buy Me a Coffee) live on that page, so
    // they can change without an app rebuild.
    pub support_url: &'static str,
}

pub const EN: Strings = Strings {
    window_title: "ArcThumb Configuration",
    menu_file: "File",
    menu_file_exit: "Exit",
    menu_help: "Help",
    menu_help_check_updates: "Check for updates",
    menu_help_donate: "Support ArcThumb",
    menu_help_about: "About ArcThumb",
    group_extensions: "Enabled extensions",
    group_image_exts: "Image formats used for thumbnails (inside archives)",
    group_sort: "Sort order",
    sort_natural: "Natural (page2 < page10)",
    sort_alphabetical: "Alphabetical",
    group_cover: "Cover image (cover / folder / thumb / thumbnail / front)",
    cover_prefer: "Use cover if present, else first page",
    cover_only: "Cover only (no thumbnail otherwise)",
    cover_ignore: "Always use first page",
    group_other: "Other settings",
    cb_enable_preview: "Enable preview pane (Alt+P)",
    cb_overlay_border: "Mark archives with a coloured border",
    cb_overlay_label: "Mark archives with a format label (CBZ, EPUB, ...)",
    btn_ok: "OK",
    btn_cancel: "Cancel",
    btn_apply: "Apply",
    btn_regenerate: "Regenerate thumbnails",
    btn_close: "Close",
    about_title: "About ArcThumb",
    about_body: "ArcThumb — archive thumbnail provider for Windows Explorer.\n\nThis application uses Slint (https://slint.dev) under the Slint Royalty-Free License 2.0.",
    regen_confirm: "This will close all Explorer windows, delete the Windows thumbnail and icon caches, and restart Explorer.\n\nUse this if archive thumbnails are still missing after installing or enabling new file types, or to apply a change to the identification overlay.\n\nContinue?",
    regen_done: "Thumbnail cache cleared and Explorer restarted.\n\nNew thumbnails will be generated as you browse.",
    regen_partial: "Some cache files were locked and could not be deleted. Try closing other applications and run this again.",
    error_title: "ArcThumb",
    error_save: "Failed to save settings to the registry.",
    error_register: "Failed to update shell extension registration.",
    error_elevation_declined: "ArcThumb is installed for all users, so changing the enabled extensions or the preview pane needs administrator rights.\n\nThose two settings were not changed.",
    error_gui_init: "Failed to initialize the configuration UI. The graphics backend could not start. This can happen on systems without GPU acceleration (for example Windows Sandbox).",
    update_title: "Update available",
    update_available: "A new version of ArcThumb is available: v{}  (current: v{})",
    update_skip_checkbox: "Skip this version",
    update_btn_open: "Open download page",
    update_btn_later: "Remind me later",
    update_check_title: "Check for updates",
    update_up_to_date: "You're on the latest version (v{}).",
    update_check_failed: "Could not check for updates. Check your internet connection and try again.",
    donation_title: "Thank you for updating!",
    donation_prompt: "ArcThumb has been updated to v{}.\nWould you like to support development?",
    donation_dont_show_checkbox: "Don't show this again",
    donation_btn_support: "Open support page",
    donation_btn_later: "Maybe next time",
    support_url: "https://citrussoda.com/en/arcthumb/sponsor",
};

pub const JA: Strings = Strings {
    window_title: "ArcThumb 設定",
    menu_file: "ファイル",
    menu_file_exit: "終了",
    menu_help: "ヘルプ",
    menu_help_check_updates: "更新を確認",
    menu_help_donate: "ArcThumb を支援する",
    menu_help_about: "ArcThumb について",
    group_extensions: "有効にする拡張子",
    group_image_exts: "サムネイルに使う画像形式 (アーカイブ内)",
    group_sort: "並び順",
    sort_natural: "自然順 (page2 < page10)",
    sort_alphabetical: "アルファベット順",
    group_cover: "カバー画像 (cover / folder / thumb / thumbnail / front)",
    cover_prefer: "カバーを優先（無ければ先頭ページ）",
    cover_only: "カバーがあるときだけ表示（無ければ通常アイコン）",
    cover_ignore: "常に先頭ページを使う",
    group_other: "その他の設定",
    cb_enable_preview: "プレビュー ウィンドウを有効にする (Alt+P)",
    cb_overlay_border: "アーカイブを色付きの枠線で示す",
    cb_overlay_label: "アーカイブにフォーマットラベルを表示 (CBZ, EPUB, ...)",
    btn_ok: "OK",
    btn_cancel: "キャンセル",
    btn_apply: "適用",
    btn_regenerate: "サムネイルを再生成",
    btn_close: "閉じる",
    about_title: "ArcThumb について",
    about_body: "ArcThumb — Windows エクスプローラー向けのアーカイブサムネイル プロバイダー。\n\nこのアプリケーションは Slint (https://slint.dev) を Slint Royalty-Free License 2.0 に基づいて使用しています。",
    regen_confirm: "エクスプローラーのウィンドウをすべて閉じ、Windows のサムネイル/アイコンキャッシュを削除してエクスプローラーを再起動します。\n\nインストール後や対応拡張子を有効にしたあとでサムネイルが表示されない場合や、識別オーバーレイの設定を変更したあとに使ってください。\n\n続行しますか？",
    regen_done: "サムネイルキャッシュを削除し、エクスプローラーを再起動しました。\n\nフォルダを開くと新しいサムネイルが作成されます。",
    regen_partial: "一部のキャッシュファイルがロックされていて削除できませんでした。他のアプリを閉じてから、もう一度実行してください。",
    error_title: "ArcThumb",
    error_save: "設定の保存に失敗しました。",
    error_register: "シェル拡張の登録状態の更新に失敗しました。",
    error_elevation_declined: "ArcThumb はすべてのユーザー向けにインストールされているため、有効な拡張子とプレビューウィンドウの変更には管理者権限が必要です。\n\nこの 2 つの設定は変更されませんでした。",
    error_gui_init: "設定 UI の初期化に失敗しました。グラフィックスバックエンドを開始できませんでした。GPU アクセラレーションが利用できない環境 (Windows Sandbox など) で発生することがあります。",
    update_title: "アップデート通知",
    update_available: "ArcThumb の新しいバージョンがあります: v{}  (現在: v{})",
    update_skip_checkbox: "このバージョンをスキップ",
    update_btn_open: "ダウンロードページを開く",
    update_btn_later: "あとで通知",
    update_check_title: "更新の確認",
    update_up_to_date: "最新バージョンです (v{})。",
    update_check_failed: "更新を確認できませんでした。インターネット接続を確認して、もう一度お試しください。",
    donation_title: "アップデートありがとうございます！",
    donation_prompt: "ArcThumb v{} にアップデートされました。\n開発を支援しますか？",
    donation_dont_show_checkbox: "今後表示しない",
    donation_btn_support: "支援ページを開く",
    donation_btn_later: "また今度",
    support_url: "https://citrussoda.com/arcthumb/sponsor",
};

/// Resolve the UI language to use right now.
pub fn current() -> &'static Strings {
    // 1. Registry override
    if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\ArcThumb")
        && let Ok(lang) = key.get_value::<String, _>("Language")
    {
        match lang.to_ascii_lowercase().as_str() {
            "en" | "english" => return &EN,
            "ja" | "japanese" | "jp" => return &JA,
            _ => {}
        }
    }

    // 2. OS default locale
    if detect_os_locale_is_japanese() {
        return &JA;
    }

    // 3. Fallback
    &EN
}

fn detect_os_locale_is_japanese() -> bool {
    use windows::Win32::Globalization::GetUserDefaultLocaleName;

    // LOCALE_NAME_MAX_LENGTH = 85
    let mut buf = [0u16; 85];
    let len = unsafe { GetUserDefaultLocaleName(&mut buf) };
    if len <= 0 {
        return false;
    }
    let end = (len as usize).saturating_sub(1);
    let s = String::from_utf16_lossy(&buf[..end]);
    s.to_ascii_lowercase().starts_with("ja")
}
