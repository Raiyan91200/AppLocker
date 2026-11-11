//! AppLocker: locks chosen apps behind a master password.
//!
//! One small process: a hidden window + tray icon on the main thread, and a watcher
//! thread that suspends locked apps until the password is entered.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod config;
mod crypto;
mod monitor;
mod startup;
mod tray;
mod ui;
mod win;

use std::cell::Cell;
use std::mem::zeroed;
use std::ptr::{null, null_mut};

use windows_sys::w;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// wParam = 1 opens Settings without asking for the password (right after first setup).
const WM_APP_SHOW_SETTINGS: u32 = WM_APP + 3;

thread_local! {
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static PROMPTING: Cell<bool> = const { Cell::new(false) };
    static VERIFYING: Cell<bool> = const { Cell::new(false) };
}

fn main_class() -> *const u16 {
    w!("AppLockerMain")
}

fn main() {
    let background = std::env::args().skip(1).any(|a| a.eq_ignore_ascii_case("--background"));

    // Single instance: a second launch just asks the running one to open Settings.
    // SAFETY: plain Win32 calls; the mutex handle lives until the process exits.
    unsafe {
        let _instance = CreateMutexW(null(), 0, w!("Local\\AppLocker.SingleInstance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if !background {
                let other = FindWindowW(main_class(), null());
                if !other.is_null() {
                    AllowSetForegroundWindow(ASFW_ANY);
                    PostMessageW(other, WM_APP_SHOW_SETTINGS, 0, 0);
                }
            }
            return;
        }
    }

    ui::init();

    let first_run = match config::load() {
        Some(cfg) if !cfg.password_hash.is_empty() => {
            config::set_current(cfg);
            false
        }
        _ => {
            let Some(pw) = ui::password::new_password(
                "Welcome to AppLocker",
                "Create a master password.\nYou'll need it to open locked apps.",
            ) else {
                return;
            };
            let cfg = config::Config { password_hash: crypto::hash_password(&pw), apps: Vec::new() };
            if let Err(e) = config::save(&cfg) {
                ui::message_box(null_mut(), &format!("Couldn't save settings:\n{e}"), MB_ICONERROR);
                return;
            }
            config::set_current(cfg);
            startup::set(true);
            true
        }
    };
    if startup::is_enabled() {
        startup::set(true); // keep the path current if the exe was moved
    }
    monitor::set_locked(&config::current().apps);

    // SAFETY: standard hidden-window + message-loop setup.
    unsafe {
        ui::register_class(main_class(), main_proc);
        let hwnd = CreateWindowExW(
            0,
            main_class(),
            w!("AppLocker"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            ui::hinst(),
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        TASKBAR_CREATED.set(RegisterWindowMessageW(w!("TaskbarCreated")));
        tray::add(hwnd);
        monitor::start(hwnd);

        if first_run {
            tray::notify(hwnd, "AppLocker is running", "It lives here in the tray. Add apps to lock in Settings.");
            PostMessageW(hwnd, WM_APP_SHOW_SETTINGS, 1, 0);
        } else if !background {
            PostMessageW(hwnd, WM_APP_SHOW_SETTINGS, 0, 0);
        }
        ui::trim_memory();

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        tray::remove(hwnd);
    }
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        tray::WM_APP_TRAY => {
            match (lp & 0xFFFF) as u32 {
                WM_LBUTTONDBLCLK => open_settings(false),
                WM_RBUTTONUP | WM_CONTEXTMENU => match tray::menu(hwnd) {
                    tray::MENU_SETTINGS => open_settings(false),
                    tray::MENU_EXIT => exit(hwnd),
                    _ => {}
                },
                _ => {}
            }
            0
        }
        monitor::WM_APP_LOCKED => {
            handle_locked();
            0
        }
        WM_APP_SHOW_SETTINGS => {
            open_settings(wp == 1);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        m if m != 0 && m == TASKBAR_CREATED.get() => {
            // Explorer restarted: put the tray icon back.
            tray::add(hwnd);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

/// Shows the unlock prompt for every app waiting in the queue, one at a time.
fn handle_locked() {
    if PROMPTING.get() {
        return; // the running loop below will pick it up
    }
    PROMPTING.set(true);
    let hash = config::current().password_hash;
    while let Some(app) = monitor::next_pending() {
        let ok = ui::password::unlock(&app, &hash);
        monitor::resolve(&app, ok);
    }
    PROMPTING.set(false);
    ui::trim_memory();
}

fn verify_master() -> bool {
    if VERIFYING.get() {
        return false;
    }
    VERIFYING.set(true);
    let ok = ui::password::verify(&config::current().password_hash);
    VERIFYING.set(false);
    ok
}

fn open_settings(skip_password: bool) {
    if ui::settings::is_open() {
        ui::settings::show();
        return;
    }
    if skip_password || verify_master() {
        ui::settings::show();
    }
    ui::trim_memory();
}

fn exit(hwnd: HWND) {
    if PROMPTING.get() {
        return;
    }
    if verify_master() {
        monitor::release_all();
        unsafe { DestroyWindow(hwnd) };
    }
}
