//! Password dialogs: unlock a locked app, verify the master password, or set a new one.

use windows_sys::w;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{GetSysColorBrush, SetBkMode, SetTextColor, HDC, TRANSPARENT};
use windows_sys::Win32::UI::Controls::EM_SETCUEBANNER;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use super::{
    button, child, focus, force_foreground, get_text, label, modal_loop, register_class, set_state,
    set_text, state, top_window, COLOR_BTNFACE, ID_CANCEL, ID_OK,
};
use crate::crypto::verify_password;
use crate::win::wide;

const MAX_TRIES: u32 = 3;
const MIN_LEN: usize = 4;
/// `DM_GETDEFID` / `DC_HASDEFID`: tells IsDialogMessage that Enter means OK.
const DM_GETDEFID: u32 = WM_USER;
const DC_HASDEFID: isize = 0x534B;

enum Kind {
    /// Check the entered password against this Argon2 hash.
    Verify(String),
    /// Ask for a new password twice.
    New,
}

struct State {
    kind: Kind,
    edit1: HWND,
    edit2: HWND,
    status: HWND,
    tries: u32,
    result: Option<String>,
}

fn class() -> *const u16 {
    w!("AppLockerPassword")
}

/// Unlock prompt for a locked app. True if the right password was entered.
pub fn unlock(app: &str, hash: &str) -> bool {
    run(
        &format!("AppLocker - {app}"),
        &format!("{app} is locked.\nEnter your password to open it."),
        Kind::Verify(hash.to_owned()),
        "Unlock",
        true,
    )
    .is_some()
}

/// Asks for the master password (before Settings / Exit).
pub fn verify(hash: &str) -> bool {
    run(
        "AppLocker",
        "Enter your master password.",
        Kind::Verify(hash.to_owned()),
        "OK",
        false,
    )
    .is_some()
}

/// Asks for a new password (typed twice). Returns it, or None if cancelled.
pub fn new_password(title: &str, message: &str) -> Option<String> {
    run(title, message, Kind::New, "Save", false)
}

fn run(title: &str, message: &str, kind: Kind, ok_text: &str, topmost: bool) -> Option<String> {
    register_class(class(), proc);
    let two = matches!(kind, Kind::New);
    let (status_y, buttons_y, height) = if two { (118, 142, 182) } else { (86, 110, 150) };
    let ex = WS_EX_DLGMODALFRAME | WS_EX_APPWINDOW | if topmost { WS_EX_TOPMOST } else { 0 };
    let hwnd = top_window(class(), title, ex, WS_POPUP | WS_CAPTION | WS_SYSMENU, 320, height);
    if hwnd.is_null() {
        return None;
    }

    label(hwnd, message, 16, 14, 288, 34);
    let edit_style = (ES_PASSWORD | ES_AUTOHSCROLL) as u32 | WS_TABSTOP;
    let edit1 = child(hwnd, w!("EDIT"), "", edit_style, WS_EX_CLIENTEDGE, 16, 54, 288, 24, 10);
    let edit2 = if two {
        child(hwnd, w!("EDIT"), "", edit_style, WS_EX_CLIENTEDGE, 16, 86, 288, 24, 11)
    } else {
        std::ptr::null_mut()
    };
    cue(edit1, if two { "New password" } else { "Password" });
    if two {
        cue(edit2, "Confirm password");
    }
    let status = label(hwnd, "", 16, status_y, 288, 18);
    button(hwnd, ok_text, 128, buttons_y, 84, 28, ID_OK, true);
    button(hwnd, "Cancel", 220, buttons_y, 84, 28, ID_CANCEL, false);

    let mut st = State { kind, edit1, edit2, status, tries: 0, result: None };
    set_state(hwnd, &mut st);
    force_foreground(hwnd);
    focus(edit1);
    modal_loop(hwnd);
    // SAFETY: still valid if the loop ended because the app is quitting.
    unsafe {
        if IsWindow(hwnd) != 0 {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DestroyWindow(hwnd);
        }
    }
    st.result.take()
}

fn cue(edit: HWND, text: &str) {
    let text = wide(text);
    unsafe { SendMessageW(edit, EM_SETCUEBANNER, 1, text.as_ptr() as isize) };
}

unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if let Some(st) = state::<State>(hwnd) {
        match msg {
            WM_COMMAND => match (wp & 0xFFFF) as i32 {
                ID_OK => {
                    on_ok(hwnd, st);
                    return 0;
                }
                ID_CANCEL => {
                    DestroyWindow(hwnd);
                    return 0;
                }
                _ => {}
            },
            DM_GETDEFID => return (DC_HASDEFID << 16) | ID_OK as isize,
            WM_CTLCOLORSTATIC if lp as HWND == st.status => {
                let hdc = wp as HDC;
                SetTextColor(hdc, 0x0000_20C8); // red (COLORREF is 0x00BBGGRR)
                SetBkMode(hdc, TRANSPARENT as i32);
                return GetSysColorBrush(COLOR_BTNFACE) as LRESULT;
            }
            _ => {}
        }
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

fn on_ok(hwnd: HWND, st: &mut State) {
    let first = get_text(st.edit1);
    match &st.kind {
        Kind::Verify(hash) => {
            if !first.is_empty() && verify_password(&first, hash) {
                st.result = Some(String::new());
                unsafe { DestroyWindow(hwnd) };
                return;
            }
            st.tries += 1;
            if st.tries >= MAX_TRIES {
                unsafe { DestroyWindow(hwnd) };
                return;
            }
            let left = MAX_TRIES - st.tries;
            set_text(st.status, &format!("Wrong password. {left} {} left.", if left == 1 { "try" } else { "tries" }));
            set_text(st.edit1, "");
            focus(st.edit1);
        }
        Kind::New => {
            let second = get_text(st.edit2);
            if first.chars().count() < MIN_LEN {
                set_text(st.status, &format!("Use at least {MIN_LEN} characters."));
                focus(st.edit1);
            } else if first != second {
                set_text(st.status, "The two passwords don't match.");
                set_text(st.edit2, "");
                focus(st.edit2);
            } else {
                st.result = Some(first);
                unsafe { DestroyWindow(hwnd) };
            }
        }
    }
}
