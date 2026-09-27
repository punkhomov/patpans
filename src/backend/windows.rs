#![allow(unsafe_code)]

use std::ptr;
use std::sync::{Mutex, OnceLock};

use anyhow::{Result, bail};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_KEYBOARD_LL,
};

use crate::backend::Backend;
use crate::backend::hook::{self, Decision, HookInput};
use crate::engine::{Edge, Engine, Event};

struct HookState {
    engine: Mutex<Engine>,
    test_tag: Option<usize>,
}

static STATE: OnceLock<HookState> = OnceLock::new();

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
        use std::sync::atomic::Ordering;
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
        let module = unsafe { GetModuleHandleW(ptr::null()) };
        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0) };
        if hook.is_null() {
            bail!(
                "SetWindowsHookExW failed: {}",
                std::io::Error::last_os_error()
            );
        }
        #[cfg(feature = "testing")]
        READY.store(true, std::sync::atomic::Ordering::SeqCst);
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
        unsafe { UnhookWindowsHookEx(hook) };
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
