//! 7z backend — via `sevenz-rust`, direct Read+Seek.

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::limits;
use crate::settings::Settings;

pub(super) fn sevenz_read_first_image<R: Read + Seek>(
    mut reader: R,
    settings: &Settings,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    use sevenz_rust::{Archive, BlockDecoder};

    let size = reader.seek(SeekFrom::End(0))?;
    reader.seek(SeekFrom::Start(0))?;
    check_start_header(&mut reader, size)?;
    reader.seek(SeekFrom::Start(0))?;

    let archive = Archive::read(&mut reader, size, &[])?;

    let entry_count = archive.files.len();
    if entry_count > limits::MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "archive has too many entries ({entry_count} > {} limit)",
            limits::MAX_ARCHIVE_ENTRIES
        )
        .into());
    }

    // The 7z metadata lives in the footer, which SevenZReader::new has
    // already parsed — so we can list all entry names without reading
    // any compressed data. Candidates carry their file index so the
    // extraction below matches on it instead of on the name, which
    // need not be unique.
    let candidates: Vec<(usize, String)> = archive
        .files
        .iter()
        .enumerate()
        .filter(|(_, f)| {
            !f.is_directory()
                && settings.accepts_image_ext(&f.name)
                && f.size <= limits::MAX_ENTRY_SIZE
        })
        .map(|(i, f)| (i, f.name.clone()))
        .collect();
    let (target_index, target) = settings
        .pick_first_image(candidates)
        .ok_or("archive contains no (small enough) image files")?;

    // Second phase: open only the block ("folder") that holds the
    // target. Entries sharing a block come out of one decompression
    // stream in order, and sevenz-rust does not skip the ones the
    // callback leaves unread, so everything in front of the target has
    // to be drained or the target's reader starts at the wrong offset.
    let folder_index = archive
        .stream_map
        .file_folder_index
        .get(target_index)
        .copied()
        .flatten()
        .ok_or("7z entry has no data stream")?;
    let mut file_index = archive.stream_map.folder_first_file_index[folder_index];
    let mut skipped: u64 = 0;
    let mut captured: Option<Vec<u8>> = None;
    BlockDecoder::new(folder_index, &archive, &[], &mut reader).for_each_entries(
        &mut |entry, r| {
            let current = file_index;
            file_index += 1;
            if current == target_index {
                captured = Some(limits::read_capped(r, entry.size, limits::MAX_ENTRY_SIZE)?);
                return Ok(false); // stop iteration
            }
            skipped = skipped.saturating_add(entry.size);
            if skipped > limits::MAX_SOLID_SKIP {
                return Err(sevenz_rust::Error::other(
                    "too much data ahead of the image in a solid 7z block",
                ));
            }
            std::io::copy(r, &mut std::io::sink())?;
            Ok(true)
        },
    )?;

    let data = captured.ok_or("7z entry found in metadata but not in stream")?;
    Ok((target, data))
}

/// Reject a signature header whose "next header" lies outside the file
/// or is implausibly large. `Archive::read` allocates the declared size
/// before reading it, so this has to happen first.
fn check_start_header<R: Read>(reader: &mut R, file_size: u64) -> Result<(), Box<dyn Error>> {
    const SIGNATURE_HEADER_SIZE: u64 = 32;

    let mut head = [0u8; SIGNATURE_HEADER_SIZE as usize];
    reader.read_exact(&mut head)?;
    let offset = u64::from_le_bytes(head[12..20].try_into().unwrap());
    let header_size = u64::from_le_bytes(head[20..28].try_into().unwrap());

    if header_size > limits::MAX_SEVENZ_HEADER_SIZE {
        return Err(format!("7z header too large ({header_size} bytes)").into());
    }
    let end = SIGNATURE_HEADER_SIZE
        .checked_add(offset)
        .and_then(|n| n.checked_add(header_size));
    if end.is_none_or(|end| end > file_size) {
        return Err("7z header lies outside the file".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::read_first_image;
    use super::super::tests::make_tiny_png;
    use crate::settings::Settings;
    use std::io::Cursor;

    #[test]
    fn detect_sevenz() {
        assert_eq!(
            super::super::detect_format(b"7z\xBC\xAF\x27\x1Crest"),
            super::super::Format::SevenZ
        );
    }

    fn build_7z(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        use sevenz_rust::{SevenZArchiveEntry, SevenZWriter};
        let mut buf = Vec::new();
        {
            let mut sz = SevenZWriter::new(Cursor::new(&mut buf)).unwrap();
            for (name, body) in entries {
                let mut entry = SevenZArchiveEntry::new();
                entry.name = (*name).to_string();
                entry.has_stream = true;
                entry.size = body.len() as u64;
                sz.push_archive_entry(entry, Some(&mut Cursor::new(*body)))
                    .unwrap();
            }
            sz.finish().unwrap();
        }
        Cursor::new(buf)
    }

    #[test]
    fn sevenz_picks_first_image_natural_order() {
        let png = make_tiny_png();
        let sz = build_7z(&[
            ("page10.png", &png),
            ("page2.png", &png),
            ("page1.png", &png),
            ("notes.txt", b"text"),
        ]);
        let (name, bytes) =
            read_first_image(sz, &Settings::default()).expect("7z read_first_image");
        assert_eq!(name, "page1.png");
        // Round-trip the bytes through the decoder to prove they
        // survived the 7z compression cycle intact.
        let img = crate::decode::decode_with_limits(&name, &bytes).expect("decode 7z entry");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }

    #[test]
    fn sevenz_picks_cover_over_sort() {
        let png = make_tiny_png();
        let sz = build_7z(&[("aaa.jpg", &png), ("cover.jpg", &png), ("zzz.jpg", &png)]);
        let (name, _) = read_first_image(sz, &Settings::default()).expect("7z read_first_image");
        assert_eq!(name, "cover.jpg");
    }

    /// Like `build_7z`, but packs every entry into one solid block, the
    /// way 7-Zip does by default.
    fn build_solid_7z(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        use sevenz_rust::{SevenZArchiveEntry, SevenZWriter, SourceReader};
        let mut buf = Vec::new();
        {
            let mut sz = SevenZWriter::new(Cursor::new(&mut buf)).unwrap();
            let headers = entries
                .iter()
                .map(|(name, _)| {
                    let mut entry = SevenZArchiveEntry::new();
                    entry.name = (*name).to_string();
                    entry.has_stream = true;
                    entry
                })
                .collect();
            let bodies: Vec<SourceReader<Cursor<&[u8]>>> = entries
                .iter()
                .map(|(_, body)| SourceReader::new(Cursor::new(*body)))
                .collect();
            sz.push_archive_entries(headers, bodies.into()).unwrap();
            sz.finish().unwrap();
        }
        Cursor::new(buf)
    }

    #[test]
    fn sevenz_solid_block_returns_the_target_not_the_first_entry() {
        let sz = build_solid_7z(&[
            ("notes.txt", b"not an image, sits first in the block"),
            ("page2.png", b"bytes of page two"),
            ("page1.png", b"bytes of page one"),
            ("cover.png", b"bytes of the cover"),
        ]);
        let (name, bytes) =
            read_first_image(sz, &Settings::default()).expect("solid 7z read_first_image");
        assert_eq!(name, "cover.png");
        assert_eq!(bytes, b"bytes of the cover");
    }

    #[test]
    fn sevenz_duplicate_names_extract_the_picked_entry() {
        // The first `a.png` is over the per-entry cap only in spirit: we
        // can't afford a 500 MiB fixture, so check the index match by
        // content instead. Natural sort is stable, so the first one wins.
        let sz = build_solid_7z(&[("a.png", b"first"), ("a.png", b"second")]);
        let (_, bytes) = read_first_image(sz, &Settings::default()).expect("7z read_first_image");
        assert_eq!(bytes, b"first");
    }

    #[test]
    fn sevenz_oversized_header_claim_is_rejected() {
        // Signature header only, declaring a 100 GiB metadata header.
        let mut head = Vec::new();
        head.extend_from_slice(b"7z\xBC\xAF\x27\x1C\x00\x04");
        head.extend_from_slice(&[0u8; 4]); // start header CRC
        head.extend_from_slice(&0u64.to_le_bytes()); // next header offset
        head.extend_from_slice(&(100u64 << 30).to_le_bytes()); // next header size
        head.extend_from_slice(&[0u8; 4]); // next header CRC
        assert_eq!(head.len(), 32);
        assert!(read_first_image(Cursor::new(head), &Settings::default()).is_err());
    }

    #[test]
    fn sevenz_header_past_end_of_file_is_rejected() {
        let mut head = Vec::new();
        head.extend_from_slice(b"7z\xBC\xAF\x27\x1C\x00\x04");
        head.extend_from_slice(&[0u8; 4]);
        head.extend_from_slice(&0u64.to_le_bytes());
        head.extend_from_slice(&(1u64 << 20).to_le_bytes()); // 1 MiB, file has 32 bytes
        head.extend_from_slice(&[0u8; 4]);
        assert!(read_first_image(Cursor::new(head), &Settings::default()).is_err());
    }

    #[test]
    fn sevenz_with_no_images_errors() {
        let sz = build_7z(&[("readme.txt", b"hello"), ("notes.md", b"# md")]);
        assert!(read_first_image(sz, &Settings::default()).is_err());
    }

    // ---------------------------------------------------------------
    // end-to-end: image-extension mask gating
    // ---------------------------------------------------------------

    #[test]
    fn sevenz_mask_excludes_disabled_image_extension() {
        use crate::settings::{SUPPORTED_IMAGE_EXTS, Settings, default_enabled_image_exts_mask};

        let png = make_tiny_png();
        let jpg_idx = SUPPORTED_IMAGE_EXTS
            .iter()
            .position(|&e| e == ".jpg")
            .unwrap();
        let sz = build_7z(&[("a.jpg", &png), ("b.png", &png)]);
        let settings = Settings {
            enabled_image_exts_mask: !(1u32 << jpg_idx) & default_enabled_image_exts_mask(),
            cover_mode: crate::settings::CoverMode::Ignore,
            ..Settings::default()
        };
        let (name, _) = read_first_image(sz, &settings).expect("mask excludes jpg");
        assert_eq!(name, "b.png");
    }

    #[test]
    fn sevenz_mask_of_zero_rejects_all_images() {
        use crate::settings::Settings;

        let png = make_tiny_png();
        let sz = build_7z(&[("only.png", &png)]);
        let settings = Settings {
            enabled_image_exts_mask: 0,
            ..Settings::default()
        };
        assert!(read_first_image(sz, &settings).is_err());
    }

    #[test]
    fn sevenz_every_supported_extension_round_trips_when_enabled_alone() {
        use crate::settings::{SUPPORTED_IMAGE_EXTS, Settings};

        let png = make_tiny_png();
        for (i, ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
            let entry = format!("file{ext}");
            let sz = build_7z(&[(&entry, &png)]);
            let settings = Settings {
                enabled_image_exts_mask: 1u32 << i,
                cover_mode: crate::settings::CoverMode::Ignore,
                ..Settings::default()
            };
            let (name, _) = read_first_image(sz, &settings)
                .unwrap_or_else(|e| panic!("7z ext {ext} solo-enabled failed: {e}"));
            assert_eq!(name, entry);
        }
    }

    // ---------------------------------------------------------------
    // Entry-count cap.
    //
    // The actual production threshold (`MAX_ARCHIVE_ENTRIES = 100_000`)
    // would push 7z compression-per-entry past a reasonable unit-test
    // budget. We assert here only that a normal-sized archive still
    // works — the cap check itself is verified end-to-end in zip.rs,
    // and the check site in sevenz.rs is one straight-line comparison.
    // ---------------------------------------------------------------

    #[test]
    fn sevenz_under_entry_cap_still_works() {
        let png = make_tiny_png();
        let sz = build_7z(&[("a.png", &png), ("b.png", &png), ("c.png", &png)]);
        let (name, _) = read_first_image(sz, &Settings::default()).expect("small 7z should pass");
        assert!(name.ends_with(".png"));
    }
}
