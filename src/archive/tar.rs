//! TAR / CBT backend — via `tar` crate, Read only (we use Seek to
//! rewind between listing and extraction passes).

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use crate::limits;
use crate::settings::Settings;

pub(super) fn tar_read_first_image<R: Read + Seek>(
    mut reader: R,
    settings: &Settings,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    // Pass 1: walk the archive and collect image entries, each with
    // its position in the archive. The block scope drops the
    // `tar::Archive` (and its borrow of `reader`) before we seek for
    // pass 2.
    let (target_index, target): (usize, String) = {
        reader.seek(SeekFrom::Start(0))?;
        let mut archive = tar::Archive::new(&mut reader);
        let mut candidates: Vec<(usize, String)> = Vec::new();
        for (index, entry) in archive.entries()?.enumerate() {
            if index >= limits::MAX_ARCHIVE_ENTRIES {
                return Err(format!(
                    "archive has too many entries (> {} limit)",
                    limits::MAX_ARCHIVE_ENTRIES
                )
                .into());
            }
            let entry = entry?;
            if !entry.header().entry_type().is_file() {
                continue;
            }
            if entry.size() > limits::MAX_ENTRY_SIZE {
                continue;
            }
            // `path_bytes`, not `path`: on Windows `path()` fails for a
            // name that isn't UTF-8, and one Shift_JIS name would then
            // take the whole archive down with it. A lossy name is good
            // enough to sort and to check the extension.
            let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
            if settings.accepts_image_ext(&name) {
                candidates.push((index, name));
            }
        }
        settings
            .pick_first_image(candidates)
            .ok_or("archive contains no (small enough) image files")?
    };

    // Pass 2: walk again and extract the entry at the chosen position.
    // Matching on position instead of on the name means a second entry
    // with the same name can't stand in for the one that was picked.
    reader.seek(SeekFrom::Start(0))?;
    let mut archive = tar::Archive::new(&mut reader);
    for (index, entry) in archive.entries()?.enumerate() {
        let entry = entry?;
        if index == target_index {
            let size = entry.size();
            let buf = limits::read_capped(entry, size, limits::MAX_ENTRY_SIZE)?;
            return Ok((target, buf));
        }
    }

    Err("tar target not found on second pass".into())
}

#[cfg(test)]
mod tests {
    use super::super::read_first_image;
    use crate::settings::Settings;
    use std::io::Cursor;

    #[test]
    fn detect_tar_ustar_at_257() {
        let mut buf = vec![0u8; 512];
        buf[257..262].copy_from_slice(b"ustar");
        assert_eq!(super::super::detect_format(&buf), super::super::Format::Tar);
    }

    fn build_tar(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        let mut buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut buf);
            for (name, body) in entries {
                let mut header = tar::Header::new_ustar();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, *body).unwrap();
            }
            builder.finish().unwrap();
        }
        Cursor::new(buf)
    }

    #[test]
    fn tar_picks_first_image_natural_order() {
        let tar = build_tar(&[
            ("page10.png", b"TEN"),
            ("page2.png", b"TWO"),
            ("page1.png", b"ONE"),
            ("notes.txt", b"text"),
        ]);
        let (name, bytes) = read_first_image(tar, &Settings::default()).expect("read_first_image");
        assert_eq!(name, "page1.png");
        assert_eq!(bytes, b"ONE");
    }

    /// Like `build_tar`, but takes raw name bytes so a name that is not
    /// UTF-8 can be written. `tar::Builder::append_data` only accepts
    /// paths, which on Windows must be Unicode.
    fn build_raw_tar(entries: &[(&[u8], &[u8])]) -> Cursor<Vec<u8>> {
        let mut buf = Vec::new();
        for (name, body) in entries {
            let mut header = tar::Header::new_ustar();
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_size(body.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            buf.extend_from_slice(header.as_bytes());
            buf.extend_from_slice(body);
            buf.resize(buf.len().next_multiple_of(512), 0);
        }
        buf.extend_from_slice(&[0u8; 1024]); // end-of-archive marker
        Cursor::new(buf)
    }

    #[test]
    fn tar_with_a_non_utf8_name_still_yields_a_thumbnail() {
        // "表紙.txt" in Shift_JIS, followed by an ordinary image.
        let sjis_name: &[u8] = b"\x95\x5c\x8e\x86.txt";
        let tar = build_raw_tar(&[(sjis_name, b"notes"), (b"page1.png", b"image bytes")]);
        let (name, bytes) = read_first_image(tar, &Settings::default()).expect("tar read");
        assert_eq!(name, "page1.png");
        assert_eq!(bytes, b"image bytes");
    }

    #[test]
    fn tar_image_with_a_non_utf8_name_is_extracted() {
        let sjis_name: &[u8] = b"\x95\x5c\x8e\x86.png";
        let tar = build_raw_tar(&[(sjis_name, b"image bytes")]);
        let (name, bytes) = read_first_image(tar, &Settings::default()).expect("tar read");
        assert!(
            name.ends_with(".png"),
            "lossy name keeps the extension: {name}"
        );
        assert_eq!(bytes, b"image bytes");
    }

    #[test]
    fn tar_duplicate_names_extract_the_picked_entry() {
        // Sorting is stable, so the first `a.png` is the one picked,
        // and it is the one that has to come back.
        let tar = build_raw_tar(&[(b"a.png", b"first"), (b"a.png", b"second")]);
        let (_, bytes) = read_first_image(tar, &Settings::default()).expect("tar read");
        assert_eq!(bytes, b"first");
    }

    #[test]
    fn tar_picks_cover_over_sort() {
        let tar = build_tar(&[
            ("aaa.jpg", b"A"),
            ("cover.jpg", b"COVER"),
            ("zzz.jpg", b"Z"),
        ]);
        let (name, _) = read_first_image(tar, &Settings::default()).expect("read_first_image");
        assert_eq!(name, "cover.jpg");
    }

    // ---------------------------------------------------------------
    // end-to-end: image-extension mask gating
    // ---------------------------------------------------------------

    #[test]
    fn tar_mask_excludes_disabled_image_extension() {
        use crate::settings::{SUPPORTED_IMAGE_EXTS, Settings, default_enabled_image_exts_mask};

        let jpg_idx = SUPPORTED_IMAGE_EXTS
            .iter()
            .position(|&e| e == ".jpg")
            .unwrap();
        let tar = build_tar(&[("a.jpg", b"JPG"), ("b.png", b"PNG")]);
        let settings = Settings {
            enabled_image_exts_mask: !(1u32 << jpg_idx) & default_enabled_image_exts_mask(),
            cover_mode: crate::settings::CoverMode::Ignore,
            ..Settings::default()
        };
        let (name, _) = read_first_image(tar, &settings).expect("mask excludes jpg");
        assert_eq!(name, "b.png");
    }

    #[test]
    fn tar_mask_of_zero_rejects_all_images() {
        use crate::settings::Settings;

        let tar = build_tar(&[("only.png", b"PNG")]);
        let settings = Settings {
            enabled_image_exts_mask: 0,
            ..Settings::default()
        };
        assert!(read_first_image(tar, &settings).is_err());
    }

    #[test]
    fn tar_every_supported_extension_round_trips_when_enabled_alone() {
        use crate::settings::{SUPPORTED_IMAGE_EXTS, Settings};

        for (i, ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
            let entry = format!("file{ext}");
            let tar = build_tar(&[(&entry, b"BODY")]);
            let settings = Settings {
                enabled_image_exts_mask: 1u32 << i,
                cover_mode: crate::settings::CoverMode::Ignore,
                ..Settings::default()
            };
            let (name, _) = read_first_image(tar, &settings)
                .unwrap_or_else(|e| panic!("tar ext {ext} solo-enabled failed: {e}"));
            assert_eq!(name, entry);
        }
    }

    // ---------------------------------------------------------------
    // Entry-count cap.
    // ---------------------------------------------------------------

    #[test]
    fn tar_under_entry_cap_still_works() {
        let tar = build_tar(&[("a.png", b"AAA"), ("b.png", b"BBB")]);
        let (name, _) = read_first_image(tar, &Settings::default()).expect("small tar should pass");
        assert!(name.ends_with(".png"));
    }
}
