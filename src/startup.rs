//! "Start with Windows" via `HKCU\...\CurrentVersion\Run` (no admin needed).

use std::ffi::c_void;
use std::ptr::null_mut;

use windows_sys::w;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};

use crate::win::{exe_path, wide};

pub fn is_enabled() -> bool {
    let mut size = 0u32;
    // SAFETY: only queries the value size.
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            w!("AppLocker"),
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        ) == ERROR_SUCCESS
    }
}

pub fn set(enable: bool) -> bool {
    // SAFETY: buffers are valid for the duration of each call.
    unsafe {
        if enable {
            let cmd = wide(&format!("\"{}\" --background", exe_path()));
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                w!("AppLocker"),
                REG_SZ,
                cmd.as_ptr() as *const c_void,
                (cmd.len() * 2) as u32,
            ) == ERROR_SUCCESS
        } else {
            let r = RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                w!("AppLocker"),
            );
            r == ERROR_SUCCESS || r == ERROR_FILE_NOT_FOUND
        }
    }
}
