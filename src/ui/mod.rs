//! Shared native Win32 UI helpers: font, DPI scaling, icons, window creation, modal loop.

pub mod password;
pub mod settings;

use std::cell::Cell;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use windows_sys::w;
use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateFontIndirectW, GetStockObject, DEFAULT_GUI_FONT, HBRUSH, HFONT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{
    AttachThreadInput, GetCurrentProcess, GetCurrentThreadId, SetProcessWorkingSetSize,
};
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, LoadIconMetric, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES,
    INITCOMMONCONTROLSEX, LIM_LARGE, LIM_SMALL,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::win::{from_wide, wide};

/// `COLOR_BTNFACE` system color index (standard dialog background).
pub const COLOR_BTNFACE: i32 = 15;
pub const ID_OK: i32 = 1;
pub const ID_CANCEL: i32 = 2;

pub type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

thread_local! {
    static FONT: Cell<HFONT> = const { Cell::new(null_mut()) };
    static DPI: Cell<i32> = const { Cell::new(96) };
    static ICON_SMALL: Cell<HICON> = const { Cell::new(null_mut()) };
    static ICON_LARGE: Cell<HICON> = const { Cell::new(null_mut()) };
}

pub fn init() {
    // SAFETY: plain Win32 initialization calls with valid structs.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_LISTVIEW_CLASSES,
        };
        InitCommonControlsEx(&icc);
        DPI.set((GetDpiForSystem() as i32).max(96));

        let mut ncm: NONCLIENTMETRICSW = zeroed();
        ncm.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        let font = if SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            &mut ncm as *mut _ as *mut _,
            0,
        ) != 0
        {
            CreateFontIndirectW(&ncm.lfMessageFont)
        } else {
            GetStockObject(DEFAULT_GUI_FONT) as HFONT
        };
        FONT.set(font);
    }
}

/// Scales a 96-DPI layout value to the system DPI.
pub fn s(v: i32) -> i32 {
    v * DPI.get() / 96
}

pub fn hinst() -> HINSTANCE {
    unsafe { GetModuleHandleW(null()) }
}

/// App icon from the exe resources (falls back to the system shield icon).
pub fn icon(small: bool) -> HICON {
    let cell = if small { &ICON_SMALL } else { &ICON_LARGE };
    let cached = cell.get();
    if !cached.is_null() {
        return cached;
    }
    let lim = if small { LIM_SMALL } else { LIM_LARGE };
    let mut h: HICON = null_mut();
    // SAFETY: resource id 1 is MAKEINTRESOURCE(1); output pointer is valid.
    #[allow(clippy::manual_dangling_ptr)]
    unsafe {
        if LoadIconMetric(hinst(), 1usize as *const u16, lim, &mut h) < 0 || h.is_null() {
            LoadIconMetric(null_mut(), IDI_SHIELD, lim, &mut h);
        }
    }
    cell.set(h);
    h
}

pub fn register_class(name: *const u16, proc: WndProc) {
    // SAFETY: a second registration of the same class just fails harmlessly.
    unsafe {
        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinst(),
            hIcon: icon(false),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_BTNFACE as usize + 1) as HBRUSH,
            lpszMenuName: null(),
            lpszClassName: name,
        };
        RegisterClassW(&wc);
    }
}

/// Creates a top-level window with the given client size (96-DPI units), centered on screen.
pub fn top_window(class: *const u16, title: &str, ex: u32, style: u32, cw: i32, ch: i32) -> HWND {
    // SAFETY: all pointers are valid for the calls.
    unsafe {
        let mut r = RECT { left: 0, top: 0, right: s(cw), bottom: s(ch) };
        AdjustWindowRectEx(&mut r, style, 0, ex);
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let mut wa: RECT = zeroed();
        SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0);
        let x = wa.left + (wa.right - wa.left - w) / 2;
        let y = wa.top + (wa.bottom - wa.top - h) / 2;
        let title = wide(title);
        CreateWindowExW(ex, class, title.as_ptr(), style, x, y, w, h, null_mut(), null_mut(), hinst(), null())
    }
}

/// Creates a child control at 96-DPI coordinates and gives it the UI font.
#[allow(clippy::too_many_arguments)]
pub fn child(parent: HWND, class: *const u16, text: &str, style: u32, ex: u32, x: i32, y: i32, w: i32, h: i32, id: i32) -> HWND {
    // SAFETY: all pointers are valid for the calls.
    unsafe {
        let text = wide(text);
        let hwnd = CreateWindowExW(
            ex,
            class,
            text.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            s(x),
            s(y),
            s(w),
            s(h),
            parent,
            id as isize as HMENU,
            hinst(),
            null(),
        );
        SendMessageW(hwnd, WM_SETFONT, FONT.get() as usize, 1);
        hwnd
    }
}

#[allow(clippy::too_many_arguments)]
pub fn button(parent: HWND, text: &str, x: i32, y: i32, w: i32, h: i32, id: i32, default: bool) -> HWND {
    let kind = if default { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON } as u32;
    child(parent, w!("BUTTON"), text, kind | WS_TABSTOP, 0, x, y, w, h, id)
}

pub fn label(parent: HWND, text: &str, x: i32, y: i32, w: i32, h: i32) -> HWND {
    child(parent, w!("STATIC"), text, 0, 0, x, y, w, h, -1)
}

pub fn get_text(hwnd: HWND) -> String {
    // SAFETY: buffer is sized from GetWindowTextLengthW.
    unsafe {
        let len = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buf = vec![0u16; len + 1];
        GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        from_wide(&buf)
    }
}

pub fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    unsafe { SetWindowTextW(hwnd, text.as_ptr()) };
}

pub fn focus(hwnd: HWND) {
    unsafe { SetFocus(hwnd) };
}

/// Brings a window to the front even when another app currently has focus.
pub fn force_foreground(hwnd: HWND) {
    // SAFETY: attaching input queues briefly is the standard way to take focus.
    unsafe {
        let me = GetCurrentThreadId();
        let fg = GetForegroundWindow();
        let other = if fg.is_null() { 0 } else { GetWindowThreadProcessId(fg, null_mut()) };
        let attach = other != 0 && other != me;
        if attach {
            AttachThreadInput(me, other, 1);
        }
        ShowWindow(hwnd, SW_SHOW);
        BringWindowToTop(hwnd);
        SetForegroundWindow(hwnd);
        if attach {
            AttachThreadInput(me, other, 0);
        }
    }
}

/// Runs messages until `hwnd` is destroyed. Gives Tab / Enter / Esc dialog keys.
pub fn modal_loop(hwnd: HWND) {
    // SAFETY: standard message loop.
    unsafe {
        let mut msg: MSG = zeroed();
        while IsWindow(hwnd) != 0 {
            let r = GetMessageW(&mut msg, null_mut(), 0, 0);
            if r == 0 {
                // App is quitting: pass the quit on to the outer loop.
                PostQuitMessage(msg.wParam as i32);
                return;
            }
            if r == -1 {
                return;
            }
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

pub fn message_box(owner: HWND, text: &str, flags: u32) {
    let text = wide(text);
    unsafe { MessageBoxW(owner, text.as_ptr(), w!("AppLocker"), flags) };
}

/// Hands unused memory back to Windows after a dialog closes, keeping idle RAM tiny.
pub fn trim_memory() {
    unsafe { SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX) };
}

/// Pointer to the per-window state stored in `GWLP_USERDATA` (null if not set yet).
///
/// # Safety
/// The stored pointer must point to a live `T`.
pub unsafe fn state<'a, T>(hwnd: HWND) -> Option<&'a mut T> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut T).as_mut()
}

pub fn set_state<T>(hwnd: HWND, st: &mut T) {
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, st as *mut T as isize) };
}
