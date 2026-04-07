//! Kindle MOBI / AZW / AZW3 cover-image extraction.
//!
//! Amazon Kindle ebooks come in three closely related variants, all
//! built on top of the Palm Database (PalmDB) container:
//!
//! | Extension | Internal format          |
//! |-----------|---------------------------|
//! | `.mobi`   | MOBI 6 (PalmDOC text)     |
//! | `.azw`    | MOBI 6 with DRM container |
//! | `.azw3`   | KF8 (HTML5/CSS3 layout)   |
//!
//! All three carry image records inside the same PalmDB stream and
//! all three optionally point at a "cover image" via EXTH record
//! 201, which stores a u32 offset that's added to the MOBI header's
//! `first_image_index` to find the cover's PDB record.
//!
//! ## Strategy
//!
//! 1. Try the **EXTH 201** cover-offset path first. This gives us
//!    the publisher's intended cover image.
//! 2. If EXTH 201 is missing, points outside the record list, or
//!    points at a non-image marker (FLIS / FCIS / etc.), fall back
//!    to the **first image record** in the file. That's almost
//!    always the cover for books that don't carry the EXTH hint.
//! 3. If there are no image records at all, return `None` so the
//!    caller falls back to Explorer's default icon.
//!
//! We deliberately don't try to handle DRM. AZW files with DeviceID
//! or KFX content will simply have unreadable image records and we
//! return `None` — the user gets the default icon instead of a
//! broken thumbnail.

use mobi::Mobi;
use mobi::headers::ExthRecord;

/// Try to extract a cover image from raw MOBI/AZW/AZW3 file bytes.
///
/// Returns `Some((synthetic_name, image_bytes))` on success, or
/// `None` if the file isn't a parseable MOBI, has no image records,
/// or is DRM-encumbered in a way that hides the images.
pub fn try_extract_cover(bytes: &[u8]) -> Option<(String, Vec<u8>)> {
    // The `mobi` crate's constructor takes `AsRef<Vec<u8>>` so we
    // need an owned Vec on hand. The cost is one extra alloc; for
    // a thumbnail provider on files of a few MB this is negligible
    // compared to the I/O we're already doing.
    let owned = bytes.to_vec();
    let mobi = Mobi::new(&owned).ok()?;

    // Strategy 1: explicit EXTH 201 cover hint.
    if let Some(cover) = extract_via_exth(&mobi) {
        return Some(cover);
    }

    // Strategy 2: first image record. Most non-EXTH MOBIs put the
    // cover at image index 0.
    let images = mobi.image_records();
    let first = images.first()?;
    if first.content.len() < 4 {
        return None;
    }
    let bytes = first.content.to_vec();
    let name = synthesize_image_name(&bytes);
    Some((name, bytes))
}

// =============================================================================
// EXTH-based cover lookup
// =============================================================================

fn extract_via_exth(mobi: &Mobi) -> Option<(String, Vec<u8>)> {
    // EXTH 201: stores a 4-byte big-endian offset relative to
    // `first_image_index`. Some publishers use it, some don't.
    let record_values = mobi.metadata.exth.get_record(ExthRecord::CoverOffset)?;
    let first_value = record_values.first()?;
    if first_value.len() < 4 {
        return None;
    }
    let offset = u32::from_be_bytes([
        first_value[0],
        first_value[1],
        first_value[2],
        first_value[3],
    ]);

    // 0xFFFFFFFF is the conventional "no cover" sentinel some
    // publishers write instead of omitting the EXTH record entirely.
    if offset == u32::MAX {
        return None;
    }

    let first_image_idx = mobi.metadata.mobi.first_image_index;
    // Use checked_add so a malformed file with first_image_idx near
    // u32::MAX can't wrap around and pick a wrong record.
    let pdb_idx = first_image_idx.checked_add(offset)? as usize;

    let raw = mobi.raw_records();
    let records = raw.records();
    let record = records.get(pdb_idx)?;
    if record.content.len() < 4 {
        return None;
    }

    // Sanity: the EXTH offset is supposed to point at an image, but
    // we've seen MOBIs in the wild where it points at a marker
    // record (FLIS, FCIS, …). Reject those so the caller can fall
    // back to the first image record.
    let prefix = &record.content[..4];
    if is_marker_record(prefix) {
        return None;
    }

    let bytes = record.content.to_vec();
    let name = synthesize_image_name(&bytes);
    Some((name, bytes))
}

/// Known non-image PalmDB record markers used by MOBI. Mirrors the
/// list inside the `mobi` crate's `is_image_record`.
fn is_marker_record(prefix: &[u8]) -> bool {
    matches!(
        prefix,
        b"FLIS" | b"FCIS" | b"SRCS" | b"RESC" | b"BOUN" | b"FDST" | b"DATP" | b"AUDI" | b"VIDE"
    ) || prefix == b"\xe9\x8e\r\n"
}

// =============================================================================
// Filename synthesis
// =============================================================================

/// Sniff an image's format from its magic bytes and produce a
/// reasonable display filename. The name is what we hand back to
/// the rest of the pipeline as the "first image" so the decoder
/// can dispatch on extension if it wants to.
fn synthesize_image_name(bytes: &[u8]) -> String {
    let ext = sniff_image_extension(bytes);
    format!("cover.{ext}")
}

fn sniff_image_extension(bytes: &[u8]) -> &'static str {
    // PNG: 89 50 4E 47 0D 0A 1A 0A
    if bytes.len() >= 8 && &bytes[..8] == b"\x89PNG\r\n\x1a\n" {
        return "png";
    }
    // JPEG: FF D8 FF
    if bytes.len() >= 3 && &bytes[..3] == b"\xff\xd8\xff" {
        return "jpg";
    }
    // GIF: GIF87a or GIF89a
    if bytes.len() >= 6 && (&bytes[..6] == b"GIF87a" || &bytes[..6] == b"GIF89a") {
        return "gif";
    }
    // WebP: starts with RIFF????WEBP
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "webp";
    }
    // BMP: BM
    if bytes.len() >= 2 && &bytes[..2] == b"BM" {
        return "bmp";
    }
    // Unknown — let the decoder sniff for itself via the byte stream.
    "img"
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- sniff_image_extension --------------------------------

    #[test]
    fn sniff_png() {
        assert_eq!(sniff_image_extension(b"\x89PNG\r\n\x1a\n....."), "png");
    }

    #[test]
    fn sniff_jpeg() {
        assert_eq!(sniff_image_extension(b"\xff\xd8\xff\xe0...."), "jpg");
    }

    #[test]
    fn sniff_gif87a() {
        assert_eq!(sniff_image_extension(b"GIF87a..."), "gif");
    }

    #[test]
    fn sniff_gif89a() {
        assert_eq!(sniff_image_extension(b"GIF89a..."), "gif");
    }

    #[test]
    fn sniff_webp() {
        let mut bytes = Vec::from(*b"RIFF\0\0\0\0WEBPVP8 ");
        bytes.extend_from_slice(&[0u8; 16]);
        assert_eq!(sniff_image_extension(&bytes), "webp");
    }

    #[test]
    fn sniff_bmp() {
        assert_eq!(sniff_image_extension(b"BMfoo"), "bmp");
    }

    #[test]
    fn sniff_unknown() {
        assert_eq!(sniff_image_extension(b"random bytes"), "img");
    }

    #[test]
    fn sniff_empty() {
        assert_eq!(sniff_image_extension(b""), "img");
    }

    #[test]
    fn synthesize_includes_extension() {
        assert_eq!(synthesize_image_name(b"\x89PNG\r\n\x1a\nXX"), "cover.png");
        assert_eq!(synthesize_image_name(b"\xff\xd8\xffXX"), "cover.jpg");
        assert_eq!(synthesize_image_name(b"unknown"), "cover.img");
    }

    // ---- is_marker_record --------------------------------------

    #[test]
    fn marker_records_recognised() {
        assert!(is_marker_record(b"FLIS"));
        assert!(is_marker_record(b"FCIS"));
        assert!(is_marker_record(b"SRCS"));
        assert!(is_marker_record(b"RESC"));
        assert!(is_marker_record(b"BOUN"));
        assert!(is_marker_record(b"FDST"));
        assert!(is_marker_record(b"DATP"));
        assert!(is_marker_record(b"AUDI"));
        assert!(is_marker_record(b"VIDE"));
        assert!(is_marker_record(b"\xe9\x8e\r\n"));
    }

    #[test]
    fn image_prefixes_not_marker() {
        // PNG and JPEG magic must NOT be misclassified as markers.
        assert!(!is_marker_record(b"\x89PNG"));
        assert!(!is_marker_record(b"\xff\xd8\xff\xe0"));
        assert!(!is_marker_record(b"GIF8"));
    }

    // ---- try_extract_cover failure paths -----------------------

    #[test]
    fn rejects_garbage_bytes() {
        // Random bytes are not a valid MOBI. The constructor returns
        // an error and we yield None.
        assert!(try_extract_cover(b"this is not a mobi file at all").is_none());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(try_extract_cover(b"").is_none());
    }

    // End-to-end success tests live in `archive::tests` and operate
    // on real fixture bytes. We don't try to hand-craft a MOBI here
    // because the format is significantly more complex than the
    // RAR4 fixture (PalmDB headers, MOBI headers, EXTH header,
    // record-offset table, image records). The `archive` tests
    // exercise the full pipeline using a fixture file approach.
}
