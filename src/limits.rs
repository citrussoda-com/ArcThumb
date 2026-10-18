//! Hard safety limits.
//!
//! Shell extensions run inside `explorer.exe` — any OOM, hang, or
//! panic here crashes Windows Explorer. These constants cap the
//! worst-case resource usage of a single thumbnail request so that
//! a malicious or malformed archive can't take the user's desktop
//! down with it.

/// Maximum size for ZIP / 7z archives (and ZIP-based formats like
/// EPUB and CBZ). These have a central directory / metadata footer
/// at the end of the file, so the parser reads only the small index
/// plus the single entry it picks — total file size is a poor proxy
/// for our memory use. The real protection comes from `MAX_ENTRY_SIZE`,
/// `MAX_ARCHIVE_ENTRIES`, and the image decoder caps below. 1 TiB is
/// effectively "no limit"; the filesystem hits its own ceiling first.
pub const MAX_ARCHIVE_SIZE_INDEXED: u64 = 1024 * 1024 * 1024 * 1024; // 1 TiB

/// Maximum size for sequential archives (TAR, RAR) and single-file
/// ebook containers (FB2, MOBI). These formats require either reading
/// through the whole stream (TAR / FB2 / MOBI) or buffering the entire
/// file to `%TEMP%` first (RAR, because the unrar binding wants a
/// path), so file size directly bounds resource cost.
pub const MAX_ARCHIVE_SIZE_SEQUENTIAL: u64 = 2 * 1024 * 1024 * 1024; // 2 GiB

/// Maximum number of entries we'll enumerate in any archive. Guards
/// against a malicious or malformed central directory that claims
/// billions of entries: `ZipArchive::new` and `SevenZReader::new`
/// allocate proportional to the claimed count, so without this an
/// attacker could OOM Explorer just by handing us the metadata header.
/// 100k is ~3 orders of magnitude beyond any realistic CBZ/CBR
/// (a 1000-page manga has ~1000 entries).
pub const MAX_ARCHIVE_ENTRIES: usize = 100_000;

/// Maximum per-entry (compressed *or* uncompressed) size for files
/// considered as thumbnail candidates. Larger entries are skipped
/// during listing — we'd rather pick a smaller sibling image than
/// spend a minute decoding a 1 GB TIFF.
pub const MAX_ENTRY_SIZE: u64 = 500 * 1024 * 1024; // 500 MiB

/// Maximum size of the 7z metadata header we let `sevenz-rust2` load.
/// The crate allocates the size declared in the 32-byte signature
/// header after checking only that it fits in the file, so a sparse
/// or padded file can still ask for gigabytes.
/// A real header for 100k entries is a few MiB.
pub const MAX_SEVENZ_HEADER_SIZE: u64 = 64 * 1024 * 1024; // 64 MiB

/// Maximum number of bytes we'll decompress and throw away to reach
/// an entry inside a solid 7z block. Entries in a solid block can only
/// be decoded in order, so reaching a late one costs CPU proportional
/// to everything in front of it.
pub const MAX_SOLID_SKIP: u64 = 512 * 1024 * 1024; // 512 MiB

/// Read `reader` to the end, failing once more than `cap` bytes come
/// out. `size_hint` is the size the container declares for the entry;
/// it only pre-sizes the buffer (clamped to `cap`) and is never trusted
/// as a bound, because a decompressor keeps producing bytes for as
/// long as the compressed stream says so.
pub fn read_capped<R: std::io::Read>(
    reader: R,
    size_hint: u64,
    cap: u64,
) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let mut buf = Vec::with_capacity(size_hint.min(cap) as usize);
    let read = reader.take(cap.saturating_add(1)).read_to_end(&mut buf)?;
    if read as u64 > cap {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("entry exceeds the {cap} byte limit"),
        ));
    }
    Ok(buf)
}

/// Maximum decoded image dimension (width or height, in pixels).
/// Enforced via `image::Limits` before full decode.
pub const MAX_IMAGE_DIMENSION: u32 = 32_768;

/// Maximum bytes the image decoder is allowed to allocate. Defends
/// against "decompression bomb" images (tiny compressed source that
/// expands to gigabytes of pixel data).
pub const MAX_IMAGE_ALLOC: u64 = 512 * 1024 * 1024; // 512 MiB

/// Orphaned temp files (left over from the RAR backend if Explorer
/// was killed mid-extraction) older than this are deleted on the
/// next RAR thumbnail request.
pub const TEMP_FILE_MAX_AGE_SECS: u64 = 3600; // 1 hour

/// Minimum thumbnail side we'll produce. Explorer will never ask for
/// less than 16, but we clamp defensively.
pub const MIN_THUMBNAIL_SIZE: u32 = 16;

/// Maximum thumbnail side. 2560 is Windows's largest standard icon
/// bucket (Extra Large Icons × high DPI).
pub const MAX_THUMBNAIL_SIZE: u32 = 2560;

/// Maximum size of the debug log file at `%TEMP%\arcthumb.log`.
/// When the file exceeds this, it gets truncated on the next write
/// so a forgotten debug session doesn't fill the disk.
pub const MAX_LOG_FILE_SIZE: u64 = 1024 * 1024; // 1 MiB

// Compile-time invariants on the constants above. Using `const { assert! }`
// fails the build (rather than a single test) if a future tweak ever
// inverts one of these relationships.
const _: () = {
    // The whole point of splitting the archive-size cap was that ZIP/7z
    // can afford a much larger ceiling because their parsers don't read
    // the whole file. If these ever invert the split has lost its meaning.
    assert!(MAX_ARCHIVE_SIZE_INDEXED > MAX_ARCHIVE_SIZE_SEQUENTIAL);

    // A 1000-page manga has ~1000 entries; keep at least two orders of
    // magnitude of headroom so legitimate archives are never rejected
    // on entry count alone.
    assert!(MAX_ARCHIVE_ENTRIES >= 10_000);

    // Per-entry size bounds how much we'll read from the archive; the
    // image-alloc budget bounds how much the decoder may expand it to.
    // The decoder budget must be at least as large, otherwise a valid
    // small archive entry could still trip the decoder cap.
    assert!(MAX_IMAGE_ALLOC >= MAX_ENTRY_SIZE);
};

#[cfg(test)]
mod tests {
    use super::read_capped;
    use std::io::Cursor;

    #[test]
    fn read_capped_returns_data_up_to_the_cap() {
        let data = vec![7u8; 100];
        assert_eq!(read_capped(Cursor::new(&data), 100, 100).unwrap(), data);
    }

    #[test]
    fn read_capped_rejects_more_than_the_cap() {
        // The declared size says 10 bytes; the stream keeps going.
        let data = vec![7u8; 101];
        assert!(read_capped(Cursor::new(&data), 10, 100).is_err());
    }

    #[test]
    fn read_capped_does_not_trust_a_huge_size_hint() {
        let data = vec![7u8; 4];
        let buf = read_capped(Cursor::new(&data), u64::MAX, 64).unwrap();
        assert_eq!(buf, data);
        assert!(buf.capacity() <= 64);
    }
}
