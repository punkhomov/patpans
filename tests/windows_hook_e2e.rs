#![cfg(all(windows, feature = "testing"))]
#![allow(unsafe_code)]

use std::mem::size_of;
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, HHOOK, HOOKPROC, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    PM_REMOVE, PeekMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
};

use patpans::backend::Backend;
use patpans::backend::windows::WindowsBackend;
use patpans::engine::{Engine, Group};
use patpans::keys;

const TEST_TAG: usize = 0x5041_5450_414E_5753;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Record {
    message: u32,
    vk: u32,
    injected: bool,
    tag: usize,
}

static HEAD: OnceLock<Mutex<Vec<Record>>> = OnceLock::new();
static TAIL: OnceLock<Mutex<Vec<Record>>> = OnceLock::new();

fn push(slot: &OnceLock<Mutex<Vec<Record>>>, lparam: LPARAM, wparam: WPARAM) {
    let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
    if let Some(records) = slot.get() {
        if let Ok(mut records) = records.lock() {
            records.push(Record {
                message: u32::try_from(wparam).unwrap_or_default(),
                vk: info.vkCode,
                injected: info.flags & LLKHF_INJECTED != 0,
                tag: info.dwExtraInfo,
            });
        }
    }
}

unsafe extern "system" fn head_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        push(&HEAD, lparam, wparam);
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

unsafe extern "system" fn tail_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code >= 0 {
        push(&TAIL, lparam, wparam);
        let info = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        if info.flags & LLKHF_INJECTED != 0 {
            return 1;
        }
    }
    unsafe { CallNextHookEx(ptr::null_mut(), code, wparam, lparam) }
}

fn install(proc: HOOKPROC) -> HHOOK {
    let module = unsafe { GetModuleHandleW(ptr::null()) };
    let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, proc, module, 0) };
    assert!(
        !hook.is_null(),
        "SetWindowsHookExW failed: {}",
        std::io::Error::last_os_error()
    );
    hook
}

fn send_key(vk: u16, release: bool) {
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if release { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: TEST_TAG,
            },
        },
    };
    let size = i32::try_from(size_of::<INPUT>()).unwrap();
    let sent = unsafe { SendInput(1, &raw const input, size) };
    assert_eq!(
        sent,
        1,
        "SendInput failed: {}",
        std::io::Error::last_os_error()
    );
}

fn pump_until(deadline: Instant, done: impl Fn() -> bool) {
    let mut message = MSG::default();
    loop {
        while unsafe { PeekMessageW(&raw mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            unsafe {
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }
        if done() || Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

fn records(slot: &OnceLock<Mutex<Vec<Record>>>) -> Vec<Record> {
    slot.get()
        .and_then(|records| records.lock().ok().map(|records| records.clone()))
        .unwrap_or_default()
}

const fn is_key(message: u32) -> bool {
    matches!(message, WM_KEYDOWN | WM_KEYUP | 0x0104 | 0x0105)
}

fn rec(message: u32, name: &str, tag: usize) -> Record {
    Record {
        message,
        vk: u32::from(keys::by_name(name).unwrap().windows_vk),
        injected: true,
        tag,
    }
}

#[test]
fn end_to_end_through_the_keyboard_hook() {
    let _ = HEAD.set(Mutex::new(Vec::new()));
    let _ = TAIL.set(Mutex::new(Vec::new()));

    let tail_hook = install(Some(tail_proc));

    let engine = Engine::new(
        vec![
            Group::new(keys::by_name("A").unwrap(), keys::by_name("D").unwrap()),
            Group::new(keys::by_name("W").unwrap(), keys::by_name("S").unwrap()),
        ],
        Some(keys::by_name("F8").unwrap()),
        true,
    );
    thread::spawn(move || {
        let mut backend = WindowsBackend::new().with_test_tag(TEST_TAG);
        if let Err(err) = backend.run(engine) {
            eprintln!("patpans backend stopped: {err:#}");
        }
    });
    assert!(
        WindowsBackend::wait_ready(Duration::from_secs(10)),
        "the patpans hook was not installed"
    );

    let head_hook = install(Some(head_proc));

    let script: [(&str, bool); 14] = [
        ("A", false),
        ("D", false),
        ("D", true),
        ("A", true),
        ("Q", false),
        ("Q", true),
        ("F8", false),
        ("F8", true),
        ("W", false),
        ("S", false),
        ("F8", false),
        ("F8", true),
        ("W", true),
        ("S", true),
    ];
    for (name, release) in script {
        send_key(keys::by_name(name).unwrap().windows_vk, release);
        thread::sleep(Duration::from_millis(25));
    }

    let expected = vec![
        rec(WM_KEYDOWN, "A", TEST_TAG),
        rec(WM_KEYUP, "A", 0),
        rec(WM_KEYDOWN, "D", 0),
        rec(WM_KEYUP, "D", 0),
        rec(WM_KEYDOWN, "A", 0),
        rec(WM_KEYDOWN, "Q", TEST_TAG),
        rec(WM_KEYUP, "Q", TEST_TAG),
        rec(WM_KEYDOWN, "W", TEST_TAG),
        rec(WM_KEYDOWN, "S", TEST_TAG),
        rec(WM_KEYUP, "S", TEST_TAG),
    ];

    let deadline = Instant::now() + Duration::from_secs(15);
    pump_until(deadline, || records(&TAIL).len() >= expected.len());

    let head = records(&HEAD);
    let tail: Vec<Record> = records(&TAIL)
        .into_iter()
        .filter(|record| is_key(record.message))
        .collect();
    unsafe {
        UnhookWindowsHookEx(head_hook);
        UnhookWindowsHookEx(tail_hook);
    }

    assert_eq!(
        tail, expected,
        "unexpected stream reached the end of the hook chain"
    );

    let tagged = head
        .iter()
        .filter(|record| is_key(record.message) && record.tag == TEST_TAG)
        .count();
    let injected_by_patpans = head
        .iter()
        .filter(|record| is_key(record.message) && record.injected && record.tag != TEST_TAG)
        .count();
    let expected_injections = expected
        .iter()
        .filter(|record| record.tag != TEST_TAG)
        .count();
    assert_eq!(
        tagged,
        script.len(),
        "the head hook did not see all synthetic inputs"
    );
    assert_eq!(
        injected_by_patpans, expected_injections,
        "unexpected number of patpans injections, possible echo loop"
    );
}
