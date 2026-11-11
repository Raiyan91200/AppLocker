//! Small helpers for UTF-16 strings and paths.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

/// `&str` -> null-terminated UTF-16 buffer for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

/// Null-terminated UTF-16 buffer -> `String`.
pub fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

/// Copies `s` into a fixed UTF-16 array, truncating and keeping a trailing null.
pub fn copy_wide(dst: &mut [u16], s: &str) {
    let max = dst.len().saturating_sub(1);
    for (d, c) in dst.iter_mut().zip(s.encode_utf16().take(max)) {
        *d = c;
    }
}

/// Full path of this executable.
pub fn exe_path() -> String {
    std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Lower-cased file name of a path, e.g. `C:\...\Chrome.exe` -> `chrome.exe`.
pub fn file_name_lower(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}
