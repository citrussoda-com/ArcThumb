//! Ebook format backends.
//!
//! Each submodule handles one ebook family and exposes a small API
//! that the generic archive backends can call as a fast path before
//! falling back to their own image-scanning logic.
//!
//! Currently:
//! - `epub` — EPUB 2 / EPUB 3 cover-image extraction via OPF metadata.
//! - `fb2`  — FictionBook XML with base64-embedded `<binary>` images.
//! - `mobi` — Amazon Kindle (MOBI / AZW / AZW3) cover records via
//!   EXTH 201, with first-image-record fallback.

pub mod epub;
pub mod fb2;
pub mod mobi;
