#![allow(unsafe_code)]

use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};

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
    PostQuitMessage, PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_QUIT,
};
use windows_sys::core::BOOL;

use crate::backend::Backend;
use crate::backend::hook::{self, Decision, HookInput};
use crate::config::Config;
use crate::control::{Command, Control};
use crate::engine::{Edge, Engine, Event};
use crate::keys::Key;

const WAKE_MESSAGE: u32 = 0x8000 + 3;

struct HookState {
    engine: Mutex<Engine>,
    capture: Mutex<Option<mpsc::Sender<Option<Key>>>>,
    control: Mutex<Option<Control>>,
    test_tag: Option<usize>,
}

static STATE: OnceLock<HookState> = OnceLock::new();
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
static CONSOLE_HANDLER: AtomicBool = AtomicBool::new(false);

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
static READY: AtomicBool = AtomicBool::new(false);

pub struct WindowsBackend {
    test_tag: Option<usize>,
    tray: bool,
    control: Option<Control>,
    commands: Option<mpsc::Receiver<Command>>,
}

impl WindowsBackend {
    pub const fn new() -> Self {
        Self {
            test_tag: None,
            tray: false,
            control: None,
            commands: None,
        }
    }

    #[must_use]
    pub const fn with_tray(mut self, tray: bool) -> Self {
        self.tray = tray;
        self
    }

    #[must_use]
    pub fn with_control(mut self, control: Control, commands: mpsc::Receiver<Command>) -> Self {
        self.control = Some(control);
        self.commands = Some(commands);
        self
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

#[allow(clippy::too_many_lines)]
impl Backend for WindowsBackend {
    fn run(&mut self, engine: Engine) -> Result<()> {
        let enabled = engine.enabled();
        if let Some(state) = STATE.get() {
            let Ok(mut current) = state.engine.lock() else {
                bail!("the engine lock is poisoned");
            };
            *current = engine;
            if let Ok(mut control) = state.control.lock() {
                (*control).clone_from(&self.control);
            }
        } else {
            let _ = STATE.set(HookState {
                engine: Mutex::new(engine),
                capture: Mutex::new(None),
                control: Mutex::new(self.control.clone()),
                test_tag: self.test_tag,
            });
        }
        HOOK_THREAD.store(unsafe { GetCurrentThreadId() }, Ordering::SeqCst);
        if let Some(control) = &self.control {
            control.set_wake(|| {
                let thread_id = HOOK_THREAD.load(Ordering::SeqCst);
                if thread_id != 0 {
                    unsafe { PostThreadMessageW(thread_id, WAKE_MESSAGE, 0, 0) };
                }
            });
            control.set_status(enabled);
        }
        let module = unsafe { GetModuleHandleW(ptr::null()) };
        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0) };
        if hook.is_null() {
            bail!(
                "SetWindowsHookExW failed: {}",
                std::io::Error::last_os_error()
            );
        }
        let hook = HookGuard(hook);
        if !CONSOLE_HANDLER.swap(true, Ordering::SeqCst)
            && unsafe { SetConsoleCtrlHandler(Some(console_ctrl_handler), 1) } == 0
        {
            crate::log!(
                "patpans: SetConsoleCtrlHandler failed: {}",
                std::io::Error::last_os_error()
            );
        }
        #[cfg(feature = "testing")]
        READY.store(true, Ordering::SeqCst);
        if self.tray
            && let Err(err) = crate::tray::start(enabled)
        {
            crate::log!("patpans: tray unavailable: {err:#}");
        }
        crate::log!(
            "patpans: low-level keyboard hook installed, toggle: {}",
            toggle_name()
        );
        let mut message = MSG::default();
        loop {
            let result = unsafe { GetMessageW(&raw mut message, ptr::null_mut(), 0, 0) };
            if result <= 0 {
                break;
            }
            if message.message == WAKE_MESSAGE {
                handle_commands(self.commands.as_ref());
                continue;
            }
            unsafe {
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        crate::tray::shutdown();
        drop(hook);
        HOOK_THREAD.store(0, Ordering::SeqCst);
        crate::log!("patpans: keyboard hook removed, exiting");
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

fn handle_commands(commands: Option<&mpsc::Receiver<Command>>) {
    let Some(commands) = commands else {
        return;
    };
    while let Ok(command) = commands.try_recv() {
        match command {
            Command::Toggle(reply) => apply_enabled(None, &reply),
            Command::SetEnabled(value, reply) => apply_enabled(Some(value), &reply),
            Command::Replace(config, reply) => apply_config(&config, &reply),
            Command::Capture(reply) => set_capture(Some(reply)),
            Command::CaptureCancel => set_capture(None),
            Command::Stop => unsafe { PostQuitMessage(0) },
        }
    }
}

fn apply_enabled(target: Option<bool>, reply: &mpsc::Sender<()>) {
    let Some(state) = STATE.get() else {
        let _ = reply.send(());
        return;
    };
    let Ok(mut engine) = state.engine.lock() else {
        let _ = reply.send(());
        return;
    };
    let enabled = target.unwrap_or(!engine.enabled());
    let out = engine.set_enabled(enabled);
    update_status(state, &engine);
    crate::log!("patpans: snap tap {}", state_label(engine.enabled()));
    drop(engine);
    send_input(&out);
    let _ = reply.send(());
}

fn apply_config(config: &Config, reply: &mpsc::Sender<()>) {
    if let Some(state) = STATE.get()
        && let Ok(mut engine) = state.engine.lock()
    {
        *engine = Engine::new(config.groups.clone(), config.toggle, config.sticky);
        update_status(state, &engine);
    }
    crate::log!("patpans: settings applied");
    let _ = reply.send(());
}

fn set_capture(reply: Option<mpsc::Sender<Option<Key>>>) {
    if let Some(state) = STATE.get()
        && let Ok(mut capture) = state.capture.lock()
    {
        *capture = reply;
    }
}

fn update_status(state: &HookState, engine: &Engine) {
    crate::tray::reflect(engine.enabled());
    if let Ok(control) = state.control.lock()
        && let Some(control) = control.as_ref()
    {
        control.set_status(engine.enabled());
    }
}

const fn state_label(enabled: bool) -> &'static str {
    if enabled { "ON" } else { "OFF" }
}

pub(crate) fn handle_menu_command(command: &str) {
    let Some(state) = STATE.get() else {
        return;
    };
    let Ok(control) = state.control.lock() else {
        return;
    };
    let Some(control) = control.as_ref() else {
        return;
    };
    match command {
        "toggle" => {
            let (reply, _) = mpsc::channel();
            control.send(Command::Toggle(reply));
        }
        "quit" => {
            control.send(Command::Stop);
        }
        _ => {}
    }
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
    if edge == Edge::Press {
        let capture = state.capture.lock().ok().and_then(|mut slot| slot.take());
        if let Some(reply) = capture {
            let _ = reply.send(Some(key));
            return 1;
        }
    }
    let control = state
        .control
        .lock()
        .ok()
        .and_then(|control| control.clone());
    let Ok(mut engine) = state.engine.lock() else {
        return call_next(code, wparam, lparam);
    };
    let was_enabled = engine.enabled();
    let decision = hook::decide(&mut engine, key, edge);
    if was_enabled != engine.enabled() {
        crate::tray::reflect(engine.enabled());
        if let Some(control) = &control {
            control.set_status(engine.enabled());
        }
        crate::log!("patpans: snap tap {}", state_label(engine.enabled()));
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
