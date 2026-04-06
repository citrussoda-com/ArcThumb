//! Tiny file-based logger.
//!
//! Shell extensions run inside `explorer.exe`, so `eprintln!` goes
//! nowhere useful. This module appends plain text lines to
//! `%TEMP%\arcthumb.log` so we can see what happened after the fact.
//!
//! Remove or gate this behind a `cfg` flag before shipping.

use std::fs::OpenOptions;
use std::io::Write;

pub fn log(msg: &str) {
    let path = std::env::temp_dir().join("arcthumb.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{msg}");
    }
}

#[macro_export]
macro_rules! alog {
    ($($arg:tt)*) => {{
        $crate::log::log(&format!($($arg)*));
    }};
}
