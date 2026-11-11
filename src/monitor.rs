//! Background watcher: finds newly started locked apps, suspends them, and asks the
//! UI thread for the password.
//!
//! Event-driven, so it costs ~nothing while idle:
//! - A WinEvent hook (`EVENT_OBJECT_CREATE`) reports every new window; we check only that
//!   window's process.
//! - Locked apps' processes are tracked by handle, and the thread sleeps in
//!   `MsgWaitForMultipleObjects` until one exits, so re-locking happens instantly.
//! - A full process scan runs only every few seconds as a safety net (windowless processes),
//!   and is skipped entirely when no apps are locked.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::mem::{size_of, transmute, zeroed};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use windows_sys::w;
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
    WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SUSPEND_RESUME,
    PROCESS_TERMINATE,
};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetWindowThreadProcessId, MsgWaitForMultipleObjects, PeekMessageW,
    PostMessageW, PostThreadMessageW, TranslateMessage, MSG, PM_NOREMOVE, PM_REMOVE, QS_ALLINPUT,
    WM_APP,
};

use crate::config::LockedApp;
use crate::win::from_wide;

/// Posted to the main window when a locked app is waiting for its password.
pub const WM_APP_LOCKED: u32 = WM_APP + 2;
/// Posted to the watcher thread to make it re-read the locked list right away.
const WM_WATCHER_WAKE: u32 = WM_APP + 10;
/// Safety-net full scan interval.
const SAFETY_SCAN: Duration = Duration::from_secs(5);
/// How long a "not a locked app" answer for a PID is trusted (skips repeat lookups
/// while an app creates many windows at once).
const NEGATIVE_CACHE: Duration = Duration::from_secs(2);
/// `MsgWaitForMultipleObjects` limit (64 minus one slot for the message queue).
const MAX_WAIT: usize = 63;
const SYNCHRONIZE: u32 = 0x0010_0000;
const EVENT_OBJECT_CREATE: u32 = 0x8000;
const WINEVENT_OUTOFCONTEXT: u32 = 0x0000;
const WINEVENT_SKIPOWNPROCESS: u32 = 0x0002;
const OBJID_WINDOW: i32 = 0;
const CHILDID_SELF: i32 = 0;

enum AppState {
    /// Not running, or running but not yet unlocked.
    Locked,
    /// Suspended, waiting for the password prompt. Holds the suspended PIDs.
    Pending(Vec<u32>),
    /// Password entered. Stays unlocked until every process of the app exits.
    Unlocked,
}

/// State shared between the watcher thread and the UI thread.
#[derive(Default)]
struct Shared {
    locked: HashSet<String>,
    states: HashMap<String, AppState>,
    queue: VecDeque<String>,
    /// Re-check all running processes (after the locked list changed). Apps that are
    /// already running at that moment count as unlocked.
    rescan: bool,
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
static WATCHER_TID: AtomicU32 = AtomicU32::new(0);

fn shared() -> MutexGuard<'static, Shared> {
    SHARED
        .get_or_init(|| Mutex::new(Shared::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------------------
// API used by the UI thread
// ---------------------------------------------------------------------------------------

/// Sets which exe names are locked. Call at startup and whenever Settings changes the list.
pub fn set_locked(apps: &[LockedApp]) {
    let new: HashSet<String> = apps.iter().map(|a| a.name.clone()).collect();
    {
        let mut s = shared();
        let removed: Vec<String> = s.states.keys().filter(|k| !new.contains(*k)).cloned().collect();
        for key in removed {
            if let Some(AppState::Pending(pids)) = s.states.remove(&key) {
                pids.into_iter().for_each(resume);
            }
        }
        s.queue.retain(|k| new.contains(k));
        s.locked = new;
        s.rescan = true;
    }
    let tid = WATCHER_TID.load(Ordering::Acquire);
    if tid != 0 {
        unsafe { PostThreadMessageW(tid, WM_WATCHER_WAKE, 0, 0) };
    }
}

/// Next app that is suspended and waiting for the password.
pub fn next_pending() -> Option<String> {
    let mut s = shared();
    while let Some(key) = s.queue.pop_front() {
        if matches!(s.states.get(&key), Some(AppState::Pending(_))) {
            return Some(key);
        }
    }
    None
}

/// Applies the prompt result: resume the app, or kill it.
pub fn resolve(key: &str, unlocked: bool) {
    let mut s = shared();
    let Some(state) = s.states.get_mut(key) else { return };
    if let AppState::Pending(pids) = std::mem::replace(state, AppState::Locked) {
        for pid in pids {
            if unlocked {
                resume(pid);
            } else {
                terminate(pid);
            }
        }
        if unlocked {
            *state = AppState::Unlocked;
        }
    }
}

/// Resumes anything still suspended (used on exit so nothing stays frozen).
pub fn release_all() {
    let mut s = shared();
    for state in s.states.values_mut() {
        if let AppState::Pending(pids) = std::mem::replace(state, AppState::Locked) {
            pids.into_iter().for_each(resume);
        }
    }
}

pub fn start(ui: HWND) {
    let ui = ui as usize;
    std::thread::Builder::new()
        .name("watcher".into())
        .stack_size(256 * 1024)
        .spawn(move || run(ui as HWND))
        .expect("failed to start watcher thread");
}

// ---------------------------------------------------------------------------------------
// Watcher thread
// ---------------------------------------------------------------------------------------

struct Tracked {
    pid: u32,
    key: String,
    /// Kept open so the PID can't be reused and so we can wait for the exit.
    handle: HANDLE,
}

struct Watcher {
    ui: HWND,
    me: u32,
    tracked: Vec<Tracked>,
    not_locked: HashMap<u32, Instant>,
    last_scan: Instant,
}

thread_local! {
    static WATCHER: RefCell<Option<Watcher>> = const { RefCell::new(None) };
}

fn with_watcher(f: impl FnOnce(&mut Watcher)) {
    WATCHER.with(|w| {
        if let Some(w) = w.borrow_mut().as_mut() {
            f(w);
        }
    });
}

fn run(ui: HWND) {
    let mut msg: MSG = unsafe { zeroed() };
    // SAFETY: creates this thread's message queue, then installs an out-of-context hook
    // whose callbacks are delivered through that queue.
    unsafe {
        PeekMessageW(&mut msg, null_mut(), 0, 0, PM_NOREMOVE);
        WATCHER_TID.store(GetCurrentThreadId(), Ordering::Release);
        SetWinEventHook(
            EVENT_OBJECT_CREATE,
            EVENT_OBJECT_CREATE,
            null_mut(),
            Some(on_win_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        );
    }
    WATCHER.with(|w| {
        *w.borrow_mut() = Some(Watcher {
            ui,
            me: std::process::id(),
            tracked: Vec::new(),
            not_locked: HashMap::new(),
            last_scan: Instant::now(),
        })
    });
    with_watcher(|w| w.scan());

    let mut handles: Vec<HANDLE> = Vec::with_capacity(MAX_WAIT);
    loop {
        // Window events wake this loop constantly, so the safety-scan deadline is tracked
        // separately instead of relying on the wait timeout (which restarts on every wake).
        let mut since_scan = Duration::ZERO;
        handles.clear();
        with_watcher(|w| {
            handles.extend(w.tracked.iter().take(MAX_WAIT).map(|t| t.handle));
            since_scan = w.last_scan.elapsed();
        });
        if since_scan >= SAFETY_SCAN {
            with_watcher(|w| w.scan());
            continue;
        }
        let timeout = (SAFETY_SCAN - since_scan).as_millis() as u32 + 1;
        let n = handles.len() as u32;
        // SAFETY: the handles stay open until the watcher closes them after this wait.
        let r = unsafe { MsgWaitForMultipleObjects(n, handles.as_ptr(), 0, timeout, QS_ALLINPUT) };
        if r == WAIT_TIMEOUT {
            // The deadline check at the top of the loop runs the scan.
        } else if r < WAIT_OBJECT_0 + n {
            with_watcher(|w| w.sweep());
        } else if r == WAIT_OBJECT_0 + n {
            // Pumping messages runs `on_win_event` for queued window events.
            let mut wake = false;
            // SAFETY: standard message pump.
            unsafe {
                while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    if msg.message == WM_WATCHER_WAKE {
                        wake = true;
                    } else {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
            if wake {
                with_watcher(|w| w.scan());
            }
        } else {
            // WAIT_FAILED should not happen; back off and resync.
            std::thread::sleep(Duration::from_millis(250));
            with_watcher(|w| w.scan());
        }
    }
}

unsafe extern "system" fn on_win_event(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != OBJID_WINDOW || id_child != CHILDID_SELF || hwnd.is_null() {
        return;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != 0 {
        with_watcher(|w| w.on_pid(pid));
    }
}

impl Watcher {
    fn is_tracked(&self, pid: u32) -> bool {
        self.tracked.iter().any(|t| t.pid == pid)
    }

    /// A process just created a window.
    fn on_pid(&mut self, pid: u32) {
        if pid == self.me || self.is_tracked(pid) {
            return;
        }
        if let Some(t) = self.not_locked.get(&pid) {
            if t.elapsed() < NEGATIVE_CACHE {
                return;
            }
        }
        let mut guard = shared();
        if guard.locked.is_empty() {
            return;
        }
        if guard.rescan {
            drop(guard);
            self.scan();
            return;
        }
        let Some((handle, name)) = open_with_name(pid) else { return };
        if !guard.locked.contains(&name) {
            unsafe { CloseHandle(handle) };
            if self.not_locked.len() > 512 {
                self.not_locked.retain(|_, t| t.elapsed() < NEGATIVE_CACHE);
            }
            self.not_locked.insert(pid, Instant::now());
            return;
        }
        let prompt = self.track(&mut guard, pid, name, handle, false);
        drop(guard);
        if prompt {
            self.notify_ui();
        }
    }

    /// Full process scan (startup, after the locked list changes, and as a safety net).
    fn scan(&mut self) {
        self.last_scan = Instant::now();
        let mut guard = shared();
        let sh = &mut *guard;
        let initial = std::mem::take(&mut sh.rescan);
        if initial {
            for t in self.tracked.drain(..) {
                unsafe { CloseHandle(t.handle) };
            }
        }
        self.not_locked.clear();
        let mut prompt = false;
        if !sh.locked.is_empty() {
            // SAFETY: snapshot handle is checked and closed; entry has dwSize set.
            unsafe {
                let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
                if snap != INVALID_HANDLE_VALUE {
                    let mut e: PROCESSENTRY32W = zeroed();
                    e.dwSize = size_of::<PROCESSENTRY32W>() as u32;
                    let mut more = Process32FirstW(snap, &mut e) != 0;
                    while more {
                        let pid = e.th32ProcessID;
                        if pid != self.me && !self.is_tracked(pid) {
                            let name = from_wide(&e.szExeFile).to_lowercase();
                            if sh.locked.contains(&name) {
                                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid);
                                if !h.is_null() {
                                    prompt |= self.track(sh, pid, name, h, initial);
                                }
                            }
                        }
                        more = Process32NextW(snap, &mut e) != 0;
                    }
                    CloseHandle(snap);
                }
            }
        }
        self.sweep_locked(sh);
        drop(guard);
        if prompt {
            self.notify_ui();
        }
    }

    /// Starts tracking a locked app's process. Returns true if a prompt is needed.
    fn track(&mut self, sh: &mut Shared, pid: u32, name: String, handle: HANDLE, initial: bool) -> bool {
        self.tracked.push(Tracked { pid, key: name.clone(), handle });
        let state = sh.states.entry(name.clone()).or_insert(AppState::Locked);
        match state {
            AppState::Unlocked => false,
            AppState::Pending(pids) => {
                suspend(pid);
                pids.push(pid);
                false
            }
            AppState::Locked if initial => {
                // Already running when the list changed: leave it alone.
                *state = AppState::Unlocked;
                false
            }
            AppState::Locked => {
                suspend(pid);
                *state = AppState::Pending(vec![pid]);
                sh.queue.push_back(name);
                true
            }
        }
    }

    /// A tracked process exited.
    fn sweep(&mut self) {
        let mut guard = shared();
        self.sweep_locked(&mut guard);
    }

    /// Drops exited processes and re-locks apps that have no processes left.
    fn sweep_locked(&mut self, sh: &mut Shared) {
        self.tracked.retain(|t| {
            // SAFETY: handle is owned by this entry.
            let alive = unsafe { WaitForSingleObject(t.handle, 0) } == WAIT_TIMEOUT;
            if !alive {
                unsafe { CloseHandle(t.handle) };
            }
            alive
        });
        let tracked = &self.tracked;
        for (key, state) in sh.states.iter_mut() {
            let relock = match state {
                AppState::Unlocked => !tracked.iter().any(|t| &t.key == key),
                AppState::Pending(pids) => {
                    pids.retain(|p| tracked.iter().any(|t| t.pid == *p));
                    pids.is_empty()
                }
                AppState::Locked => false,
            };
            if relock {
                *state = AppState::Locked;
            }
        }
    }

    fn notify_ui(&self) {
        unsafe { PostMessageW(self.ui, WM_APP_LOCKED, 0, 0) };
    }
}

/// Opens a process for waiting and returns it with its lower-cased exe file name.
fn open_with_name(pid: u32) -> Option<(HANDLE, String)> {
    // SAFETY: handle is checked, and closed on failure.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        if QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) == 0 {
            CloseHandle(h);
            return None;
        }
        let path = &buf[..len as usize];
        let start = path.iter().rposition(|&c| c == '\\' as u16).map_or(0, |i| i + 1);
        Some((h, String::from_utf16_lossy(&path[start..]).to_lowercase()))
    }
}

// ---------------------------------------------------------------------------------------
// Suspend / resume / kill
// ---------------------------------------------------------------------------------------

type NtProcessFn = unsafe extern "system" fn(HANDLE) -> i32;

fn nt_fn(name: &'static [u8]) -> Option<NtProcessFn> {
    // SAFETY: ntdll is always loaded; the exported symbols have this signature.
    unsafe {
        let ntdll = GetModuleHandleW(w!("ntdll.dll"));
        if ntdll.is_null() {
            return None;
        }
        GetProcAddress(ntdll, name.as_ptr())
            .map(|f| transmute::<unsafe extern "system" fn() -> isize, NtProcessFn>(f))
    }
}

fn nt_fns() -> (Option<NtProcessFn>, Option<NtProcessFn>) {
    static NT: OnceLock<(Option<NtProcessFn>, Option<NtProcessFn>)> = OnceLock::new();
    *NT.get_or_init(|| (nt_fn(b"NtSuspendProcess\0"), nt_fn(b"NtResumeProcess\0")))
}

fn with_process(pid: u32, access: u32, f: impl FnOnce(HANDLE)) {
    // SAFETY: handle is checked and closed.
    unsafe {
        let h = OpenProcess(access, 0, pid);
        if !h.is_null() {
            f(h);
            CloseHandle(h);
        }
    }
}

fn suspend(pid: u32) {
    if let (Some(f), _) = nt_fns() {
        with_process(pid, PROCESS_SUSPEND_RESUME, |h| unsafe {
            f(h);
        });
    }
}

fn resume(pid: u32) {
    if let (_, Some(f)) = nt_fns() {
        with_process(pid, PROCESS_SUSPEND_RESUME, |h| unsafe {
            f(h);
        });
    }
}

fn terminate(pid: u32) {
    with_process(pid, PROCESS_TERMINATE, |h| unsafe {
        TerminateProcess(h, 1);
    });
}
