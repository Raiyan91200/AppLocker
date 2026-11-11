//! Notification-area (tray) icon and its right-click menu.

use std::mem::{size_of, zeroed};
use std::ptr::null;

use windows_sys::w;
use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, PostMessageW, SetForegroundWindow,
    SetMenuDefaultItem, TrackPopupMenu, MF_SEPARATOR, MF_STRING, TPM_NONOTIFY, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, WM_APP, WM_NULL,
};

use crate::win::copy_wide;

pub const WM_APP_TRAY: u32 = WM_APP + 1;
pub const MENU_SETTINGS: u32 = 1;
pub const MENU_EXIT: u32 = 2;

fn data(hwnd: HWND) -> NOTIFYICONDATAW {
    // SAFETY: plain data struct.
    let mut d: NOTIFYICONDATAW = unsafe { zeroed() };
    d.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    d.hWnd = hwnd;
    d.uID = 1;
    d
}

pub fn add(hwnd: HWND) {
    let mut d = data(hwnd);
    d.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    d.uCallbackMessage = WM_APP_TRAY;
    d.hIcon = crate::ui::icon(true);
    copy_wide(&mut d.szTip, "AppLocker - your apps are protected");
    unsafe { Shell_NotifyIconW(NIM_ADD, &d) };
}

pub fn remove(hwnd: HWND) {
    unsafe { Shell_NotifyIconW(NIM_DELETE, &data(hwnd)) };
}

/// Shows a balloon / toast from the tray icon.
pub fn notify(hwnd: HWND, title: &str, text: &str) {
    let mut d = data(hwnd);
    d.uFlags = NIF_INFO;
    d.dwInfoFlags = NIIF_INFO;
    copy_wide(&mut d.szInfoTitle, title);
    copy_wide(&mut d.szInfo, text);
    unsafe { Shell_NotifyIconW(NIM_MODIFY, &d) };
}

/// Shows the context menu at the cursor and returns the chosen item (0 = none).
pub fn menu(hwnd: HWND) -> u32 {
    // SAFETY: standard popup-menu sequence; the menu is destroyed before returning.
    unsafe {
        let m = CreatePopupMenu();
        AppendMenuW(m, MF_STRING, MENU_SETTINGS as usize, w!("Settings"));
        AppendMenuW(m, MF_SEPARATOR, 0, null());
        AppendMenuW(m, MF_STRING, MENU_EXIT as usize, w!("Exit"));
        SetMenuDefaultItem(m, MENU_SETTINGS, 0);
        let mut pt = POINT { x: 0, y: 0 };
        GetCursorPos(&mut pt);
        SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            m,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY,
            pt.x,
            pt.y,
            0,
            hwnd,
            null(),
        );
        PostMessageW(hwnd, WM_NULL, 0, 0);
        DestroyMenu(m);
        cmd as u32
    }
}
