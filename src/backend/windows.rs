#![allow(unsafe_code)]

use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_QUIT,
};
use windows_sys::core::BOOL;

use crate::backend::Backend;
use crate::backend::hook::{self, Decision, HookInput};
use crate::engine::{Edge, Engine, Event};

struct HookState {
    engine: Mutex<Engine>,
    test_tag: Option<usize>,
}

static STATE: OnceLock<HookState> = OnceLock::new();
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

struct HookGuard(HHOOK);

impl Drop for HookGuard {
    fn drop(&mut self) {
        unsafe { UnhookWindowsHookEx(self.0) };
    }
}

unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> BOOL {
    if matches!(ctrl_type, CTRL_C_EVENT | CTRL_BREAK_EVENT) {
        let thread_id = HOOK_THREAD.load(Ordering::SeqCst);
        if thread_id != 0 {
            unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
        }
        return 1;
    }
    0
}

#[cfg(feature = "testing")]
static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub struct WindowsBackend {
    test_tag: Option<usize>,
}

impl WindowsBackend {
    pub const fn new() -> Self {
        Self { test_tag: None }
    }

    #[cfg(feature = "testing")]
    #[must_use]
    pub const fn with_test_tag(mut self, tag: usize) -> Self {
        self.test_tag = Some(tag);
        self
    }

    #[cfg(feature = "testing")]
    pub fn wait_ready(timeout: std::time::Duration) -> bool {
        use std::time::Instant;

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if READY.load(Ordering::SeqCst) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }
}

impl Default for WindowsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend for WindowsBackend {
    fn run(&mut self, engine: Engine) -> Result<()> {
        STATE.get_or_init(|| HookState {
            engine: Mutex::new(engine),
            test_tag: self.test_tag,
        });
        HOOK_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
        let module = unsafe { GetModuleHandleW(ptr::null()) };
        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0) };
        if hook.is_null() {
            bail!(
                "SetWindowsHookExW failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let hook = HookGuard(hook);
        if unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 1) } == 0 {
            eprintln!(
                "patpans: SetConsoleCtrlHandler failed: {}",
                std::io::Error::last_os_error()
            );
        }
        #[cfg(feature = "testing")]
        READY.store(true, Ordering::SeqCst);
        eprintln!(
            "patpans: low-level keyboard hook installed, toggle: {}",
            toggle_name()
        );
        let mut message = MSG::default();
        loop {
            let result = unsafe { GetMessageW(&raw mut message, ptr::null_mut(), 0, 0) };
            if result <= 0 {
                break;
            }
            unsafe {
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        drop(hook);
        HOOK_THREAD.store(0, Ordering::SeqCst);
        eprintln!("patpans: keyboard hook removed, exiting");
        Ok(())
    }
}

fn toggle_name() -> String {
    STATE
        .get()
        .and_then(|state| state.engine.lock().ok())
        .and_then(|engine| engine.toggle())
        .map_or_else(|| "none".to_string(), |key| key.to_string())
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code < 0 {
        return call_next(code, wparam, lparam);
    }
    let Some(state) = STATE.get() else {
        return call_next(code, wparam, lparam);
    };
    let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    let input = HookInput {
        message: u32::try_from(wparam).unwrap_or_default(),
        vk: info.vkCode,
        injected: info.flags & LLKHF_INJECTED != 0,
        tag: info.dwExtraInfo,
    };
    let Some((key, edge)) = hook::relevant(&input, state.test_tag) else {
        return call_next(code, wparam, lparam);
    };
    let Ok(mut engine) = state.engine.lock() else {
        return call_next(code, wparam, lparam);
    };
    let was_enabled = engine.enabled();
    let decision = hook::decide(&mut engine, key, edge);
    if was_enabled != engine.enabled() {
        let state = if engine.enabled() { "ON" } else { "OFF" };
        eprintln!("patpans: snap tap {state}");
    }
    drop(engine);
    match decision {
        Decision::Pass => call_next(code, wparam, lparam),
        Decision::Swallow => 1,
        Decision::Inject(events) => {
            send_input(&events);
            1
        }
    }
}

fn call_next(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

fn send_input(events: &[Event]) {
    let inputs: Vec<INPUT> = events
        .iter()
        .map(|event| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: event.key.windows_vk,
                    wScan: 0,
                    dwFlags: if event.edge == Edge::Release {
                        KEYEVENTF_KEYUP
                    } else {
                        0
                    },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        })
        .collect();
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}
