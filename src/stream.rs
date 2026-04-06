//! Adapter that lets a COM `IStream` be used as a Rust `Read + Seek`.
//!
//! The `zip`, `unrar`, and `sevenz-rust` crates all want a
//! `std::io::Read + Seek` source. Explorer hands us an `IStream` via
//! `IInitializeWithStream::Initialize`. This file bridges the two.

use std::ffi::c_void;
use std::io::{self, Read, Seek, SeekFrom};

use windows::Win32::System::Com::{
    IStream, STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET,
};

/// Wraps an `IStream` in a Rust `Read + Seek` interface.
///
/// The COM stream is owned (via refcount) for the lifetime of this
/// struct. Dropping it releases the reference.
pub struct ComStreamReader {
    stream: IStream,
}

impl ComStreamReader {
    pub fn new(stream: IStream) -> Self {
        Self { stream }
    }
}

impl Read for ComStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut bytes_read: u32 = 0;
        // ISequentialStream::Read can return S_FALSE at EOF, which is
        // "not an error" but would be treated as one by `Result::ok()`.
        // We inspect the HRESULT manually instead.
        let hr = unsafe {
            self.stream.Read(
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
                Some(&mut bytes_read),
            )
        };
        if hr.is_err() {
            return Err(io::Error::other(format!("IStream::Read failed: {hr:?}")));
        }
        Ok(bytes_read as usize)
    }
}

impl Seek for ComStreamReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let (dlibmove, origin): (i64, STREAM_SEEK) = match pos {
            SeekFrom::Start(n) => (n as i64, STREAM_SEEK_SET),
            SeekFrom::Current(n) => (n, STREAM_SEEK_CUR),
            SeekFrom::End(n) => (n, STREAM_SEEK_END),
        };
        let mut new_pos: u64 = 0;
        unsafe {
            self.stream
                .Seek(dlibmove, origin, Some(&mut new_pos))
                .map_err(|e| io::Error::other(format!("IStream::Seek failed: {e}")))?;
        }
        Ok(new_pos)
    }
}
