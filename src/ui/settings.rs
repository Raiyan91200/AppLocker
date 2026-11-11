//! Settings window: locked-app list, add/remove, change password, start with Windows.

use std::cell::Cell;
use std::mem::{size_of, zeroed};
use std::ptr::null_mut;

use windows_sys::w;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
    OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{
    SetWindowTheme, BST_CHECKED, BST_UNCHECKED, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS,
    LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETITEMTEXTW, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_NOSORTHEADER,
    LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{
    button, child, force_foreground, label, message_box, modal_loop, register_class, s, set_state,
    state, top_window, ID_CANCEL,
};
use crate::config::{self, LockedApp};
use crate::win::{exe_path, file_name_lower, from_wide, wide};
use crate::{crypto, monitor, startup};

const ID_LIST: i32 = 100;
const ID_ADD: i32 = 101;
const ID_REMOVE: i32 = 102;
const ID_PASSWORD: i32 = 103;
const ID_AUTOSTART: i32 = 104;

/// Processes that must never be locked (would break Windows or AppLocker itself).
const NEVER_LOCK: &[&str] = &[
    "explorer.exe", "csrss.exe", "winlogon.exe", "dwm.exe", "svchost.exe", "lsass.exe",
    "services.exe", "smss.exe", "wininit.exe", "logonui.exe", "sihost.exe", "fontdrvhost.exe",
];

struct State {
    list: HWND,
    autostart: HWND,
}

thread_local! {
    static OPEN: Cell<HWND> = const { Cell::new(null_mut()) };
}

fn class() -> *const u16 {
    w!("AppLockerSettings")
}

pub fn is_open() -> bool {
    !OPEN.get().is_null()
}

/// Opens Settings (or brings it to the front if it is already open).
pub fn show() {
    let open = OPEN.get();
    if !open.is_null() {
        force_foreground(open);
        return;
    }
    register_class(class(), proc);
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
    let hwnd = top_window(class(), "AppLocker Settings", WS_EX_APPWINDOW, style, 460, 334);
    if hwnd.is_null() {
        return;
    }

    label(hwnd, "These apps ask for your password every time they start:", 12, 12, 436, 18);
    let list_style = LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_NOSORTHEADER;
    let list = child(hwnd, WC_LISTVIEWW, "", list_style | WS_TABSTOP, WS_EX_CLIENTEDGE, 12, 34, 436, 208, ID_LIST);
    // SAFETY: valid list-view handle and column structs.
    unsafe {
        SetWindowTheme(list, w!("Explorer"), std::ptr::null());
        let ex = (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize;
        SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, ex as usize, ex);
        add_column(list, 0, "App", 130);
        add_column(list, 1, "Location", 280);
    }
    button(hwnd, "Add app...", 12, 252, 100, 28, ID_ADD, false);
    button(hwnd, "Remove", 118, 252, 100, 28, ID_REMOVE, false);
    button(hwnd, "Change password", 318, 252, 130, 28, ID_PASSWORD, false);
    let autostart = child(
        hwnd,
        w!("BUTTON"),
        "Start with Windows",
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP,
        0,
        12,
        298,
        220,
        22,
        ID_AUTOSTART,
    );
    button(hwnd, "Close", 348, 294, 100, 28, ID_CANCEL, false);

    let check = if startup::is_enabled() { BST_CHECKED } else { BST_UNCHECKED };
    unsafe { SendMessageW(autostart, BM_SETCHECK, check as usize, 0) };
    fill(list);

    let mut st = State { list, autostart };
    set_state(hwnd, &mut st);
    OPEN.set(hwnd);
    force_foreground(hwnd);
    modal_loop(hwnd);
    OPEN.set(null_mut());
    unsafe {
        if IsWindow(hwnd) != 0 {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DestroyWindow(hwnd);
        }
    }
}

unsafe fn add_column(list: HWND, index: usize, title: &str, width: i32) {
    let mut text = wide(title);
    let mut col: LVCOLUMNW = zeroed();
    col.mask = LVCF_TEXT | LVCF_WIDTH;
    col.cx = s(width);
    col.pszText = text.as_mut_ptr();
    SendMessageW(list, LVM_INSERTCOLUMNW, index, &col as *const _ as isize);
}

fn fill(list: HWND) {
    // SAFETY: item structs and text buffers live across each SendMessageW call.
    unsafe {
        SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
        for (i, app) in config::current().apps.iter().enumerate() {
            let mut name = wide(&app.name);
            let mut path = wide(&app.path);
            let mut item: LVITEMW = zeroed();
            item.mask = LVIF_TEXT;
            item.iItem = i as i32;
            item.pszText = name.as_mut_ptr();
            SendMessageW(list, LVM_INSERTITEMW, 0, &item as *const _ as isize);
            item.iSubItem = 1;
            item.pszText = path.as_mut_ptr();
            SendMessageW(list, LVM_SETITEMTEXTW, i, &item as *const _ as isize);
        }
    }
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_COMMAND {
        if let Some(st) = state::<State>(hwnd) {
            let (list, autostart) = (st.list, st.autostart);
            let code = ((wp >> 16) & 0xFFFF) as u32;
            match (wp & 0xFFFF) as i32 {
                ID_ADD => add_app(hwnd, list),
                ID_REMOVE => remove_app(hwnd, list),
                ID_PASSWORD => change_password(hwnd),
                ID_AUTOSTART if code == BN_CLICKED => {
                    let on = SendMessageW(autostart, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                    if !startup::set(on) {
                        message_box(hwnd, "Couldn't change the startup setting.", MB_ICONWARNING);
                    }
                }
                ID_CANCEL => {
                    DestroyWindow(hwnd);
                }
                _ => {}
            }
            return 0;
        }
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn save_or_warn(hwnd: HWND, f: impl FnOnce(&mut config::Config)) -> bool {
    match config::update(f) {
        Ok(()) => true,
        Err(e) => {
            message_box(hwnd, &format!("Couldn't save settings:\n{e}"), MB_ICONERROR);
            false
        }
    }
}

fn add_app(hwnd: HWND, list: HWND) {
    let mut buf = [0u16; 1024];
    let filter = wide("Programs (*.exe)\0*.exe\0All files\0*.*\0");
    let initial_dir = wide("C:\\Program Files");
    // SAFETY: struct and buffers are valid for the call.
    let picked = unsafe {
        let mut ofn: OPENFILENAMEW = zeroed();
        ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = hwnd;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = buf.len() as u32;
        ofn.lpstrInitialDir = initial_dir.as_ptr();
        ofn.lpstrTitle = w!("Choose an app to lock");
        ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_HIDEREADONLY | OFN_NOCHANGEDIR;
        GetOpenFileNameW(&mut ofn) != 0
    };
    if !picked {
        return;
    }
    let path = from_wide(&buf);
    let name = file_name_lower(&path);
    if !name.ends_with(".exe") {
        message_box(hwnd, "Please choose a program (.exe) file.", MB_ICONWARNING);
        return;
    }
    if NEVER_LOCK.contains(&name.as_str()) || name == file_name_lower(&exe_path()) {
        message_box(hwnd, &format!("{name} is part of Windows or AppLocker and can't be locked."), MB_ICONWARNING);
        return;
    }
    if config::current().apps.iter().any(|a| a.name == name) {
        message_box(hwnd, &format!("{name} is already locked."), MB_ICONINFORMATION);
        return;
    }
    if save_or_warn(hwnd, |c| c.apps.push(LockedApp { name, path })) {
        monitor::set_locked(&config::current().apps);
        fill(list);
    }
}

fn remove_app(hwnd: HWND, list: HWND) {
    let sel = unsafe { SendMessageW(list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize) };
    if sel < 0 {
        message_box(hwnd, "Select an app in the list first.", MB_ICONINFORMATION);
        return;
    }
    let index = sel as usize;
    if save_or_warn(hwnd, |c| {
        if index < c.apps.len() {
            c.apps.remove(index);
        }
    }) {
        monitor::set_locked(&config::current().apps);
        fill(list);
    }
}

fn change_password(hwnd: HWND) {
    let Some(pw) = super::password::new_password("Change password", "Enter a new master password.") else {
        return;
    };
    let hash = crypto::hash_password(&pw);
    if save_or_warn(hwnd, |c| c.password_hash = hash) {
        message_box(hwnd, "Password changed.", MB_ICONINFORMATION);
    }
}
