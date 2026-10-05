//! Archive reading: dispatch by detected magic bytes to a format-specific
//! backend, return the first image file as `(name, bytes)`.
//!
//! Supported formats:
//! - **ZIP** (`PK\x03\x04`) — via `zip` crate, direct Read+Seek
//! - **7z**  (`7z\xBC\xAF\x27\x1C`) — via `sevenz-rust2`, direct Read+Seek
//! - **RAR** (`Rar!\x1A\x07\x00` / `Rar!\x1A\x07\x01\x00`) — via `unrar`,
//!   which insists on a file path, so we spool the stream to `%TEMP%`.
//! - **TAR/CBT** (`ustar` at offset 257) — via `tar` crate, Read only
//!   (we use Seek to rewind between listing and extraction passes)
//!
//! "First image" is defined as the alphabetically smallest file whose
//! extension is in `settings::SUPPORTED_IMAGE_EXTS` AND whose bit is
//! set in the user's `enabled_image_exts_mask`.

mod detect;
mod fb2;
mod mobi;
mod rar;
mod sevenz;
mod tar;
mod zip;

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::limits;
#[cfg(test)]
use crate::settings::SUPPORTED_IMAGE_EXTS;
use crate::settings::Settings;

use detect::{Format, detect_format};

/// What kind of archive the cover image was pulled from. Derived from
/// the detected magic bytes, refined by content inspection for ZIP
/// containers (a plain ZIP, an EPUB, and an FB2-in-ZIP all share the
/// `PK` signature but are told apart by their contents).
///
/// Drives the identification overlay: the colour family of the border
/// and the fallback label text. It deliberately does *not* try to
/// recover the on-disk extension (`.cbz` vs `.zip`) — the thumbnail
/// provider only sees a stream, so that distinction is the overlay
/// renderer's job using the file name when it can get one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Zip,
    SevenZ,
    Rar,
    Tar,
    Epub,
    Fb2,
    Mobi,
}

/// The first image found in an archive, plus what kind of archive it
/// came from. Returned by [`read_first_image_with_kind`].
pub struct Extracted {
    pub name: String,
    pub bytes: Vec<u8>,
    pub kind: ContentKind,
}

/// Open an archive stream, pick the first image, return `(name, bytes)`.
///
/// Thin wrapper over [`read_first_image_with_kind`] for callers (the
/// preview handler, most tests) that don't need the archive kind.
///
/// The caller supplies the [`Settings`] snapshot that governs
/// image-extension filtering and sort order. This keeps the
/// archive module free of global state — the shell extension
/// obtains settings via [`settings::current()`] and passes them in.
pub fn read_first_image<R: Read + Seek>(
    reader: R,
    settings: &Settings,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    let extracted = read_first_image_with_kind(reader, settings)?;
    Ok((extracted.name, extracted.bytes))
}

/// Like [`read_first_image`], but also reports the [`ContentKind`] so
/// the thumbnail pipeline can draw a format-aware identification
/// overlay. Only the ZIP backend inspects contents to distinguish
/// EPUB / FB2 / plain ZIP; every other format maps 1:1 from its magic.
pub fn read_first_image_with_kind<R: Read + Seek>(
    mut reader: R,
    settings: &Settings,
) -> Result<Extracted, Box<dyn Error>> {
    // Detect format first so the size cap can be format-aware: ZIP and 7z
    // are random-access via their footer index, so file size is a poor
    // proxy for memory cost. TAR/RAR/FB2/MOBI all need the whole stream
    // either parsed sequentially or buffered, so they keep the tighter
    // 2 GiB cap.
    reader.seek(SeekFrom::Start(0))?;
    let mut magic: Vec<u8> = Vec::with_capacity(512);
    reader.by_ref().take(512).read_to_end(&mut magic)?;
    let format = detect_format(&magic);

    let total = reader.seek(SeekFrom::End(0))?;
    let max_size = max_archive_size_for(format);
    if total > max_size {
        return Err(format!("archive too large ({total} bytes > {max_size} limit)").into());
    }
    reader.seek(SeekFrom::Start(0))?;

    let (name, bytes, kind) = match format {
        Format::Zip => zip::zip_read_first_image(reader, settings)?,
        Format::SevenZ => with_kind(
            sevenz::sevenz_read_first_image(reader, settings)?,
            ContentKind::SevenZ,
        ),
        Format::Rar => with_kind(
            rar::rar_read_first_image(reader, settings)?,
            ContentKind::Rar,
        ),
        Format::Tar => with_kind(
            tar::tar_read_first_image(reader, settings)?,
            ContentKind::Tar,
        ),
        Format::Fb2 => with_kind(fb2::fb2_read_first_image(reader)?, ContentKind::Fb2),
        Format::Mobi => with_kind(mobi::mobi_read_first_image(reader)?, ContentKind::Mobi),
        Format::Unknown => return Err("unrecognised archive format".into()),
    };
    Ok(Extracted { name, bytes, kind })
}

/// Size cap that applies to a given detected format. Random-access
/// containers (ZIP, 7z) get the loose `MAX_ARCHIVE_SIZE_INDEXED`
/// because their footer-index design means total file size doesn't
/// drive our memory use; everything else gets `MAX_ARCHIVE_SIZE_SEQUENTIAL`.
fn max_archive_size_for(format: Format) -> u64 {
    match format {
        Format::Zip | Format::SevenZ => limits::MAX_ARCHIVE_SIZE_INDEXED,
        Format::Tar | Format::Rar | Format::Fb2 | Format::Mobi | Format::Unknown => {
            limits::MAX_ARCHIVE_SIZE_SEQUENTIAL
        }
    }
}

/// Tack a fixed [`ContentKind`] onto a backend's `(name, bytes)` pair.
/// Used for the formats whose magic maps unambiguously to one kind.
fn with_kind(pair: (String, Vec<u8>), kind: ContentKind) -> (String, Vec<u8>, ContentKind) {
    (pair.0, pair.1, kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ---------------------------------------------------------------
    // detect_format (shared / unknown cases)
    // ---------------------------------------------------------------

    #[test]
    fn detect_unknown_for_random_bytes() {
        assert_eq!(detect_format(b"this is not an archive"), Format::Unknown);
    }

    #[test]
    fn detect_unknown_for_short_input() {
        assert_eq!(detect_format(b""), Format::Unknown);
        assert_eq!(detect_format(b"PK"), Format::Unknown);
    }

    // ---------------------------------------------------------------
    // Settings::accepts_image_ext
    // ---------------------------------------------------------------

    fn settings_with_mask(mask: u32) -> Settings {
        Settings {
            enabled_image_exts_mask: mask,
            ..Settings::default()
        }
    }

    #[test]
    fn image_ext_recognised_lowercase() {
        let s = Settings::default();
        for ext in &[
            "jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "ico",
        ] {
            assert!(s.accepts_image_ext(&format!("foo.{ext}")), "ext={ext}");
        }
    }

    #[test]
    fn image_ext_case_insensitive() {
        let s = Settings::default();
        assert!(s.accepts_image_ext("foo.JPG"));
        assert!(s.accepts_image_ext("foo.PnG"));
        assert!(s.accepts_image_ext("comic/CHAPTER1/01.WEBP"));
    }

    #[test]
    fn image_ext_rejects_non_images() {
        let s = Settings::default();
        assert!(!s.accepts_image_ext("foo.txt"));
        assert!(!s.accepts_image_ext("foo.zip"));
        assert!(!s.accepts_image_ext("README"));
        assert!(!s.accepts_image_ext(""));
    }

    #[test]
    fn image_ext_does_not_match_substring() {
        let s = Settings::default();
        assert!(!s.accepts_image_ext("foopng"));
        assert!(!s.accepts_image_ext("imagejpg"));
    }

    #[test]
    fn mask_disables_specific_extensions() {
        assert!(settings_with_mask(0b1).accepts_image_ext("a.jpg"));
        assert!(!settings_with_mask(0b1).accepts_image_ext("a.png"));
        assert!(!settings_with_mask(0).accepts_image_ext("a.jpg"));
        let png_idx = SUPPORTED_IMAGE_EXTS
            .iter()
            .position(|&e| e == ".png")
            .unwrap();
        let s = settings_with_mask(1u32 << png_idx);
        assert!(s.accepts_image_ext("a.png"));
        assert!(!s.accepts_image_ext("a.jpg"));
    }

    #[test]
    fn every_supported_extension_can_be_solo_enabled() {
        for (i, target_ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
            let s = settings_with_mask(1u32 << i);
            let target_name = format!("foo{target_ext}");
            assert!(
                s.accepts_image_ext(&target_name),
                "{target_ext} should be recognised when its own bit (index {i}) is set"
            );
            for (j, other_ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
                if i == j {
                    continue;
                }
                if other_ext.ends_with(target_ext) || target_ext.ends_with(other_ext) {
                    continue;
                }
                let other_name = format!("bar{other_ext}");
                assert!(
                    !s.accepts_image_ext(&other_name),
                    "{other_ext} must NOT match when only {target_ext} (bit {i}) is set"
                );
            }
        }
    }

    #[test]
    fn every_supported_extension_can_be_solo_disabled() {
        let all = crate::settings::default_enabled_image_exts_mask();
        for (i, target_ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
            let mask = all & !(1u32 << i);
            let s = settings_with_mask(mask);
            let target_name = format!("foo{target_ext}");
            // Skip asymmetric suffix overlaps: disabling `.tif`
            // (index 6) doesn't reject `.tiff` because `.tiff` also
            // ends with `.tif`'s longer cousin — but in our slice
            // `.tiff` comes before `.tif`, so a plain `.tif` file
            // can still match the `.tiff` bit. Assert only when no
            // other bit could "catch" this extension via ends_with.
            let another_matches = SUPPORTED_IMAGE_EXTS
                .iter()
                .enumerate()
                .any(|(j, e)| j != i && (mask & (1u32 << j)) != 0 && target_ext.ends_with(e));
            if another_matches {
                continue;
            }
            assert!(
                !s.accepts_image_ext(&target_name),
                "{target_ext} should be rejected when only its bit (index {i}) is cleared"
            );
        }
        // Sanity: default mask accepts every supported extension.
        let default = Settings::default();
        for ext in SUPPORTED_IMAGE_EXTS {
            let name = format!("foo{ext}");
            assert!(
                default.accepts_image_ext(&name),
                "{ext} should match under the default (all-on) mask"
            );
        }
    }

    #[test]
    fn mask_matches_are_case_insensitive() {
        let s = Settings::default();
        for ext in SUPPORTED_IMAGE_EXTS {
            let upper = format!("FOO{}", ext.to_uppercase());
            assert!(
                s.accepts_image_ext(&upper),
                "uppercase {ext} should still match"
            );
        }
    }

    #[test]
    fn unknown_format_errors_cleanly() {
        let bytes = b"this is plain text, definitely not an archive".to_vec();
        let result = read_first_image(Cursor::new(bytes), &Settings::default());
        assert!(result.is_err());
    }

    // ---------------------------------------------------------------
    // max_archive_size_for: format-aware size cap dispatch.
    // ---------------------------------------------------------------

    #[test]
    fn indexed_formats_get_the_loose_cap() {
        assert_eq!(
            max_archive_size_for(Format::Zip),
            limits::MAX_ARCHIVE_SIZE_INDEXED
        );
        assert_eq!(
            max_archive_size_for(Format::SevenZ),
            limits::MAX_ARCHIVE_SIZE_INDEXED
        );
    }

    #[test]
    fn sequential_formats_get_the_tight_cap() {
        for f in [
            Format::Tar,
            Format::Rar,
            Format::Fb2,
            Format::Mobi,
            Format::Unknown,
        ] {
            assert_eq!(
                max_archive_size_for(f),
                limits::MAX_ARCHIVE_SIZE_SEQUENTIAL,
                "format {f:?} should use the sequential cap"
            );
        }
    }

    // ---------------------------------------------------------------
    // Shared test helpers (used by sub-module tests)
    // ---------------------------------------------------------------

    /// Build a tiny PNG via the `image` crate so the fixtures
    /// contain plausible image bytes.
    pub(crate) fn make_tiny_png() -> Vec<u8> {
        use image::{DynamicImage, ImageBuffer, ImageFormat, Rgba};
        let img: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_fn(2, 2, |_, _| Rgba([0, 128, 255, 255]));
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    /// Build a minimal valid FB2 document containing a single
    /// base64-encoded image binary referenced by the coverpage.
    pub(crate) fn build_fb2(cover_id: &str, png_bytes: &[u8]) -> Vec<u8> {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD as B64;
        let b64 = B64.encode(png_bytes);
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<FictionBook xmlns=\"http://www.gribuser.ru/xml/fictionbook/2.0\" \
xmlns:l=\"http://www.w3.org/1999/xlink\">\n\
  <description>\n\
    <title-info>\n\
      <coverpage>\n\
        <image l:href=\"#{cover_id}\"/>\n\
      </coverpage>\n\
    </title-info>\n\
  </description>\n\
  <body><section><p>book text</p></section></body>\n\
  <binary id=\"{cover_id}\" content-type=\"image/png\">{b64}</binary>\n\
</FictionBook>"
        )
        .into_bytes()
    }
}
