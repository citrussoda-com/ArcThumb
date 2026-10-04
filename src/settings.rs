//! User-tweakable settings, read from `HKCU\Software\ArcThumb`.
//!
//! All keys are optional. Missing or malformed keys fall back to the
//! built-in defaults. Settings are loaded once per Explorer process
//! and cached — changes take effect after restarting Explorer.
//!
//! ## Registry layout
//!
//! ```text
//! HKEY_CURRENT_USER\Software\ArcThumb
//!     SortOrder         REG_SZ    "natural" | "alphabetical"
//!     CoverMode         REG_SZ    "ignore" | "prefer" | "only"
//!     EnabledImageExts  REG_DWORD bitmask over SUPPORTED_IMAGE_EXTS
//!     OverlayBorder     REG_DWORD 0 | 1
//!     OverlayLabel      REG_DWORD 0 | 1
//! ```
//!
//! Users can tweak these by hand in `regedit` until a proper config
//! GUI (Phase 4f.2) exists.

use std::cmp::Ordering;
use std::sync::OnceLock;

use winreg::RegKey;
use winreg::enums::*;

/// How to order image files within an archive before picking the
/// "first" one for the thumbnail.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Plain byte-wise sort. `page10.jpg` comes before `page2.jpg`.
    Alphabetical,
    /// Natural sort: runs of digits compared numerically so
    /// `page2.jpg` comes before `page10.jpg`. Default because
    /// page2 < page10 is what users expect for comic archives.
    #[default]
    Natural,
}

impl SortOrder {
    fn from_registry_value(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "alphabetical" | "alpha" => Some(Self::Alphabetical),
            "natural" | "nat" => Some(Self::Natural),
            _ => None,
        }
    }

    /// Canonical registry string form. Paired with `from_registry_value`.
    pub fn as_registry_value(self) -> &'static str {
        match self {
            Self::Alphabetical => "alphabetical",
            Self::Natural => "natural",
        }
    }
}

/// How cover-named images (`cover.*`, `folder.*`, `thumb.*`,
/// `thumbnail.*`, `front.*`) are treated when picking the thumbnail
/// source inside an archive. Names match case-insensitively and must
/// equal one of those stems exactly (`cover.png` qualifies,
/// `coverpage.png` does not).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum CoverMode {
    /// Ignore cover names entirely: always take the first image by
    /// sort order.
    Ignore,
    /// Prefer a cover-named image when present, otherwise fall back to
    /// the first image by sort order. The default.
    #[default]
    Prefer,
    /// Use a cover-named image only. When the archive has none, produce
    /// no thumbnail at all so Explorer shows the plain archive icon —
    /// this keeps incidental images inside unrelated archives (a stray
    /// screenshot in a work ZIP) from becoming the thumbnail.
    Only,
}

impl CoverMode {
    fn from_registry_value(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "ignore" => Some(Self::Ignore),
            "prefer" => Some(Self::Prefer),
            "only" => Some(Self::Only),
            _ => None,
        }
    }

    /// Canonical registry string form. Paired with `from_registry_value`.
    pub fn as_registry_value(self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Prefer => "prefer",
            Self::Only => "only",
        }
    }
}

/// Image extensions ArcThumb can decode when extracted from an
/// archive. This is the fixed compile-time *supported set*; the
/// user-facing `Settings::enabled_image_exts_mask` picks a subset.
/// Order is load-bearing: bit `i` of the mask refers to index `i`
/// here, so never reorder or delete entries — append only.
pub const SUPPORTED_IMAGE_EXTS: &[&str] = &[
    ".jpg",
    ".jpeg",
    ".png",
    ".gif",
    ".bmp",
    ".tiff",
    ".tif",
    ".webp",
    ".ico",
    #[cfg(feature = "jxl")]
    ".jxl",
];

/// All supported extensions enabled. Used as the factory default and
/// as the fallback when the registry key is missing or malformed.
pub const fn default_enabled_image_exts_mask() -> u32 {
    let n = SUPPORTED_IMAGE_EXTS.len();
    if n >= 32 { u32::MAX } else { (1u32 << n) - 1 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    pub sort_order: SortOrder,
    /// How cover-named images (`cover.*`, `folder.*`, `thumb.*`,
    /// `thumbnail.*`, `front.*`) are treated when picking the
    /// thumbnail source. See [`CoverMode`].
    pub cover_mode: CoverMode,
    /// Bitmask over `SUPPORTED_IMAGE_EXTS`: bit `i` set = extension
    /// at index `i` is eligible as a thumbnail source inside
    /// archives. Only bits < `SUPPORTED_IMAGE_EXTS.len()` are
    /// meaningful; higher bits are ignored.
    pub enabled_image_exts_mask: u32,
    /// Bake a coloured identification border into the thumbnail, so
    /// archives are easier to tell apart from plain images in
    /// Explorer. The colour reflects the format family (compressed
    /// archive / e-book / other). Off by default — opting in changes
    /// how every archive thumbnail looks, so existing installs keep
    /// the bare cover image until the user asks for the border.
    pub overlay_border: bool,
    /// Bake a small format label (`CBZ`, `EPUB`, …) into the corner
    /// of the thumbnail. Off by default for the same reason as
    /// [`Self::overlay_border`]. Dropped automatically at very small
    /// thumbnail sizes where the text would be unreadable.
    pub overlay_label: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sort_order: SortOrder::Natural,
            cover_mode: CoverMode::Prefer,
            enabled_image_exts_mask: default_enabled_image_exts_mask(),
            overlay_border: false,
            overlay_label: false,
        }
    }
}

/// Registry subkey under `HKCU` where settings live in production.
const SETTINGS_SUBKEY: &str = "Software\\ArcThumb";

impl Settings {
    /// Read settings from `HKCU\Software\ArcThumb` without touching the
    /// process-wide cache. The config GUI uses this so each "Apply"
    /// round sees fresh registry state.
    pub fn load_from_registry_uncached() -> Self {
        Self::load_from_subkey(SETTINGS_SUBKEY)
    }

    /// Core load routine, parameterised by subkey so tests can
    /// round-trip through a throwaway path without stomping on the
    /// user's real settings.
    fn load_from_subkey(subkey: &str) -> Self {
        let mut out = Self::default();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let Ok(key) = hkcu.open_subkey(subkey) else {
            return out;
        };

        if let Ok(s) = key.get_value::<String, _>("SortOrder")
            && let Some(order) = SortOrder::from_registry_value(&s)
        {
            out.sort_order = order;
        }
        // CoverMode (REG_SZ) is the current key. Older builds wrote
        // PreferCoverNames (REG_DWORD); read it as a fallback so an
        // existing install keeps its choice (1 -> Prefer, 0 -> Ignore).
        if let Ok(s) = key.get_value::<String, _>("CoverMode")
            && let Some(mode) = CoverMode::from_registry_value(&s)
        {
            out.cover_mode = mode;
        } else if let Ok(v) = key.get_value::<u32, _>("PreferCoverNames") {
            out.cover_mode = if v != 0 {
                CoverMode::Prefer
            } else {
                CoverMode::Ignore
            };
        }
        if let Ok(v) = key.get_value::<u32, _>("EnabledImageExts") {
            // Mask unused high bits so a stale value from a build
            // with more supported formats can't light up phantom
            // entries after a downgrade.
            out.enabled_image_exts_mask = v & default_enabled_image_exts_mask();
        }
        if let Ok(v) = key.get_value::<u32, _>("OverlayBorder") {
            out.overlay_border = v != 0;
        }
        if let Ok(v) = key.get_value::<u32, _>("OverlayLabel") {
            out.overlay_label = v != 0;
        }
        out
    }

    /// Write every setting to `HKCU\Software\ArcThumb`. Creates the
    /// key if missing. Leaves other values (e.g. `Language`) untouched.
    pub fn save_to_registry(&self) -> std::io::Result<()> {
        self.save_to_subkey(SETTINGS_SUBKEY)
    }

    /// Is `name` a candidate image under the current settings?
    /// Combines the compile-time supported set with the user's
    /// `enabled_image_exts_mask`. Case-insensitive.
    ///
    /// Hot path: called once per entry while listing archives. The
    /// earlier implementation allocated a lowercase copy of the whole
    /// filename on every call; this byte-level comparison avoids the
    /// allocation entirely and only touches the trailing bytes.
    pub fn accepts_image_ext(&self, name: &str) -> bool {
        SUPPORTED_IMAGE_EXTS.iter().enumerate().any(|(i, ext)| {
            (self.enabled_image_exts_mask & (1u32 << i)) != 0
                && ends_with_ignore_ascii_case(name, ext)
        })
    }

    /// Pick the "best" image from a list of candidates according to
    /// this settings snapshot. Applies `sort_order` and `cover_mode`.
    ///
    /// Each candidate reports (via [`ImageCandidate`]) the entry name
    /// used for sorting and cover detection, and the chosen candidate is
    /// returned whole. That lets a random-access backend carry its entry
    /// index through selection and read by index afterwards, while a
    /// sequential backend carries the name it will match on a second
    /// pass. Nothing here re-derives a name into a lookup key.
    ///
    /// In [`CoverMode::Only`] a list with no cover-named image yields
    /// `None`, which the archive backends turn into a "no image" error
    /// and ultimately a default Explorer icon.
    pub fn pick_first_image<T: ImageCandidate>(&self, mut items: Vec<T>) -> Option<T> {
        items.retain(|t| !is_junk_entry(t.name()));
        if items.is_empty() {
            return None;
        }
        match self.sort_order {
            SortOrder::Alphabetical => items.sort_by(|a, b| a.name().cmp(b.name())),
            SortOrder::Natural => items.sort_by(|a, b| natural_cmp(a.name(), b.name())),
        }
        match self.cover_mode {
            CoverMode::Ignore => items.into_iter().next(),
            CoverMode::Prefer => match items.iter().position(|t| is_cover_name(t.name())) {
                Some(pos) => Some(items.swap_remove(pos)),
                None => items.into_iter().next(),
            },
            CoverMode::Only => items.into_iter().find(|t| is_cover_name(t.name())),
        }
    }

    fn save_to_subkey(&self, subkey: &str) -> std::io::Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(subkey)?;
        key.set_value("SortOrder", &self.sort_order.as_registry_value())?;
        key.set_value("CoverMode", &self.cover_mode.as_registry_value())?;
        let mask = self.enabled_image_exts_mask & default_enabled_image_exts_mask();
        key.set_value("EnabledImageExts", &mask)?;
        let border: u32 = if self.overlay_border { 1 } else { 0 };
        key.set_value("OverlayBorder", &border)?;
        let label: u32 = if self.overlay_label { 1 } else { 0 };
        key.set_value("OverlayLabel", &label)?;
        Ok(())
    }
}

/// Process-wide cached settings. Loaded lazily on first use and
/// held for the lifetime of the Explorer process. Restart Explorer
/// to pick up registry edits.
pub fn current() -> &'static Settings {
    static CACHE: OnceLock<Settings> = OnceLock::new();
    CACHE.get_or_init(Settings::load_from_registry_uncached)
}

/// Allocation-free case-insensitive suffix check on ASCII bytes.
///
/// Only the trailing `suffix.len()` bytes of `s` are compared, so the
/// cost scales with the extension length (handful of bytes) rather
/// than the full path length. Non-ASCII bytes in `s` are compared
/// byte-for-byte — the supported extensions are all ASCII so this
/// cannot produce a false positive.
fn ends_with_ignore_ascii_case(s: &str, suffix: &str) -> bool {
    let s = s.as_bytes();
    let sfx = suffix.as_bytes();
    if s.len() < sfx.len() {
        return false;
    }
    let tail = &s[s.len() - sfx.len()..];
    tail.iter()
        .zip(sfx.iter())
        .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

// =============================================================================
// Image selection helpers (used by Settings::pick_first_image)
// =============================================================================

/// A candidate image entry that can report the name used to sort it and
/// to detect cover filenames. Implemented for a bare `String` (the name
/// is the candidate, used by the sequential backends that match by name
/// on a second pass) and for `(index, name)` (used by the random-access
/// ZIP backend, which carries the entry index through selection and then
/// reads by index).
pub trait ImageCandidate {
    fn name(&self) -> &str;
}

impl ImageCandidate for String {
    fn name(&self) -> &str {
        self
    }
}

impl ImageCandidate for (usize, String) {
    fn name(&self) -> &str {
        &self.1
    }
}

/// Is this path a well-known cover-image filename? Checks the
/// basename (without extension) against a small allowlist.
fn is_cover_name(path: &str) -> bool {
    // Take whatever's after the last `/` or `\` — archive formats
    // use both depending on origin.
    let basename = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = basename
        .rsplit_once('.')
        .map(|(s, _)| s)
        .unwrap_or(basename);
    matches!(
        stem.to_ascii_lowercase().as_str(),
        "cover" | "folder" | "thumb" | "thumbnail" | "front"
    )
}

/// Is this entry macOS metadata rather than a real image? Archives
/// made with Finder carry an AppleDouble sidecar for every file
/// (`__MACOSX/dir/._001.jpg`). It has the image's extension but holds
/// a resource fork, and `_` sorts ahead of letters, so without this it
/// would be picked first and then fail to decode.
fn is_junk_entry(path: &str) -> bool {
    let mut components = path.rsplit(['/', '\\']);
    let basename = components.next().unwrap_or(path);
    basename.starts_with("._") || components.any(|dir| dir == "__MACOSX")
}

/// Natural sort comparator: runs of ASCII digits compared as
/// integers, everything else compared case-insensitively byte-wise.
/// Non-ASCII characters compare by their UTF-8 byte order, which is
/// consistent (if not linguistically "correct") for Japanese.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < ab.len() && j < bb.len() {
        let (ac, bc) = (ab[i], bb[j]);
        if ac.is_ascii_digit() && bc.is_ascii_digit() {
            // Walk both numeric runs.
            let a_start = i;
            while i < ab.len() && ab[i].is_ascii_digit() {
                i += 1;
            }
            let b_start = j;
            while j < bb.len() && bb[j].is_ascii_digit() {
                j += 1;
            }
            let a_num = strip_leading_zeros(&ab[a_start..i]);
            let b_num = strip_leading_zeros(&bb[b_start..j]);
            // After stripping zeros, longer number = bigger magnitude.
            match a_num.len().cmp(&b_num.len()) {
                Ordering::Equal => match a_num.cmp(b_num) {
                    Ordering::Equal => continue,
                    ord => return ord,
                },
                ord => return ord,
            }
        } else {
            match ac.to_ascii_lowercase().cmp(&bc.to_ascii_lowercase()) {
                Ordering::Equal => {
                    i += 1;
                    j += 1;
                }
                ord => return ord,
            }
        }
    }
    // At least one side is used up. Whichever still has characters left
    // sorts after. The unconsumed lengths are what matter here, not the
    // totals: leading zeros make the consumed parts differ in length
    // ("a01" against "a1b"), and comparing totals called those two
    // equal. Total length only breaks ties between names that differ in
    // nothing but leading zeros, to keep the order deterministic.
    (ab.len() - i)
        .cmp(&(bb.len() - j))
        .then(ab.len().cmp(&bb.len()))
}

fn strip_leading_zeros(s: &[u8]) -> &[u8] {
    let start = s.iter().position(|&c| c != b'0').unwrap_or(s.len());
    &s[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_sort_pages() {
        let mut v = vec!["page10.jpg", "page2.jpg", "page1.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["page1.jpg", "page2.jpg", "page10.jpg"]);
    }

    #[test]
    fn natural_sort_leading_zeros() {
        // Leading zeros should not affect numeric ordering: 02 == 2.
        // After stripping zeros, equal-magnitude numbers fall back to
        // continuing past the run, so "page02.jpg" and "page2.jpg"
        // resolve by the rest of the string (here, identical → equal).
        let mut v = vec!["page002.jpg", "page1.jpg", "page03.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["page1.jpg", "page002.jpg", "page03.jpg"]);
    }

    #[test]
    fn natural_sort_case_insensitive() {
        let mut v = vec!["B.jpg", "a.jpg", "C.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["a.jpg", "B.jpg", "C.jpg"]);
    }

    #[test]
    fn natural_sort_mixed_text_and_numbers() {
        let mut v = vec![
            "ch10_page2.jpg",
            "ch2_page10.jpg",
            "ch10_page1.jpg",
            "ch2_page2.jpg",
        ];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            v,
            vec![
                "ch2_page2.jpg",
                "ch2_page10.jpg",
                "ch10_page1.jpg",
                "ch10_page2.jpg",
            ]
        );
    }

    #[test]
    fn natural_sort_equal_strings() {
        assert_eq!(natural_cmp("page01.jpg", "page01.jpg"), Ordering::Equal);
    }

    #[test]
    fn natural_sort_leading_zeros_do_not_hide_a_longer_tail() {
        // Same total length, but "a1b" has a character left after the
        // number and "a01" does not.
        assert_eq!(natural_cmp("a01", "a1b"), Ordering::Less);
        assert_eq!(natural_cmp("a1b", "a01"), Ordering::Greater);
        // The order has to be consistent across a third name.
        assert_eq!(natural_cmp("a01", "a1a"), Ordering::Less);
        assert_eq!(natural_cmp("a1a", "a1b"), Ordering::Less);
        // Names equal up to leading zeros still get a stable order.
        assert_eq!(natural_cmp("a1", "a01"), Ordering::Less);
        assert_eq!(natural_cmp("a01", "a1"), Ordering::Greater);
    }

    #[test]
    fn natural_sort_one_is_prefix() {
        // Shorter string compares less when it is a prefix of the other.
        assert_eq!(natural_cmp("page1", "page1.jpg"), Ordering::Less);
    }

    #[test]
    fn strip_leading_zeros_basic() {
        assert_eq!(strip_leading_zeros(b"0042"), b"42");
        assert_eq!(strip_leading_zeros(b"42"), b"42");
        assert_eq!(strip_leading_zeros(b"0000"), b"");
        assert_eq!(strip_leading_zeros(b""), b"");
    }

    #[test]
    fn cover_name_detection() {
        assert!(is_cover_name("cover.jpg"));
        assert!(is_cover_name("Cover.PNG"));
        assert!(is_cover_name("comic/cover.webp"));
        assert!(is_cover_name("folder.jpg"));
        assert!(is_cover_name("thumbnail.png"));
        assert!(!is_cover_name("page01.jpg"));
        assert!(!is_cover_name("recover.jpg"));
    }

    #[test]
    fn cover_name_handles_both_separators() {
        // Archive entries can use either / or \ depending on origin.
        assert!(is_cover_name("a/b/cover.jpg"));
        assert!(is_cover_name("a\\b\\cover.jpg"));
        assert!(is_cover_name("a/b\\cover.jpg"));
    }

    #[test]
    fn cover_name_no_extension() {
        assert!(is_cover_name("cover"));
        assert!(is_cover_name("FOLDER"));
        assert!(!is_cover_name("page1"));
    }

    #[test]
    fn cover_name_all_aliases() {
        for stem in &["cover", "folder", "thumb", "thumbnail", "front"] {
            assert!(is_cover_name(&format!("{stem}.jpg")), "stem={stem}");
            assert!(
                is_cover_name(&format!("{}.jpg", stem.to_uppercase())),
                "uppercase stem={stem}"
            );
        }
    }

    #[test]
    fn cover_wins_over_sort() {
        let names = vec![
            "aaa.jpg".to_string(),
            "cover.jpg".to_string(),
            "zzz.jpg".to_string(),
        ];
        // With default settings (cover priority on), cover wins.
        assert_eq!(
            Settings::default().pick_first_image(names),
            Some("cover.jpg".to_string())
        );
    }

    #[test]
    fn pick_first_image_empty() {
        assert_eq!(
            Settings::default().pick_first_image(Vec::<String>::new()),
            None
        );
    }

    #[test]
    fn pick_first_image_skips_macos_metadata() {
        let s = Settings::default();
        let picked = s.pick_first_image(vec![
            "__MACOSX/MyComic/._001.jpg".to_string(),
            "MyComic/._cover.jpg".to_string(),
            "MyComic/002.jpg".to_string(),
            "MyComic/001.jpg".to_string(),
        ]);
        assert_eq!(picked, Some("MyComic/001.jpg".to_string()));

        // Backslash separators, as written by some Windows tools.
        let picked = s.pick_first_image(vec!["__MACOSX\\._a.png".to_string(), "b.png".to_string()]);
        assert_eq!(picked, Some("b.png".to_string()));

        // Nothing but metadata means no image at all.
        assert_eq!(
            s.pick_first_image(vec!["__MACOSX/._001.jpg".to_string()]),
            None
        );
        // A name that merely starts with an underscore is a real file.
        assert_eq!(
            s.pick_first_image(vec!["_001.jpg".to_string()]),
            Some("_001.jpg".to_string())
        );
    }

    #[test]
    fn pick_first_image_only_mode_picks_cover_or_nothing() {
        let only = Settings {
            cover_mode: CoverMode::Only,
            ..Settings::default()
        };
        // Cover present: it wins over the page scan.
        let with_cover = vec![
            "page01.jpg".to_string(),
            "cover.jpg".to_string(),
            "page02.jpg".to_string(),
        ];
        assert_eq!(
            only.pick_first_image(with_cover),
            Some("cover.jpg".to_string())
        );
        // No cover: None, which the backends turn into a "no image"
        // error so Explorer shows the plain archive icon.
        let no_cover = vec!["page01.jpg".to_string(), "page02.jpg".to_string()];
        assert_eq!(only.pick_first_image(no_cover), None);
    }

    #[test]
    fn pick_first_image_only_mode_honours_all_aliases() {
        let only = Settings {
            cover_mode: CoverMode::Only,
            ..Settings::default()
        };
        for stem in &["cover", "folder", "thumb", "thumbnail", "front"] {
            let names = vec!["page01.jpg".to_string(), format!("{stem}.jpg")];
            assert_eq!(
                only.pick_first_image(names),
                Some(format!("{stem}.jpg")),
                "only mode should accept the {stem} alias"
            );
        }
    }

    #[test]
    fn pick_first_image_ignore_mode_skips_cover() {
        // `aaa` sorts before the `thumbnail` cover stem, so a mode that
        // honoured cover names would still return the cover. Ignore must
        // give it no special treatment and return the sort-order first.
        let names = vec!["thumbnail.jpg".to_string(), "aaa.jpg".to_string()];
        let ignore = Settings {
            cover_mode: CoverMode::Ignore,
            ..Settings::default()
        };
        assert_eq!(
            ignore.pick_first_image(names.clone()),
            Some("aaa.jpg".to_string())
        );
        // Sanity: the default Prefer mode would pick the cover instead,
        // proving the difference is the mode and not the sort order.
        let prefer = Settings::default();
        assert_eq!(
            prefer.pick_first_image(names),
            Some("thumbnail.jpg".to_string())
        );
    }

    #[test]
    fn cover_mode_parse_round_trip() {
        for mode in [CoverMode::Ignore, CoverMode::Prefer, CoverMode::Only] {
            let s = mode.as_registry_value();
            assert_eq!(CoverMode::from_registry_value(s), Some(mode));
        }
        // Case-insensitive parse; unknown strings fall through.
        assert_eq!(
            CoverMode::from_registry_value("ONLY"),
            Some(CoverMode::Only)
        );
        assert_eq!(CoverMode::from_registry_value("garbage"), None);
    }

    #[test]
    fn cover_mode_registry_round_trip_all_values() {
        for mode in [CoverMode::Ignore, CoverMode::Prefer, CoverMode::Only] {
            let scratch = ScratchSubkey::new("covermode");
            let original = Settings {
                cover_mode: mode,
                ..Settings::default()
            };
            original.save_to_subkey(scratch.path()).unwrap();
            let loaded = Settings::load_from_subkey(scratch.path());
            assert_eq!(loaded.cover_mode, mode, "round-trip {mode:?}");
        }
    }

    #[test]
    fn legacy_prefer_cover_names_falls_back() {
        // Older builds wrote PreferCoverNames (REG_DWORD) and no
        // CoverMode key. Loading must honour it: 1 -> Prefer, 0 -> Ignore.
        for (dword, expected) in [(1u32, CoverMode::Prefer), (0u32, CoverMode::Ignore)] {
            let scratch = ScratchSubkey::new("legacycover");
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let (key, _) = hkcu.create_subkey(scratch.path()).unwrap();
            key.set_value("PreferCoverNames", &dword).unwrap();
            let loaded = Settings::load_from_subkey(scratch.path());
            assert_eq!(loaded.cover_mode, expected, "PreferCoverNames={dword}");
        }
    }

    #[test]
    fn cover_mode_takes_precedence_over_legacy_key() {
        // When both keys exist (an upgrade that re-saved), CoverMode wins.
        let scratch = ScratchSubkey::new("coverboth");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(scratch.path()).unwrap();
        key.set_value("PreferCoverNames", &0u32).unwrap(); // legacy -> Ignore
        key.set_value("CoverMode", &"only").unwrap();
        let loaded = Settings::load_from_subkey(scratch.path());
        assert_eq!(loaded.cover_mode, CoverMode::Only);
    }

    #[test]
    fn sort_order_parse_aliases() {
        assert_eq!(
            SortOrder::from_registry_value("alphabetical"),
            Some(SortOrder::Alphabetical)
        );
        assert_eq!(
            SortOrder::from_registry_value("ALPHA"),
            Some(SortOrder::Alphabetical)
        );
        assert_eq!(
            SortOrder::from_registry_value("Natural"),
            Some(SortOrder::Natural)
        );
        assert_eq!(
            SortOrder::from_registry_value("NAT"),
            Some(SortOrder::Natural)
        );
        assert_eq!(SortOrder::from_registry_value("garbage"), None);
        assert_eq!(SortOrder::from_registry_value(""), None);
    }

    #[test]
    fn sort_order_registry_value_roundtrip() {
        for order in [SortOrder::Alphabetical, SortOrder::Natural] {
            let s = order.as_registry_value();
            assert_eq!(SortOrder::from_registry_value(s), Some(order));
        }
    }

    #[test]
    fn settings_default_matches_documented_behaviour() {
        // The defaults are user-visible (they kick in when the registry
        // key is missing) so a regression here would silently change
        // every fresh install.
        let s = Settings::default();
        assert_eq!(s.sort_order, SortOrder::Natural);
        assert_eq!(s.cover_mode, CoverMode::Prefer);
        assert_eq!(
            s.enabled_image_exts_mask,
            default_enabled_image_exts_mask(),
            "default must enable every supported image extension"
        );
        // Identification overlay ships off so existing installs keep
        // their bare cover thumbnails until the user opts in.
        assert!(!s.overlay_border, "border overlay defaults off");
        assert!(!s.overlay_label, "label overlay defaults off");
    }

    /// RAII helper that picks a unique throwaway subkey under
    /// `HKCU\Software\ArcThumb_test\...` and deletes it on drop so
    /// parallel tests don't stomp on each other or leak state.
    struct ScratchSubkey(String);

    impl ScratchSubkey {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let path = format!("Software\\ArcThumb_test\\{tag}_{pid}_{n}");
            // Ensure a clean slate.
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let _ = hkcu.delete_subkey_all(&path);
            Self(path)
        }
        fn path(&self) -> &str {
            &self.0
        }
    }

    impl Drop for ScratchSubkey {
        fn drop(&mut self) {
            let hkcu = RegKey::predef(HKEY_CURRENT_USER);
            let _ = hkcu.delete_subkey_all(&self.0);
        }
    }

    #[test]
    fn settings_registry_round_trip_preserves_all_fields() {
        let scratch = ScratchSubkey::new("roundtrip");
        let original = Settings {
            sort_order: SortOrder::Alphabetical,
            cover_mode: CoverMode::Only,
            enabled_image_exts_mask: 0b1010_1010,
            overlay_border: true,
            overlay_label: true,
        };
        original
            .save_to_subkey(scratch.path())
            .expect("save to scratch subkey");
        let loaded = Settings::load_from_subkey(scratch.path());
        // The mask is ANDed with the default on save, so compare
        // against the AND-ed form.
        let expected_mask = 0b1010_1010 & default_enabled_image_exts_mask();
        assert_eq!(loaded.sort_order, SortOrder::Alphabetical);
        assert_eq!(loaded.cover_mode, CoverMode::Only);
        assert_eq!(loaded.enabled_image_exts_mask, expected_mask);
        assert!(loaded.overlay_border, "border overlay round-trips");
        assert!(loaded.overlay_label, "label overlay round-trips");
    }

    #[test]
    fn settings_load_missing_subkey_returns_defaults() {
        let scratch = ScratchSubkey::new("missing");
        // Scratch doesn't exist (never saved); loading should yield
        // defaults rather than panicking or returning junk.
        let loaded = Settings::load_from_subkey(scratch.path());
        assert_eq!(loaded, Settings::default());
    }

    #[test]
    fn settings_load_masks_out_of_range_high_bits() {
        // Simulate a future build that set bits beyond our supported
        // set. Those must be silently cleared on load so downgrades
        // don't enable phantom extensions.
        let scratch = ScratchSubkey::new("highbits");
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu.create_subkey(scratch.path()).unwrap();
        let stale: u32 = 0xFFFF_FFFF;
        key.set_value("EnabledImageExts", &stale).unwrap();

        let loaded = Settings::load_from_subkey(scratch.path());
        assert_eq!(
            loaded.enabled_image_exts_mask,
            default_enabled_image_exts_mask(),
            "high bits beyond the supported range must be cleared"
        );
    }

    #[test]
    fn settings_round_trip_every_single_image_ext_toggle() {
        // End-to-end: for every supported image extension, flip just
        // that extension's bit off, round-trip through the registry,
        // and verify the remaining bits survived intact.
        let all = default_enabled_image_exts_mask();
        for (i, ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
            let scratch = ScratchSubkey::new(&format!("bit{i}"));
            let original = Settings {
                enabled_image_exts_mask: all & !(1u32 << i),
                ..Settings::default()
            };
            original.save_to_subkey(scratch.path()).unwrap();
            let loaded = Settings::load_from_subkey(scratch.path());
            assert_eq!(
                loaded.enabled_image_exts_mask, original.enabled_image_exts_mask,
                "bit {i} round-trip ({ext})"
            );
        }
    }

    #[test]
    fn ends_with_ignore_ascii_case_matches_case_variants() {
        assert!(ends_with_ignore_ascii_case("foo.jpg", ".jpg"));
        assert!(ends_with_ignore_ascii_case("foo.JPG", ".jpg"));
        assert!(ends_with_ignore_ascii_case("foo.JpG", ".jpg"));
        assert!(ends_with_ignore_ascii_case("FOO.JPG", ".JPG"));
    }

    #[test]
    fn ends_with_ignore_ascii_case_rejects_non_suffix() {
        assert!(!ends_with_ignore_ascii_case("foo.jpg", ".png"));
        assert!(!ends_with_ignore_ascii_case("foo", ".jpg"));
        assert!(!ends_with_ignore_ascii_case("", ".jpg"));
        // Substring match must not be treated as suffix.
        assert!(!ends_with_ignore_ascii_case("foo.jpg.txt", ".jpg"));
    }

    #[test]
    fn ends_with_ignore_ascii_case_empty_suffix_always_matches() {
        assert!(ends_with_ignore_ascii_case("anything", ""));
        assert!(ends_with_ignore_ascii_case("", ""));
    }

    #[test]
    fn ends_with_ignore_ascii_case_handles_non_ascii_in_input() {
        // Non-ASCII bytes in the *input* must not crash. The supported
        // extensions are ASCII so the comparison boils down to byte-
        // equality on those trailing bytes.
        assert!(ends_with_ignore_ascii_case("日本語.jpg", ".jpg"));
        assert!(!ends_with_ignore_ascii_case("日本語.jpg", ".png"));
    }

    #[test]
    fn default_image_mask_covers_exactly_supported_length() {
        let n = SUPPORTED_IMAGE_EXTS.len();
        let expected = if n >= 32 { u32::MAX } else { (1u32 << n) - 1 };
        assert_eq!(default_enabled_image_exts_mask(), expected);
        // Every bit below `n` is set, and every bit at or above `n`
        // is cleared. The upper-bound check guards against future
        // silent overflow if the supported list grows past 32.
        for i in 0..n {
            assert!(
                default_enabled_image_exts_mask() & (1u32 << i) != 0,
                "bit {i} should be set in the default mask"
            );
        }
        for i in n..32 {
            assert_eq!(
                default_enabled_image_exts_mask() & (1u32 << i),
                0,
                "bit {i} should be clear (beyond supported length)"
            );
        }
    }
}
