//! Archive reading — Phase 2b: list entries and extract the first image.
//!
//! Phase 3 will add RAR and 7Z backends behind a common interface.

use std::error::Error;
use std::io::{Read, Seek, SeekFrom};

use zip::ZipArchive;

/// Image extensions we recognise inside archives. Case-insensitive.
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "tiff", "tif", "webp", "avif", "ico",
];

fn has_image_ext(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    IMAGE_EXTS
        .iter()
        .any(|ext| lower.ends_with(&format!(".{ext}")))
}

/// Open a ZIP, find the first image entry (alphabetically), and
/// return its raw bytes along with its name.
///
/// Returns an error if the archive is unreadable or contains no
/// recognised image files.
pub fn read_first_image<R: Read + Seek>(
    mut reader: R,
) -> Result<(String, Vec<u8>), Box<dyn Error>> {
    reader.seek(SeekFrom::Start(0))?;
    let mut archive = ZipArchive::new(reader)?;

    // First pass: collect sorted image names. We can't hold a
    // `ZipFile` handle across the loop (it borrows the archive
    // mutably), so we only remember names here.
    let mut names: Vec<String> = (0..archive.len())
        .filter_map(|i| {
            let f = archive.by_index(i).ok()?;
            if f.is_file() && has_image_ext(f.name()) {
                Some(f.name().to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();

    let name = names
        .into_iter()
        .next()
        .ok_or("archive contains no image files")?;

    // Second pass: actually read the chosen file.
    let mut file = archive.by_name(&name)?;
    let mut buf = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut buf)?;

    Ok((name, buf))
}
