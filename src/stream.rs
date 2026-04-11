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

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;

    use windows::Win32::Foundation::E_FAIL;
    use windows::Win32::System::Com::{
        ISequentialStream, ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE, STATSTG, STGC,
        STREAM_SEEK,
    };
    use windows::core::{HRESULT, Result, implement};

    // ---------------------------------------------------------------
    // In-memory IStream mock — wraps a Vec<u8> + cursor position.
    // Only Read and Seek are exercised by ComStreamReader, so the
    // remaining IStream methods are stubs.
    // ---------------------------------------------------------------

    #[implement(IStream, ISequentialStream)]
    struct MemStream {
        data: std::cell::RefCell<Vec<u8>>,
        pos: std::cell::Cell<usize>,
    }

    impl MemStream {
        fn new(data: Vec<u8>) -> IStream {
            let s: IStream = Self {
                data: std::cell::RefCell::new(data),
                pos: std::cell::Cell::new(0),
            }
            .into();
            s
        }
    }

    impl ISequentialStream_Impl for MemStream_Impl {
        fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
            let data = self.data.borrow();
            let pos = self.pos.get();
            let available = data.len().saturating_sub(pos);
            let to_read = (cb as usize).min(available);
            if to_read > 0 && !pv.is_null() {
                unsafe {
                    std::ptr::copy_nonoverlapping(data[pos..].as_ptr(), pv as *mut u8, to_read);
                }
            }
            self.pos.set(pos + to_read);
            if !pcbread.is_null() {
                unsafe { *pcbread = to_read as u32 };
            }
            windows::Win32::Foundation::S_OK
        }

        fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
            E_FAIL
        }
    }

    impl IStream_Impl for MemStream_Impl {
        fn Seek(
            &self,
            dlibmove: i64,
            dworigin: STREAM_SEEK,
            plibnewposition: *mut u64,
        ) -> Result<()> {
            let data = self.data.borrow();
            let len = data.len() as i64;
            let new_pos = match dworigin {
                STREAM_SEEK_SET => dlibmove,
                STREAM_SEEK_CUR => self.pos.get() as i64 + dlibmove,
                STREAM_SEEK_END => len + dlibmove,
                _ => return Err(E_FAIL.into()),
            };
            if new_pos < 0 || new_pos > len {
                return Err(E_FAIL.into());
            }
            self.pos.set(new_pos as usize);
            if !plibnewposition.is_null() {
                unsafe { *plibnewposition = new_pos as u64 };
            }
            Ok(())
        }

        fn SetSize(&self, _libnewsize: u64) -> Result<()> {
            Err(E_FAIL.into())
        }

        fn CopyTo(
            &self,
            _pstm: Option<&IStream>,
            _cb: u64,
            _pcbread: *mut u64,
            _pcbwritten: *mut u64,
        ) -> Result<()> {
            Err(E_FAIL.into())
        }

        fn Commit(&self, _grfcommitflags: &STGC) -> Result<()> {
            Ok(())
        }

        fn Revert(&self) -> Result<()> {
            Ok(())
        }

        fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> Result<()> {
            Err(E_FAIL.into())
        }

        fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> Result<()> {
            Err(E_FAIL.into())
        }

        fn Stat(
            &self,
            _pstatstg: *mut STATSTG,
            _grfstatflag: &windows::Win32::System::Com::STATFLAG,
        ) -> Result<()> {
            Err(E_FAIL.into())
        }

        fn Clone(&self) -> Result<IStream> {
            Err(E_FAIL.into())
        }
    }

    // ---------------------------------------------------------------
    // Tests
    // ---------------------------------------------------------------

    #[test]
    fn read_entire_buffer() {
        let stream = MemStream::new(b"hello world".to_vec());
        let mut reader = ComStreamReader::new(stream);
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).expect("read_to_end");
        assert_eq!(buf, b"hello world");
    }

    #[test]
    fn read_in_chunks() {
        let stream = MemStream::new(b"abcdefgh".to_vec());
        let mut reader = ComStreamReader::new(stream);

        let mut chunk = [0u8; 3];
        let n = reader.read(&mut chunk).expect("first read");
        assert_eq!(n, 3);
        assert_eq!(&chunk, b"abc");

        let n = reader.read(&mut chunk).expect("second read");
        assert_eq!(n, 3);
        assert_eq!(&chunk, b"def");

        let n = reader.read(&mut chunk).expect("third read");
        assert_eq!(n, 2);
        assert_eq!(&chunk[..2], b"gh");

        // EOF
        let n = reader.read(&mut chunk).expect("eof read");
        assert_eq!(n, 0);
    }

    #[test]
    fn read_empty_stream() {
        let stream = MemStream::new(vec![]);
        let mut reader = ComStreamReader::new(stream);
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).expect("read");
        assert_eq!(n, 0);
    }

    #[test]
    fn seek_from_start() {
        let stream = MemStream::new(b"0123456789".to_vec());
        let mut reader = ComStreamReader::new(stream);
        let pos = reader.seek(SeekFrom::Start(5)).expect("seek");
        assert_eq!(pos, 5);

        let mut buf = [0u8; 3];
        reader.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"567");
    }

    #[test]
    fn seek_from_current() {
        let stream = MemStream::new(b"abcdefghij".to_vec());
        let mut reader = ComStreamReader::new(stream);

        // Read 4 bytes to advance position
        let mut skip = [0u8; 4];
        reader.read_exact(&mut skip).expect("read");

        // Seek forward 2 from current (position 4 → 6)
        let pos = reader.seek(SeekFrom::Current(2)).expect("seek");
        assert_eq!(pos, 6);

        let mut buf = [0u8; 2];
        reader.read_exact(&mut buf).expect("read");
        assert_eq!(&buf, b"gh");
    }

    #[test]
    fn seek_from_end() {
        let stream = MemStream::new(b"0123456789".to_vec());
        let mut reader = ComStreamReader::new(stream);
        let pos = reader.seek(SeekFrom::End(-3)).expect("seek");
        assert_eq!(pos, 7);

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).expect("read");
        assert_eq!(buf, b"789");
    }

    #[test]
    fn seek_to_start_after_reading() {
        let stream = MemStream::new(b"hello".to_vec());
        let mut reader = ComStreamReader::new(stream);

        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).expect("first read");
        assert_eq!(buf, b"hello");

        // Rewind
        reader.seek(SeekFrom::Start(0)).expect("rewind");
        buf.clear();
        reader.read_to_end(&mut buf).expect("second read");
        assert_eq!(buf, b"hello");
    }

    #[test]
    fn seek_to_end_returns_length() {
        let data = b"twelve bytes";
        let stream = MemStream::new(data.to_vec());
        let mut reader = ComStreamReader::new(stream);
        let pos = reader.seek(SeekFrom::End(0)).expect("seek end");
        assert_eq!(pos, data.len() as u64);
    }
}
