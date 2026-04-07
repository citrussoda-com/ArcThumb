//! UI strings for the config GUI, with English + Japanese translations.
//!
//! Selection order:
//! 1. `HKCU\Software\ArcThumb\Language` registry override (`"en"` | `"ja"`).
//! 2. OS default locale via `GetUserDefaultLocaleName` — starts with `"ja"` → Japanese.
//! 3. English fallback.
//!
//! The GUI only covers settings (sort order, cover priority, per-extension
//! toggling). Application install/uninstall is the installer's job and
//! isn't surfaced here, so the string table stays small.

use winreg::enums::*;
use winreg::RegKey;

pub struct Strings {
    pub window_title: &'static str,
    pub group_extensions: &'static str,
    pub group_sort: &'static str,
    pub sort_natural: &'static str,
    pub sort_alphabetical: &'static str,
    pub cb_prefer_cover: &'static str,
    pub cb_enable_preview: &'static str,
    pub btn_ok: &'static str,
    pub btn_cancel: &'static str,
    pub btn_apply: &'static str,
    pub error_title: &'static str,
    pub error_save: &'static str,
    pub error_register: &'static str,
}

pub const EN: Strings = Strings {
    window_title: "ArcThumb Configuration",
    // Group-title strings carry 4 leading + 1 trailing spaces so
    // the label background extends noticeably to the left of the
    // visible text (and only barely past its right edge) when
    // painted over the frame's top border.
    group_extensions: "    Enabled extensions ",
    group_sort: "    Sort order ",
    sort_natural: "Natural (page2 < page10)",
    sort_alphabetical: "Alphabetical",
    cb_prefer_cover: "Prefer cover / folder / thumb / thumbnail / front",
    cb_enable_preview: "Enable preview pane (Alt+P)",
    btn_ok: "OK",
    btn_cancel: "Cancel",
    btn_apply: "Apply",
    error_title: "ArcThumb",
    error_save: "Failed to save settings to the registry.",
    error_register: "Failed to update shell extension registration.",
};

pub const JA: Strings = Strings {
    window_title: "ArcThumb 設定",
    group_extensions: "    有効にする拡張子 ",
    group_sort: "    並び順 ",
    sort_natural: "自然順 (page2 < page10)",
    sort_alphabetical: "アルファベット順",
    cb_prefer_cover: "cover / folder / thumb / thumbnail / front を優先",
    cb_enable_preview: "プレビュー ウィンドウを有効にする (Alt+P)",
    btn_ok: "OK",
    btn_cancel: "キャンセル",
    btn_apply: "適用",
    error_title: "ArcThumb",
    error_save: "設定の保存に失敗しました。",
    error_register: "シェル拡張の登録状態の更新に失敗しました。",
};

/// Resolve the UI language to use right now.
pub fn current() -> &'static Strings {
    // 1. Registry override
    if let Ok(key) = RegKey::predef(HKEY_CURRENT_USER).open_subkey("Software\\ArcThumb") {
        if let Ok(lang) = key.get_value::<String, _>("Language") {
            match lang.to_ascii_lowercase().as_str() {
                "en" | "english" => return &EN,
                "ja" | "japanese" | "jp" => return &JA,
                _ => {}
            }
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
