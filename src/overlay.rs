//! Archive identification overlay baked into the thumbnail.
//!
//! Explorer can't tell an archive's cover image apart from a plain
//! picture at a glance — more so with extensions hidden or at
//! extra-large icon sizes. When the user opts in, we draw two cues
//! straight onto the bitmap before it becomes an `HBITMAP`:
//!
//! * a **border** coloured by format family (compressed archive /
//!   e-book / other), and
//! * a small **format label** (`CBZ`, `EPUB`, …) in the bottom-right
//!   corner.
//!
//! Both are independent toggles and both default off, so existing
//! installs keep their bare cover thumbnails until asked otherwise.
//! `IThumbnailProvider::GetThumbnail` returns one static bitmap and
//! Explorer caches it, so there is no compositing-after-the-fact: the
//! overlay has to be part of the bitmap we hand back.
//!
//! The label is modelled as a [`LabelSlot`] — today only text, but the
//! enum leaves room for a user-supplied image later without reworking
//! the renderer.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, Glyph, PxScale, ScaleFont, point};
use image::{Rgba, RgbaImage};

use crate::archive::ContentKind;
use crate::settings::Settings;

/// A–Z / 0–9 subset of Roboto Bold (Apache-2.0). See `assets/fonts/`.
static FONT_BYTES: &[u8] = include_bytes!("../assets/fonts/Roboto-Bold-subset.ttf");

/// Below this size (in the shorter dimension) the label text would be
/// an unreadable smudge, so we drop it and keep only the border. The
/// border itself stays useful even on tiny icons.
const LABEL_MIN_PX: u32 = 48;

/// Format family. Drives the border / chip colour. `Other` is a
/// catch-all that nothing maps to today but keeps the palette open
/// for future formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FormatGroup {
    Archive,
    Ebook,
    /// Reserved fallback colour. No `ContentKind` maps here today, but
    /// the palette keeps a third slot for formats that are neither a
    /// compressed archive nor an e-book (e.g. a future PDF/DjVu cover).
    #[allow(dead_code)]
    Other,
}

impl FormatGroup {
    /// Opaque RGBA used for both the border and the label chip, so the
    /// two read as one identity colour.
    fn color(self) -> Rgba<u8> {
        match self {
            // Folder-like yellow for compressed archives.
            FormatGroup::Archive => Rgba([240, 195, 70, 255]),
            // Cool indigo for e-books.
            FormatGroup::Ebook => Rgba([58, 124, 165, 255]),
            // Neutral grey fallback.
            FormatGroup::Other => Rgba([110, 110, 110, 255]),
        }
    }
}

fn group_for(kind: ContentKind) -> FormatGroup {
    match kind {
        ContentKind::Zip | ContentKind::SevenZ | ContentKind::Rar | ContentKind::Tar => {
            FormatGroup::Archive
        }
        ContentKind::Epub | ContentKind::Fb2 | ContentKind::Mobi => FormatGroup::Ebook,
    }
}

/// What goes in the corner. Text for now; an `Image(...)` variant can
/// be added later for user-supplied icons without touching callers.
enum LabelSlot {
    Text(String),
}

/// Pick the label text for an archive.
///
/// `file_ext` is the on-disk extension (lowercased, no dot) when the
/// thumbnail host gave us a name to work with. We prefer it for the
/// container formats so a `.cbz` reads "CBZ" rather than the generic
/// "ZIP". For EPUB / FB2 / MOBI the detected content wins, because
/// those are recognised from the bytes and the wrapper extension can
/// be misleading (an FB2 shipped as `.fb2.zip` should still say "FB2",
/// not "ZIP").
fn label_for(kind: ContentKind, file_ext: Option<&str>) -> LabelSlot {
    let text = match kind {
        ContentKind::Epub => "EPUB".to_string(),
        ContentKind::Fb2 => "FB2".to_string(),
        ContentKind::Mobi => match file_ext {
            Some("azw") => "AZW".to_string(),
            Some("azw3") => "AZW3".to_string(),
            _ => "MOBI".to_string(),
        },
        ContentKind::Zip => ext_label(file_ext, &["zip", "cbz"], "ZIP"),
        ContentKind::SevenZ => ext_label(file_ext, &["7z", "cb7"], "7Z"),
        ContentKind::Rar => ext_label(file_ext, &["rar", "cbr"], "RAR"),
        ContentKind::Tar => ext_label(file_ext, &["tar", "cbt"], "TAR"),
    };
    LabelSlot::Text(text)
}

/// Uppercase `file_ext` when it's one this container is expected to
/// wear; otherwise fall back to `default`. Guards against a renamed or
/// missing extension producing a nonsense label.
fn ext_label(file_ext: Option<&str>, allowed: &[&str], default: &str) -> String {
    match file_ext {
        Some(ext) if allowed.contains(&ext) => ext.to_ascii_uppercase(),
        _ => default.to_string(),
    }
}

/// The registered extension whose per-extension overlay toggle governs
/// this thumbnail.
///
/// Follows the same rule as [`label_for`], so the checkbox that turns
/// an overlay off is the one named after the label it would have
/// shown. When the host gave us no file name, the container's generic
/// extension stands in: a nameless ZIP answers to `.zip`, a nameless
/// MOBI-family book to `.mobi`. TAR has only `.cbt` registered.
fn toggle_ext(kind: ContentKind, file_ext: Option<&str>) -> &'static str {
    match kind {
        ContentKind::Epub => ".epub",
        ContentKind::Fb2 => ".fb2",
        ContentKind::Mobi => match file_ext {
            Some("azw") => ".azw",
            Some("azw3") => ".azw3",
            _ => ".mobi",
        },
        ContentKind::Zip => match file_ext {
            Some("cbz") => ".cbz",
            _ => ".zip",
        },
        ContentKind::SevenZ => match file_ext {
            Some("cb7") => ".cb7",
            _ => ".7z",
        },
        ContentKind::Rar => match file_ext {
            Some("cbr") => ".cbr",
            _ => ".rar",
        },
        ContentKind::Tar => ".cbt",
    }
}

/// Bake the enabled overlay cues into `img` in place.
///
/// A no-op (and free) when both toggles are off, which is the default,
/// or when the overlay is switched off for this file's extension.
pub fn apply_overlay(
    img: &mut RgbaImage,
    kind: ContentKind,
    file_ext: Option<&str>,
    settings: &Settings,
) {
    if !settings.overlay_border && !settings.overlay_label {
        return;
    }
    if !settings.overlay_allowed_for(toggle_ext(kind, file_ext)) {
        return;
    }

    let group = group_for(kind);
    let (w, h) = img.dimensions();
    let min_dim = w.min(h);
    if min_dim == 0 {
        return;
    }

    if settings.overlay_border {
        draw_border(img, group.color(), border_thickness(min_dim));
    }

    // Drop the label on icons too small for it to be legible; the
    // border alone still flags the file as an archive.
    if settings.overlay_label && min_dim >= LABEL_MIN_PX {
        match label_for(kind, file_ext) {
            LabelSlot::Text(text) => draw_label_text(img, &text, group.color()),
        }
    }
}

/// Border width: ~4% of the shorter side, at least 2 px.
fn border_thickness(min_dim: u32) -> u32 {
    ((min_dim as f32 * 0.04).round() as u32).max(2)
}

/// Paint an opaque frame of width `t` around the image edges.
fn draw_border(img: &mut RgbaImage, color: Rgba<u8>, t: u32) {
    let (w, h) = img.dimensions();
    // A frame thicker than half the image would cover everything; clamp.
    let t = t.min(w.div_ceil(2)).min(h.div_ceil(2));
    for y in 0..h {
        let on_horizontal_band = y < t || y >= h - t;
        for x in 0..w {
            if on_horizontal_band || x < t || x >= w - t {
                img.put_pixel(x, y, color);
            }
        }
    }
}

/// Black or white, whichever reads better on `bg`.
fn pick_text_color(bg: Rgba<u8>) -> Rgba<u8> {
    let [r, g, b, _] = bg.0;
    // Perceived luminance (ITU-R BT.601 weights).
    let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    if lum > 140.0 {
        Rgba([0, 0, 0, 255])
    } else {
        Rgba([255, 255, 255, 255])
    }
}

fn font() -> &'static FontRef<'static> {
    static FONT: OnceLock<FontRef<'static>> = OnceLock::new();
    FONT.get_or_init(|| {
        FontRef::try_from_slice(FONT_BYTES).expect("embedded Roboto subset must be valid")
    })
}

/// Draw `text` in a coloured chip in the bottom-right corner.
fn draw_label_text(img: &mut RgbaImage, text: &str, chip: Rgba<u8>) {
    let (w, h) = img.dimensions();
    let min_dim = w.min(h);

    let font = font();
    let font_px = (min_dim as f32 * 0.18).max(11.0);
    let scale = PxScale::from(font_px);
    let scaled = font.as_scaled(scale);

    // Lay the glyphs out left-to-right on a baseline at `ascent`.
    let mut glyphs: Vec<Glyph> = Vec::with_capacity(text.len());
    let mut caret = 0.0f32;
    let mut prev = None;
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        if let Some(p) = prev {
            caret += scaled.kern(p, id);
        }
        glyphs.push(id.with_scale_and_position(scale, point(caret, scaled.ascent())));
        caret += scaled.h_advance(id);
        prev = Some(id);
    }
    let text_w = caret.ceil().max(1.0);
    let text_h = (scaled.ascent() - scaled.descent()).ceil().max(1.0);

    // Chip = text box + padding.
    let pad_x = (font_px * 0.35).round();
    let pad_y = (font_px * 0.20).round();
    let chip_w = (text_w + 2.0 * pad_x).round() as u32;
    let chip_h = (text_h + 2.0 * pad_y).round() as u32;
    if chip_w >= w || chip_h >= h {
        return; // doesn't fit — leave the thumbnail alone.
    }

    // Bottom-right, tucked just inside the border.
    let margin = border_thickness(min_dim) + (font_px * 0.15).round() as u32;
    let chip_x = w.saturating_sub(chip_w).saturating_sub(margin);
    let chip_y = h.saturating_sub(chip_h).saturating_sub(margin);

    // The text and the chip's outline share one colour, picked for
    // contrast against the fill. The outline keeps the chip readable
    // even when the cover behind it is the same hue as the fill.
    let text_color = pick_text_color(chip);

    // Rounded chip, anti-aliased at the corners via a rounded-rectangle
    // signed-distance field. We paint the outline colour across the
    // whole shape, then the fill over an inset copy of it, leaving a
    // stroke of width `outline_w` around the edge.
    let fw = chip_w as f32;
    let fh = chip_h as f32;
    let radius = (fh * 0.28).min(fw / 2.0).min(fh / 2.0);
    let outline_w = (font_px * 0.09).max(1.5);
    for y in chip_y..(chip_y + chip_h).min(h) {
        for x in chip_x..(chip_x + chip_w).min(w) {
            let lx = (x - chip_x) as f32 + 0.5;
            let ly = (y - chip_y) as f32 + 0.5;
            let d = rounded_rect_sdf(lx, ly, fw, fh, radius);
            let shape_cov = (0.5 - d).clamp(0.0, 1.0);
            if shape_cov <= 0.0 {
                continue;
            }
            // Inset shape: 1 inside the fill region, 0 in the stroke band.
            let fill_cov = (0.5 - (d + outline_w)).clamp(0.0, 1.0);
            let dst = img.get_pixel_mut(x, y);
            let outlined = blend(*dst, text_color, shape_cov);
            *dst = blend(outlined, chip, fill_cov);
        }
    }

    // Glyphs, blended over the chip with their coverage.
    let origin_x = chip_x as f32 + pad_x;
    let origin_y = chip_y as f32 + pad_y;
    for glyph in glyphs {
        let Some(outline) = font.outline_glyph(glyph) else {
            continue; // whitespace / no outline
        };
        let bounds = outline.px_bounds();
        outline.draw(|gx, gy, coverage| {
            let px = (origin_x + bounds.min.x + gx as f32).round();
            let py = (origin_y + bounds.min.y + gy as f32).round();
            if px < 0.0 || py < 0.0 {
                return;
            }
            let (px, py) = (px as u32, py as u32);
            if px < w && py < h {
                let dst = img.get_pixel_mut(px, py);
                *dst = blend(*dst, text_color, coverage);
            }
        });
    }
}

/// Signed distance from the pixel at local `(lx, ly)` to the edge of a
/// rounded rectangle of size `w` × `h` with corner `radius`, all
/// measured from the rectangle's top-left. Negative inside the shape.
/// The standard rounded-box SDF.
fn rounded_rect_sdf(lx: f32, ly: f32, w: f32, h: f32, radius: f32) -> f32 {
    let (hw, hh) = (w / 2.0, h / 2.0);
    // Offset from the centre of the inner box the corner arcs ride on.
    let qx = (lx - hw).abs() - (hw - radius);
    let qy = (ly - hh).abs() - (hh - radius);
    let outside = qx.max(0.0).hypot(qy.max(0.0));
    let inside = qx.max(qy).min(0.0);
    outside + inside - radius
}

/// Alpha-blend `src` over `dst` by `coverage` (0..=1). The result stays
/// fully opaque — both inputs are, and the thumbnail is too.
fn blend(dst: Rgba<u8>, src: Rgba<u8>, coverage: f32) -> Rgba<u8> {
    let c = coverage.clamp(0.0, 1.0);
    let mix = |d: u8, s: u8| (d as f32 * (1.0 - c) + s as f32 * c).round() as u8;
    Rgba([
        mix(dst.0[0], src.0[0]),
        mix(dst.0[1], src.0[1]),
        mix(dst.0[2], src.0[2]),
        255,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, color: Rgba<u8>) -> RgbaImage {
        RgbaImage::from_pixel(w, h, color)
    }

    fn off() -> Settings {
        Settings {
            overlay_border: false,
            overlay_label: false,
            ..Settings::default()
        }
    }

    // ----- group mapping -------------------------------------------------

    #[test]
    fn group_mapping_splits_archives_and_ebooks() {
        for k in [
            ContentKind::Zip,
            ContentKind::SevenZ,
            ContentKind::Rar,
            ContentKind::Tar,
        ] {
            assert_eq!(group_for(k), FormatGroup::Archive, "{k:?}");
        }
        for k in [ContentKind::Epub, ContentKind::Fb2, ContentKind::Mobi] {
            assert_eq!(group_for(k), FormatGroup::Ebook, "{k:?}");
        }
    }

    // ----- label resolution ----------------------------------------------

    fn text_of(slot: LabelSlot) -> String {
        match slot {
            LabelSlot::Text(t) => t,
        }
    }

    #[test]
    fn label_prefers_real_extension_for_containers() {
        assert_eq!(text_of(label_for(ContentKind::Zip, Some("cbz"))), "CBZ");
        assert_eq!(text_of(label_for(ContentKind::Zip, Some("zip"))), "ZIP");
        assert_eq!(text_of(label_for(ContentKind::Rar, Some("cbr"))), "CBR");
        assert_eq!(text_of(label_for(ContentKind::SevenZ, Some("cb7"))), "CB7");
        assert_eq!(text_of(label_for(ContentKind::Tar, Some("cbt"))), "CBT");
    }

    #[test]
    fn label_falls_back_to_generic_without_a_usable_extension() {
        assert_eq!(text_of(label_for(ContentKind::Zip, None)), "ZIP");
        assert_eq!(text_of(label_for(ContentKind::SevenZ, None)), "7Z");
        assert_eq!(text_of(label_for(ContentKind::Rar, None)), "RAR");
        assert_eq!(text_of(label_for(ContentKind::Tar, None)), "TAR");
        // A renamed/odd extension shouldn't leak into the label.
        assert_eq!(text_of(label_for(ContentKind::Zip, Some("bin"))), "ZIP");
    }

    #[test]
    fn label_content_wins_for_ebooks() {
        // .fb2.zip detected as FB2 must say FB2 even though its on-disk
        // extension is ".zip".
        assert_eq!(text_of(label_for(ContentKind::Fb2, Some("zip"))), "FB2");
        assert_eq!(text_of(label_for(ContentKind::Epub, Some("epub"))), "EPUB");
        assert_eq!(text_of(label_for(ContentKind::Mobi, None)), "MOBI");
        assert_eq!(text_of(label_for(ContentKind::Mobi, Some("azw3"))), "AZW3");
        assert_eq!(text_of(label_for(ContentKind::Mobi, Some("azw"))), "AZW");
    }

    // ----- text colour contrast ------------------------------------------

    #[test]
    fn text_color_contrasts_with_background() {
        assert_eq!(
            pick_text_color(Rgba([0, 0, 0, 255])),
            Rgba([255, 255, 255, 255])
        );
        assert_eq!(
            pick_text_color(Rgba([255, 255, 255, 255])),
            Rgba([0, 0, 0, 255])
        );
        // The e-book indigo is dark enough to want white text.
        assert_eq!(
            pick_text_color(FormatGroup::Ebook.color()),
            Rgba([255, 255, 255, 255])
        );
        // The folder yellow is bright, so text (and the matching chip
        // outline) come out black.
        assert_eq!(
            pick_text_color(FormatGroup::Archive.color()),
            Rgba([0, 0, 0, 255])
        );
    }

    // ----- rounded chip -------------------------------------------------

    #[test]
    fn rounded_rect_carves_corners_but_keeps_center_solid() {
        // Fill coverage the chip draw derives from the SDF.
        let cov = |lx, ly, w, h, r| (0.5 - rounded_rect_sdf(lx, ly, w, h, r)).clamp(0.0, 1.0);
        let (w, h, r) = (40.0, 24.0, 8.0);
        // Centre is fully inside.
        assert_eq!(cov(20.0, 12.0, w, h, r), 1.0);
        // The extreme top-left pixel sits beyond the corner arc.
        assert!(cov(0.5, 0.5, w, h, r) < 1.0);
        // A mid-edge pixel (not a corner) stays solid.
        assert_eq!(cov(0.5, 12.0, w, h, r), 1.0);
        // radius 0 degrades to a plain rectangle: the corner is solid.
        assert_eq!(cov(0.5, 0.5, w, h, 0.0), 1.0);
    }

    // ----- apply_overlay behaviour ---------------------------------------

    #[test]
    fn both_toggles_off_is_a_no_op() {
        let original = solid(128, 128, Rgba([10, 20, 30, 255]));
        let mut img = original.clone();
        apply_overlay(&mut img, ContentKind::Zip, Some("cbz"), &off());
        assert_eq!(img, original, "overlay off must not touch the bitmap");
    }

    #[test]
    fn border_paints_the_corner_with_the_group_color() {
        let mut img = solid(128, 128, Rgba([10, 20, 30, 255]));
        let settings = Settings {
            overlay_border: true,
            ..off()
        };
        apply_overlay(&mut img, ContentKind::Zip, None, &settings);
        // ContentKind::Zip → Archive → orange.
        assert_eq!(*img.get_pixel(0, 0), FormatGroup::Archive.color());
        assert_eq!(*img.get_pixel(127, 127), FormatGroup::Archive.color());
        // Centre is untouched.
        assert_eq!(*img.get_pixel(64, 64), Rgba([10, 20, 30, 255]));
    }

    #[test]
    fn label_is_dropped_below_the_size_threshold() {
        // 32 px < LABEL_MIN_PX, border off → nothing should change.
        let original = solid(32, 32, Rgba([10, 20, 30, 255]));
        let mut img = original.clone();
        let settings = Settings {
            overlay_label: true,
            ..off()
        };
        apply_overlay(&mut img, ContentKind::Zip, Some("cbz"), &settings);
        assert_eq!(img, original, "label must be skipped on tiny icons");
    }

    #[test]
    fn label_is_drawn_above_the_threshold() {
        let original = solid(256, 256, Rgba([10, 20, 30, 255]));
        let mut img = original.clone();
        let settings = Settings {
            overlay_label: true,
            ..off()
        };
        apply_overlay(&mut img, ContentKind::Zip, Some("cbz"), &settings);
        assert_ne!(img, original, "a chip + label should have been drawn");
        // The chip lands in the bottom-right quadrant.
        let mut changed = false;
        for y in 128..256 {
            for x in 128..256 {
                if img.get_pixel(x, y) != original.get_pixel(x, y) {
                    changed = true;
                }
            }
        }
        assert!(changed, "expected changes in the bottom-right quadrant");
    }

    // ----- per-extension toggle -------------------------------------------

    /// Both cues on, with the overlay switched off for `exts`.
    fn on_except(exts: &[&str]) -> Settings {
        let mask = exts
            .iter()
            .fold(crate::settings::default_overlay_exts_mask(), |mask, ext| {
                let i = crate::registry::EXTENSIONS
                    .iter()
                    .position(|e| e == ext)
                    .expect("registered ext");
                mask & !(1u32 << i)
            });
        Settings {
            overlay_border: true,
            overlay_label: true,
            overlay_exts_mask: mask,
            ..Settings::default()
        }
    }

    #[test]
    fn toggle_ext_is_always_a_registered_extension() {
        let kinds = [
            ContentKind::Zip,
            ContentKind::SevenZ,
            ContentKind::Rar,
            ContentKind::Tar,
            ContentKind::Epub,
            ContentKind::Fb2,
            ContentKind::Mobi,
        ];
        let exts = [
            None,
            Some("zip"),
            Some("cbz"),
            Some("rar"),
            Some("cbr"),
            Some("7z"),
            Some("cb7"),
            Some("cbt"),
            Some("epub"),
            Some("fb2"),
            Some("mobi"),
            Some("azw"),
            Some("azw3"),
            Some("bin"),
        ];
        for kind in kinds {
            for ext in exts {
                let toggle = toggle_ext(kind, ext);
                assert!(
                    crate::registry::EXTENSIONS.contains(&toggle),
                    "{kind:?}/{ext:?} resolved to {toggle}, which has no checkbox"
                );
            }
        }
    }

    #[test]
    fn toggle_ext_matches_the_label() {
        // Whatever the label says, the same-named checkbox governs it.
        // TAR is the exception: its label reads "TAR" without a name
        // but only `.cbt` is registered.
        let cases: &[(ContentKind, Option<&str>)] = &[
            (ContentKind::Zip, Some("cbz")),
            (ContentKind::Zip, Some("zip")),
            (ContentKind::Zip, None),
            (ContentKind::Zip, Some("bin")),
            (ContentKind::SevenZ, Some("cb7")),
            (ContentKind::SevenZ, None),
            (ContentKind::Rar, Some("cbr")),
            (ContentKind::Rar, None),
            (ContentKind::Tar, Some("cbt")),
            (ContentKind::Epub, Some("epub")),
            (ContentKind::Fb2, Some("zip")),
            (ContentKind::Mobi, None),
            (ContentKind::Mobi, Some("mobi")),
            (ContentKind::Mobi, Some("azw")),
            (ContentKind::Mobi, Some("azw3")),
        ];
        for &(kind, ext) in cases {
            let label = text_of(label_for(kind, ext));
            assert_eq!(
                toggle_ext(kind, ext),
                format!(".{}", label.to_ascii_lowercase()),
                "{kind:?}/{ext:?}"
            );
        }
        assert_eq!(toggle_ext(ContentKind::Tar, None), ".cbt");
    }

    #[test]
    fn disabled_extension_gets_no_overlay() {
        let original = solid(256, 256, Rgba([10, 20, 30, 255]));
        let settings = on_except(&[".mobi", ".azw"]);

        for ext in [Some("mobi"), Some("azw"), None] {
            let mut img = original.clone();
            apply_overlay(&mut img, ContentKind::Mobi, ext, &settings);
            assert_eq!(img, original, "{ext:?} must stay bare");
        }

        // The neighbours keep theirs: `.azw3` has its own checkbox, and
        // other formats are untouched.
        let mut img = original.clone();
        apply_overlay(&mut img, ContentKind::Mobi, Some("azw3"), &settings);
        assert_ne!(img, original, ".azw3 is still marked");
        let mut img = original.clone();
        apply_overlay(&mut img, ContentKind::Zip, Some("cbz"), &settings);
        assert_ne!(img, original, ".cbz is still marked");
    }

    #[test]
    fn disabled_extension_does_not_leak_across_a_shared_container() {
        // `.zip` and `.cbz` are the same container but separate toggles.
        let original = solid(256, 256, Rgba([10, 20, 30, 255]));
        let settings = on_except(&[".cbz"]);

        let mut img = original.clone();
        apply_overlay(&mut img, ContentKind::Zip, Some("cbz"), &settings);
        assert_eq!(img, original, ".cbz must stay bare");

        for ext in [Some("zip"), None] {
            let mut img = original.clone();
            apply_overlay(&mut img, ContentKind::Zip, ext, &settings);
            assert_ne!(img, original, "{ext:?} is still marked");
        }
    }

    /// Render a montage of every group/label on a few backgrounds and
    /// write it to `target/overlay-samples.png` for eyeballing the
    /// palette. Ignored by default; run on demand with:
    /// `cargo test --lib render_overlay_samples -- --ignored --nocapture`
    #[test]
    #[ignore = "writes a sample PNG for reviewing overlay colours by eye"]
    fn render_overlay_samples() {
        let cell: u32 = 220;
        let pad: u32 = 10;
        let samples: &[(ContentKind, Option<&str>)] = &[
            (ContentKind::Zip, Some("cbz")),
            (ContentKind::Zip, Some("zip")),
            (ContentKind::Rar, Some("cbr")),
            (ContentKind::Epub, Some("epub")),
            (ContentKind::Mobi, None),
        ];
        // Last background deliberately matches the archive fill hue, to
        // check the outline keeps the chip legible when they collide.
        let backgrounds: &[Rgba<u8>] = &[
            Rgba([90, 90, 90, 255]),
            Rgba([235, 235, 235, 255]),
            Rgba([60, 120, 160, 255]),
            Rgba([238, 198, 82, 255]),
        ];
        let settings = Settings {
            overlay_border: true,
            overlay_label: true,
            ..Settings::default()
        };

        let cols = backgrounds.len() as u32;
        let rows = samples.len() as u32;
        let mut montage = RgbaImage::from_pixel(cols * cell, rows * cell, Rgba([24, 24, 24, 255]));
        for (r, (kind, ext)) in samples.iter().enumerate() {
            for (c, bg) in backgrounds.iter().enumerate() {
                let mut tile = RgbaImage::from_pixel(cell - 2 * pad, cell - 2 * pad, *bg);
                apply_overlay(&mut tile, *kind, *ext, &settings);
                let (ox, oy) = (c as u32 * cell + pad, r as u32 * cell + pad);
                for (x, y, px) in tile.enumerate_pixels() {
                    montage.put_pixel(ox + x, oy + y, *px);
                }
            }
        }

        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("overlay-samples.png");
        montage.save(&out).expect("save montage");
        println!("overlay samples written to {}", out.display());
    }
}
