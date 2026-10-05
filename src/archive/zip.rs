//! ZIP backend — handles plain ZIPs, CBZ, EPUB, and FB2-inside-ZIP.

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use super::ContentKind;
use crate::settings::Settings;
use crate::{ebook, limits};

/// Look for an `.fb2` entry inside an already-opened ZIP archive
/// (the `.fb2.zip` distribution convention). Returns `None` if no
/// `.fb2` entry exists or the FB2 cover extraction fails.
fn try_extract_fb2_from_zip<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Option<(String, Vec<u8>)> {
    // First pass: find the first `.fb2` entry's index. We look up the
    // second access by index rather than by name: a ZIP entry whose
    // name isn't valid UTF-8 and lacks the EFS flag (legacy-codepage
    // names from Windows tools) is exposed by `name()` as a CP437
    // transcription, but `by_name` keys on the raw bytes, so a name
    // round-trip would miss it. See issue #44.
    let fb2_index: usize = (0..archive.len()).find_map(|i| {
        let f = archive.by_index(i).ok()?;
        if !f.is_file() {
            return None;
        }
        if !f.name().to_ascii_lowercase().ends_with(".fb2") {
            return None;
        }
        if f.size() > limits::MAX_ENTRY_SIZE {
            return None;
        }
        Some(i)
    })?;

    // Second pass: extract that entry's bytes and pass to the FB2
    // cover extractor.
    let entry = archive.by_index(fb2_index).ok()?;
    let size = entry.size();
    let bytes = limits::read_capped(entry, size, limits::MAX_ENTRY_SIZE).ok()?;
    ebook::fb2::try_extract_cover(&bytes)
}

pub(super) fn zip_read_first_image<R: Read + Seek>(
    mut reader: R,
    settings: &Settings,
) -> Result<(String, Vec<u8>, ContentKind), Box<dyn Error>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut archive = zip::ZipArchive::new(reader)?;

    // Entry-count guard. `ZipArchive::new` has already populated its
    // internal list from the central directory, so a hostile archive
    // claiming billions of entries would have already caused trouble
    // by this point — but `len()` is still useful as a backstop and
    // bounds the work the downstream candidate loop will do.
    if archive.len() > limits::MAX_ARCHIVE_ENTRIES {
        return Err(format!(
            "archive has too many entries ({} > {} limit)",
            archive.len(),
            limits::MAX_ARCHIVE_ENTRIES
        )
        .into());
    }

    // EPUB fast path: if the archive carries `META-INF/container.xml`,
    // we can pull the cover from the OPF metadata directly. On any
    // failure (broken XML, missing manifest entry, etc.) we fall
    // through to the generic image scan so slightly malformed EPUBs
    // still produce a thumbnail. Non-EPUB ZIPs cost essentially
    // nothing here — `by_name` returns immediately when missing.
    if let Some((name, bytes)) = ebook::epub::try_extract_cover(&mut archive) {
        return Ok((name, bytes, ContentKind::Epub));
    }

    // FB2.zip fast path: many FB2s are distributed wrapped in a ZIP
    // (the `.fb2.zip` convention). If we find an `.fb2` entry inside
    // the ZIP, route through the FB2 cover-extraction logic instead
    // of the generic image scan. Falls through on failure.
    if let Some((name, bytes)) = try_extract_fb2_from_zip(&mut archive) {
        return Ok((name, bytes, ContentKind::Fb2));
    }

    // Collect image candidates that also fit under the per-entry size
    // cap. Oversized entries are skipped, not an error — maybe a
    // smaller sibling is usable. Each candidate carries its entry index
    // so the chosen image is read back by index, never by name: when an
    // entry name isn't valid UTF-8 and the EFS flag is clear (legacy
    // codepage names written by some Windows ZIP tools), `name()`
    // returns a CP437 transcription whose bytes no longer match the raw
    // central-directory key that `by_name` looks up, so a name round-trip
    // would silently miss the entry. See issue #44.
    let candidates: Vec<(usize, String)> = (0..archive.len())
        .filter_map(|i| {
            let f = archive.by_index(i).ok()?;
            if f.is_file()
                && settings.accepts_image_ext(f.name())
                && f.size() <= limits::MAX_ENTRY_SIZE
            {
                Some((i, f.name().to_string()))
            } else {
                None
            }
        })
        .collect();

    let (index, name) = settings
        .pick_first_image(candidates)
        .ok_or("archive contains no (small enough) image files")?;

    // The size filter above only saw the size the entry declares. The
    // decompressor is not bound by it, so cap what we actually read.
    let file = archive.by_index(index)?;
    let size = file.size();
    let buf = limits::read_capped(file, size, limits::MAX_ENTRY_SIZE)?;

    Ok((name, buf, ContentKind::Zip))
}

#[cfg(test)]
mod tests {
    use super::super::{read_first_image, tests::make_tiny_png};
    use crate::settings::Settings;
    use std::io::Cursor;

    // ---------------------------------------------------------------
    // detect_format: ZIP
    // ---------------------------------------------------------------

    #[test]
    fn detect_zip_local_header() {
        assert_eq!(
            super::super::detect_format(b"PK\x03\x04rest"),
            super::super::Format::Zip
        );
    }

    #[test]
    fn detect_zip_empty_archive() {
        assert_eq!(
            super::super::detect_format(b"PK\x05\x06rest"),
            super::super::Format::Zip
        );
    }

    #[test]
    fn detect_zip_spanned() {
        assert_eq!(
            super::super::detect_format(b"PK\x07\x08rest"),
            super::super::Format::Zip
        );
    }

    #[test]
    fn detect_zip_rejects_other_pk_variants() {
        assert_eq!(
            super::super::detect_format(b"PK\x01\x02xxxx"),
            super::super::Format::Unknown
        );
    }

    // ---------------------------------------------------------------
    // end-to-end: ZIP backend
    // ---------------------------------------------------------------

    /// Build an in-memory ZIP containing the named entries with the
    /// given (uncompressed) bodies. Returns a Cursor ready for
    /// `read_first_image`.
    fn build_zip(entries: &[(&str, &[u8])]) -> Cursor<Vec<u8>> {
        use zip::write::SimpleFileOptions;
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, body) in entries {
                w.start_file(*name, opts).unwrap();
                std::io::Write::write_all(&mut w, body).unwrap();
            }
            w.finish().unwrap();
        }
        Cursor::new(buf)
    }

    /// Hand-build a single-entry stored ZIP whose entry name is the raw
    /// byte sequence `name_bytes`, with the UTF-8 (EFS) general-purpose
    /// flag set to `efs`. The high-level `ZipWriter` always sets EFS for
    /// non-ASCII names, so we craft the headers by hand to model archives
    /// produced by Windows tools that store names in a legacy codepage
    /// (Shift-JIS, GBK, …) with EFS clear. Used to reproduce issue #44.
    fn build_raw_zip_single(name_bytes: &[u8], body: &[u8], efs: bool) -> Cursor<Vec<u8>> {
        let crc = {
            let mut h = crc32fast::Hasher::new();
            h.update(body);
            h.finalize()
        };
        let flags: u16 = if efs { 0x0800 } else { 0x0000 };
        let nlen = name_bytes.len() as u16;
        let blen = body.len() as u32;

        let mut out = Vec::new();
        // Local file header.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&blen.to_le_bytes()); // compressed size
        out.extend_from_slice(&blen.to_le_bytes()); // uncompressed size
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(name_bytes);
        out.extend_from_slice(body);

        // Central directory header.
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // version made by
        out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        out.extend_from_slice(&flags.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        out.extend_from_slice(&0u16.to_le_bytes()); // mod time
        out.extend_from_slice(&0u16.to_le_bytes()); // mod date
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&blen.to_le_bytes());
        out.extend_from_slice(&blen.to_le_bytes());
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // extra len
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number start
        out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // local header offset
        out.extend_from_slice(name_bytes);
        let cd_size = out.len() as u32 - cd_offset;

        // End of central directory.
        out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // disk number
        out.extend_from_slice(&0u16.to_le_bytes()); // cd start disk
        out.extend_from_slice(&1u16.to_le_bytes()); // entries this disk
        out.extend_from_slice(&1u16.to_le_bytes()); // total entries
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes()); // comment len

        Cursor::new(out)
    }

    #[test]
    fn zip_non_utf8_name_without_efs_flag_is_extracted() {
        // Issue #44: a Shift-JIS folder name ("日本/") with the EFS flag
        // clear. The `.jpg` entry must still produce a thumbnail.
        let body = make_tiny_png();
        let mut name = vec![0x93u8, 0xFA, 0x96, 0x7B]; // 日本 in Shift-JIS
        name.extend_from_slice(b"/sample.jpg");
        let zip = build_raw_zip_single(&name, &body, false);
        let (_name, bytes) =
            read_first_image(zip, &Settings::default()).expect("non-UTF-8 name must extract");
        assert_eq!(bytes, body);
    }

    #[test]
    fn fb2_in_zip_with_non_utf8_name_without_efs_flag_is_extracted() {
        // Issue #44, FB2 path: the `.fb2.zip` wrapper entry carries a
        // legacy-codepage name with the EFS flag clear. The cover must
        // still come through.
        let png = make_tiny_png();
        let fb2 = super::super::tests::build_fb2("c.png", &png);
        let mut name = vec![0x93u8, 0xFA, 0x96, 0x7B]; // 日本 in Shift-JIS
        name.extend_from_slice(b".fb2");
        let zip = build_raw_zip_single(&name, &fb2, false);
        let (name, bytes) =
            read_first_image(zip, &Settings::default()).expect("non-UTF-8 fb2.zip must extract");
        assert_eq!(name, "c.png");
        let img = crate::decode::decode_with_limits(&name, &bytes).expect("decode fb2.zip cover");
        assert_eq!(img.width(), 2);
    }

    #[test]
    fn zip_picks_cover_when_present() {
        let zip = build_zip(&[
            ("page01.jpg", b"AAA"),
            ("page02.jpg", b"BBB"),
            ("cover.jpg", b"COVER"),
            ("readme.txt", b"ignore me"),
        ]);
        let (name, bytes) = read_first_image(zip, &Settings::default()).expect("read_first_image");
        assert_eq!(name, "cover.jpg");
        assert_eq!(bytes, b"COVER");
    }

    #[test]
    fn zip_natural_sort_picks_page1() {
        // No cover, but natural sort should put page1 ahead of page10.
        let zip = build_zip(&[
            ("page10.jpg", b"TEN"),
            ("page2.jpg", b"TWO"),
            ("page1.jpg", b"ONE"),
        ]);
        let (name, bytes) = read_first_image(zip, &Settings::default()).expect("read_first_image");
        assert_eq!(name, "page1.jpg");
        assert_eq!(bytes, b"ONE");
    }

    #[test]
    fn zip_skips_non_image_files() {
        let zip = build_zip(&[("notes.txt", b"text"), ("only.png", b"PNG_BYTES")]);
        let (name, _) = read_first_image(zip, &Settings::default()).expect("read_first_image");
        assert_eq!(name, "only.png");
    }

    #[test]
    fn zip_with_no_images_errors() {
        let zip = build_zip(&[("a.txt", b"text"), ("b.md", b"md")]);
        let result = read_first_image(zip, &Settings::default());
        assert!(
            result.is_err(),
            "expected error, got {:?}",
            result.map(|(n, _)| n)
        );
    }

    // ---------------------------------------------------------------
    // end-to-end: FB2 inside ZIP (`.fb2.zip` distribution variant)
    // ---------------------------------------------------------------

    #[test]
    fn fb2_inside_zip_is_extracted() {
        let png = make_tiny_png();
        let fb2 = super::super::tests::build_fb2("c.png", &png);
        let zip = build_zip(&[("book.fb2", &fb2)]);
        let (name, bytes) = read_first_image(zip, &Settings::default()).expect("fb2.zip read");
        assert_eq!(name, "c.png");
        let img = crate::decode::decode_with_limits(&name, &bytes).expect("decode fb2.zip cover");
        assert_eq!(img.width(), 2);
    }

    #[test]
    fn fb2_inside_zip_skips_unrelated_zip_images() {
        let png = make_tiny_png();
        let fb2 = super::super::tests::build_fb2("inside.png", &png);
        let zip = build_zip(&[("book.fb2", &fb2), ("zzz.png", b"not really a png")]);
        let (name, _) = read_first_image(zip, &Settings::default()).expect("fb2.zip read");
        assert_eq!(name, "inside.png");
    }

    #[test]
    fn zip_without_fb2_or_epub_still_uses_generic_scan() {
        let zip = build_zip(&[("page1.jpg", b"data")]);
        let (name, _) = read_first_image(zip, &Settings::default()).expect("plain ZIP read");
        assert_eq!(name, "page1.jpg");
    }

    // ---------------------------------------------------------------
    // ContentKind: the ZIP backend distinguishes plain ZIP / EPUB /
    // FB2-in-ZIP by content even though they share the `PK` magic.
    // This drives the identification overlay's colour and label.
    // ---------------------------------------------------------------

    #[test]
    fn plain_zip_reports_zip_kind() {
        use super::super::{ContentKind, read_first_image_with_kind};
        let zip = build_zip(&[("page1.jpg", b"data")]);
        let e = read_first_image_with_kind(zip, &Settings::default()).expect("plain ZIP read");
        assert_eq!(e.kind, ContentKind::Zip);
    }

    #[test]
    fn epub_reports_epub_kind() {
        use super::super::{ContentKind, read_first_image_with_kind};
        let png = make_tiny_png();
        let opf = r#"<?xml version="1.0"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata/>
  <manifest>
    <item id="cov" href="img/cover.png" media-type="image/png" properties="cover-image"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[("OEBPS/img/cover.png", &png)],
        );
        let e = read_first_image_with_kind(epub, &Settings::default()).expect("EPUB read");
        assert_eq!(e.kind, ContentKind::Epub);
    }

    #[test]
    fn fb2_in_zip_reports_fb2_kind() {
        use super::super::{ContentKind, read_first_image_with_kind};
        let png = make_tiny_png();
        let fb2 = super::super::tests::build_fb2("c.png", &png);
        let zip = build_zip(&[("book.fb2", &fb2)]);
        let e = read_first_image_with_kind(zip, &Settings::default()).expect("fb2.zip read");
        assert_eq!(e.kind, ContentKind::Fb2);
    }

    // ---------------------------------------------------------------
    // end-to-end: EPUB fast path through the ZIP backend
    // ---------------------------------------------------------------

    /// Build an in-memory EPUB ZIP.
    fn build_epub(
        container_xml: &str,
        opf_path: &str,
        opf_xml: &str,
        extras: &[(&str, &[u8])],
    ) -> Cursor<Vec<u8>> {
        use zip::write::SimpleFileOptions;
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);

            w.start_file("mimetype", opts).unwrap();
            std::io::Write::write_all(&mut w, b"application/epub+zip").unwrap();

            w.start_file("META-INF/container.xml", opts).unwrap();
            std::io::Write::write_all(&mut w, container_xml.as_bytes()).unwrap();

            w.start_file(opf_path, opts).unwrap();
            std::io::Write::write_all(&mut w, opf_xml.as_bytes()).unwrap();

            for (name, body) in extras {
                w.start_file(*name, opts).unwrap();
                std::io::Write::write_all(&mut w, body).unwrap();
            }
            w.finish().unwrap();
        }
        Cursor::new(buf)
    }

    fn standard_container_xml() -> &'static str {
        r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#
    }

    #[test]
    fn epub2_meta_cover_extracted() {
        let png = make_tiny_png();
        let opf = r#"<?xml version="1.0"?>
<package version="2.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata>
    <meta name="cover" content="cover-image"/>
  </metadata>
  <manifest>
    <item id="cover-image" href="images/front.png" media-type="image/png"/>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[
                ("OEBPS/images/front.png", &png),
                ("OEBPS/images/zzz.png", b"NOT THIS ONE"),
            ],
        );
        let (name, bytes) = read_first_image(epub, &Settings::default()).expect("EPUB read");
        assert_eq!(name, "OEBPS/images/front.png");
        let img = crate::decode::decode_with_limits(&name, &bytes).expect("decode EPUB cover");
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 2);
    }

    #[test]
    fn epub3_properties_cover_image_extracted() {
        let png = make_tiny_png();
        let opf = r#"<?xml version="1.0"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata/>
  <manifest>
    <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
    <item id="cov" href="img/cover.png" media-type="image/png" properties="cover-image"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[("OEBPS/img/cover.png", &png)],
        );
        let (name, _) = read_first_image(epub, &Settings::default()).expect("EPUB read");
        assert_eq!(name, "OEBPS/img/cover.png");
    }

    #[test]
    fn epub_cover_pointing_at_xhtml_falls_back_to_the_image_scan() {
        // EPUB 2 books often name a wrapper page as the cover.
        let png = make_tiny_png();
        let opf = r#"<?xml version="1.0"?>
<package version="2.0" xmlns="http://www.idpf.org/2007/opf">
  <metadata><meta name="cover" content="cover"/></metadata>
  <manifest>
    <item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/>
    <item id="img" href="images/cover.jpg" media-type="image/jpeg"/>
  </manifest>
</package>"#;
        let xhtml: &[u8] = b"\xEF\xBB\xBF<?xml version=\"1.0\"?>\n<html><body><img src=\"images/cover.jpg\"/></body></html>";
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[
                ("OEBPS/cover.xhtml", xhtml),
                ("OEBPS/images/cover.jpg", &png),
            ],
        );
        let (name, bytes) =
            read_first_image(epub, &Settings::default()).expect("fallback to image scan");
        assert_eq!(name, "OEBPS/images/cover.jpg");
        assert_eq!(bytes, png);
    }

    #[test]
    fn epub_cover_href_is_percent_decoded() {
        let png = make_tiny_png();
        // A decoy that sorts first, so a failed lookup that fell back
        // to the generic scan would pick the wrong file.
        let decoy = b"not the cover".to_vec();
        let opf = r#"<?xml version="1.0"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <manifest>
    <item id="c" href="Images/%E8%A1%A8%E7%B4%99%20art.png#top" properties="cover-image" media-type="image/png"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[
                ("OEBPS/Images/000.png", &decoy),
                ("OEBPS/Images/\u{8868}\u{7d19} art.png", &png),
            ],
        );
        let (name, bytes) = read_first_image(epub, &Settings::default()).expect("epub cover");
        assert_eq!(name, "OEBPS/Images/\u{8868}\u{7d19} art.png");
        assert_eq!(bytes, png);
    }

    #[test]
    fn epub_cover_href_with_a_literal_percent_still_resolves() {
        let png = make_tiny_png();
        let opf = r#"<?xml version="1.0"?>
<package version="3.0" xmlns="http://www.idpf.org/2007/opf">
  <manifest>
    <item id="c" href="100%20off.png" properties="cover-image" media-type="image/png"/>
  </manifest>
</package>"#;
        // The entry is literally named with `%20` in it.
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[("OEBPS/000.png", b"decoy"), ("OEBPS/100%20off.png", &png)],
        );
        let (name, bytes) = read_first_image(epub, &Settings::default()).expect("epub cover");
        assert_eq!(name, "OEBPS/100%20off.png");
        assert_eq!(bytes, png);
    }

    #[test]
    fn epub_fallback_when_no_metadata() {
        let png = make_tiny_png();
        let opf = r#"<package>
  <metadata/>
  <manifest>
    <item id="ch1" href="ch1.xhtml"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[("OEBPS/page1.png", &png), ("OEBPS/page2.png", &png)],
        );
        let (name, _) = read_first_image(epub, &Settings::default()).expect("EPUB read");
        assert!(
            name.ends_with("page1.png"),
            "expected page1.png fallback, got {name}"
        );
    }

    #[test]
    fn epub_fallback_when_opf_points_to_missing_image() {
        let png = make_tiny_png();
        let opf = r#"<package version="2.0">
  <metadata>
    <meta name="cover" content="cover-image"/>
  </metadata>
  <manifest>
    <item id="cover-image" href="images/MISSING.jpg" media-type="image/jpeg"/>
  </manifest>
</package>"#;
        let epub = build_epub(
            standard_container_xml(),
            "OEBPS/content.opf",
            opf,
            &[("OEBPS/cover.png", &png)],
        );
        let (name, _) = read_first_image(epub, &Settings::default()).expect("EPUB read");
        assert!(name.ends_with("cover.png"));
    }

    #[test]
    fn epub_fallback_when_container_xml_is_garbage() {
        let png = make_tiny_png();
        let epub = build_epub(
            "this is not xml",
            "OEBPS/content.opf",
            "<package/>",
            &[("OEBPS/cover.png", &png)],
        );
        let (name, _) = read_first_image(epub, &Settings::default()).expect("EPUB read");
        assert!(name.ends_with("cover.png"));
    }

    #[test]
    fn epub_with_root_level_opf() {
        let png = make_tiny_png();
        let container =
            r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#;
        let opf = r#"<package version="3.0">
  <manifest>
    <item id="c" href="cover.png" properties="cover-image"/>
  </manifest>
</package>"#;
        let epub = build_epub(container, "content.opf", opf, &[("cover.png", &png)]);
        let (name, _) = read_first_image(epub, &Settings::default()).expect("EPUB read");
        assert_eq!(name, "cover.png");
    }

    // ---------------------------------------------------------------
    // end-to-end: image-extension mask gating
    // ---------------------------------------------------------------

    #[test]
    fn mask_excludes_disabled_image_extension_from_candidates() {
        use crate::settings::{SUPPORTED_IMAGE_EXTS, Settings};

        let png = make_tiny_png();
        // Zip with one .jpg and one .png. The .jpg sorts before .png,
        // so with all-on mask the .jpg would be picked; with .jpg
        // disabled, the .png must be picked instead.
        let zip = build_zip(&[("a.jpg", &png), ("b.png", &png)]);

        let jpg_idx = SUPPORTED_IMAGE_EXTS
            .iter()
            .position(|&e| e == ".jpg")
            .unwrap();
        let settings = Settings {
            enabled_image_exts_mask: !(1u32 << jpg_idx)
                & crate::settings::default_enabled_image_exts_mask(),
            cover_mode: crate::settings::CoverMode::Ignore,
            ..Settings::default()
        };
        let (name, _) = read_first_image(zip, &settings).expect("jpg disabled");
        assert_eq!(name, "b.png");

        // Inverse: disable .png, .jpg should be picked.
        let zip = build_zip(&[("a.jpg", &png), ("b.png", &png)]);
        let png_idx = SUPPORTED_IMAGE_EXTS
            .iter()
            .position(|&e| e == ".png")
            .unwrap();
        let settings = Settings {
            enabled_image_exts_mask: !(1u32 << png_idx)
                & crate::settings::default_enabled_image_exts_mask(),
            cover_mode: crate::settings::CoverMode::Ignore,
            ..Settings::default()
        };
        let (name, _) = read_first_image(zip, &settings).expect("png disabled");
        assert_eq!(name, "a.jpg");
    }

    #[test]
    fn mask_of_zero_rejects_all_images_even_in_archive() {
        use crate::settings::Settings;

        let png = make_tiny_png();
        let zip = build_zip(&[("only.png", &png)]);
        let settings = Settings {
            enabled_image_exts_mask: 0,
            ..Settings::default()
        };
        let result = read_first_image(zip, &settings);
        assert!(result.is_err(), "zero mask must produce no-image error");
    }

    #[test]
    fn every_supported_extension_round_trips_through_zip_when_enabled_alone() {
        // For every supported extension, build a zip with only that
        // extension present, configure a mask that enables only that
        // extension, and verify it's picked.
        use crate::settings::{SUPPORTED_IMAGE_EXTS, Settings};

        let body = make_tiny_png();
        for (i, ext) in SUPPORTED_IMAGE_EXTS.iter().enumerate() {
            let entry = format!("file{ext}");
            let zip = build_zip(&[(&entry, &body)]);
            let settings = Settings {
                enabled_image_exts_mask: 1u32 << i,
                cover_mode: crate::settings::CoverMode::Ignore,
                ..Settings::default()
            };
            let (name, _) = read_first_image(zip, &settings)
                .unwrap_or_else(|e| panic!("ext {ext} with solo-enabled mask failed: {e}"));
            assert_eq!(name, entry, "should pick {entry} under solo mask");
        }
    }

    #[test]
    fn plain_zip_still_works_after_epub_fast_path() {
        let zip = build_zip(&[("page01.jpg", b"AAA"), ("page02.jpg", b"BBB")]);
        let (name, _) = read_first_image(zip, &Settings::default()).expect("plain ZIP read");
        assert_eq!(name, "page01.jpg");
    }

    // ---------------------------------------------------------------
    // Entry-count cap (`limits::MAX_ARCHIVE_ENTRIES`).
    // ---------------------------------------------------------------

    /// Builds a ZIP with `n` zero-byte non-image entries plus one
    /// trailing PNG, all stored (no compression). Used to drive the
    /// entry-count cap test without building a heavy archive.
    fn build_zip_with_n_dummies(n: usize) -> Cursor<Vec<u8>> {
        use zip::write::SimpleFileOptions;
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            let opts =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for i in 0..n {
                let name = format!("f{i:07}.bin");
                w.start_file(&name, opts).unwrap();
                // empty body — keeps the archive tiny
            }
            w.start_file("cover.png", opts).unwrap();
            std::io::Write::write_all(&mut w, &make_tiny_png()).unwrap();
            w.finish().unwrap();
        }
        Cursor::new(buf)
    }

    #[test]
    fn zip_under_entry_cap_still_works() {
        // Sanity: an archive with a few entries goes through normally.
        let zip = build_zip_with_n_dummies(5);
        let (name, _) =
            read_first_image(zip, &Settings::default()).expect("small archive should pass");
        assert_eq!(name, "cover.png");
    }

    #[test]
    fn zip_over_entry_cap_is_rejected() {
        use crate::limits::MAX_ARCHIVE_ENTRIES;

        // One past the cap: must fail. Total archive size stays modest
        // (~5 MB of central directory for empty stored entries) so this
        // remains a normal-cost unit test even at the production cap.
        let zip = build_zip_with_n_dummies(MAX_ARCHIVE_ENTRIES);
        let err = read_first_image(zip, &Settings::default()).expect_err("must reject");
        let msg = err.to_string();
        assert!(
            msg.contains("too many entries"),
            "error should mention entry count, got: {msg}"
        );
    }
}
