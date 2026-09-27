#![cfg(target_os = "linux")]

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, Device, EventType, InputEvent, KeyCode};

use patpans::backend::Backend;
use patpans::backend::linux::LinuxBackend;
use patpans::engine::{Engine, Group};
use patpans::keys;

const SOURCE_NAME: &str = "socd-e2e-source";
const SINK_NAME: &str = "patpans virtual keyboard";

fn keyset(names: &[&str]) -> AttributeSet<KeyCode> {
    let mut set = AttributeSet::new();
    for name in names {
        set.insert(KeyCode::new(keys::by_name(name).unwrap().linux_code));
    }
    set
}

fn wait_for_device(name: &str, timeout: Duration) -> Option<Device> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        for (_, device) in evdev::enumerate() {
            if device.name() == Some(name) {
                return Some(device);
            }
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

fn default_engine() -> Engine {
    Engine::new(
        vec![
            Group::new(keys::by_name("A").unwrap(), keys::by_name("D").unwrap()),
            Group::new(keys::by_name("W").unwrap(), keys::by_name("S").unwrap()),
        ],
        Some(keys::by_name("F8").unwrap()),
        true,
    )
}

fn managed_keys() -> Vec<patpans::Key> {
    ["A", "D", "W", "S", "F8"]
        .iter()
        .map(|name| keys::by_name(name).unwrap())
        .collect()
}

fn spawn_sink_reader(sink: Device) -> mpsc::Receiver<(u16, i32)> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut sink = sink;
        loop {
            match sink.fetch_events() {
                Ok(events) => {
                    for event in events {
                        let is_key = event.event_type() == EventType::KEY;
                        let is_edge = matches!(event.value(), 0 | 1);
                        if is_key && is_edge && tx.send((event.code(), event.value())).is_err() {
                            return;
                        }
                    }
                }
                Err(_) => return,
            }
        }
    });
    rx
}

fn feed(source: &mut VirtualDevice, script: &[(&str, i32)]) {
    for (name, value) in script {
        let code = keys::by_name(name).unwrap().linux_code;
        source
            .emit(&[InputEvent::new(EventType::KEY.0, code, *value)])
            .unwrap();
        thread::sleep(Duration::from_millis(25));
    }
}

fn collect(
    rx: &mpsc::Receiver<(u16, i32)>,
    expected: &[(u16, i32)],
    timeout: Duration,
) -> Vec<(u16, i32)> {
    let deadline = Instant::now() + timeout;
    let mut got = Vec::new();
    while got.len() < expected.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(event) => got.push(event),
            Err(_) => break,
        }
    }
    got
}

#[test]
fn end_to_end_over_a_virtual_keyboard() {
    if VirtualDevice::builder().is_err() {
        eprintln!("SKIP: /dev/uinput is not available in this environment");
        return;
    }

    let mut source = VirtualDevice::builder()
        .unwrap()
        .name(SOURCE_NAME)
        .with_keys(&keyset(&["A", "D", "W", "S", "F8", "Q"]))
        .unwrap()
        .build()
        .unwrap();

    let (control, done) = spawn_controlled_backend(&managed_keys());

    let sink = wait_for_device(SINK_NAME, Duration::from_secs(15))
        .expect("the patpans virtual keyboard did not appear");
    let rx = spawn_sink_reader(sink);

    let script = [
        ("A", 1),
        ("D", 1),
        ("D", 0),
        ("A", 0),
        ("Q", 1),
        ("Q", 0),
        ("F8", 1),
        ("F8", 0),
        ("W", 1),
        ("S", 1),
        ("F8", 1),
        ("F8", 0),
        ("W", 0),
        ("S", 0),
    ];
    feed(&mut source, &script);

    let expected: Vec<(u16, i32)> = [
        ("A", 1),
        ("A", 0),
        ("D", 1),
        ("D", 0),
        ("A", 1),
        ("A", 0),
        ("Q", 1),
        ("Q", 0),
        ("W", 1),
        ("S", 1),
        ("W", 0),
        ("S", 0),
    ]
    .iter()
    .map(|(name, value)| (keys::by_name(name).unwrap().linux_code, *value))
    .collect();

    let got = collect(&rx, &expected, Duration::from_secs(10));
    assert_eq!(
        got, expected,
        "unexpected event stream from the virtual keyboard"
    );
    stop_backend(&control, &done);
}

fn wait_for_device_gone(name: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let present = evdev::enumerate().any(|(_, device)| device.name() == Some(name));
        if !present {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

fn control_channel() -> (
    patpans::control::Control,
    mpsc::Receiver<patpans::control::Command>,
) {
    let (tx, rx) = mpsc::channel();
    (
        patpans::control::Control::new(
            tx,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ),
        rx,
    )
}

fn spawn_controlled_backend(
    managed: &[patpans::Key],
) -> (
    patpans::control::Control,
    mpsc::Receiver<Result<(), String>>,
) {
    let (control, commands) = control_channel();
    let sender = control.clone();
    let managed = managed.to_vec();
    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut backend = LinuxBackend::new(&managed).with_control(control, commands);
        let result = backend
            .run(default_engine())
            .map_err(|err| format!("{err:#}"));
        let _ = done_tx.send(result);
    });
    (sender, done_rx)
}

fn stop_backend(control: &patpans::control::Control, done: &mpsc::Receiver<Result<(), String>>) {
    assert!(control.send(patpans::control::Command::Stop));
    let result = done
        .recv_timeout(Duration::from_secs(10))
        .expect("the backend must stop");
    assert!(result.is_ok(), "backend stopped with an error: {result:?}");
}

#[test]
fn stop_releases_the_keyboard_and_start_grabs_again() {
    if VirtualDevice::builder().is_err() {
        eprintln!("SKIP: /dev/uinput is not available in this environment");
        return;
    }

    let mut source = VirtualDevice::builder()
        .unwrap()
        .name(SOURCE_NAME)
        .with_keys(&keyset(&["A", "D", "W", "S", "F8", "Q"]))
        .unwrap()
        .build()
        .unwrap();
    let managed = managed_keys();

    let (control, done) = spawn_controlled_backend(&managed);
    assert!(
        wait_for_device(SINK_NAME, Duration::from_secs(15)).is_some(),
        "the patpans virtual keyboard did not appear"
    );
    stop_backend(&control, &done);
    assert!(
        wait_for_device_gone(SINK_NAME, Duration::from_secs(10)),
        "the patpans virtual keyboard must disappear after stop"
    );

    let (control, done) = spawn_controlled_backend(&managed);
    let sink = wait_for_device(SINK_NAME, Duration::from_secs(15))
        .expect("the patpans virtual keyboard did not appear after restart");
    let rx = spawn_sink_reader(sink);
    feed(&mut source, &[("A", 1), ("A", 0)]);
    let expected = vec![
        (keys::by_name("A").unwrap().linux_code, 1),
        (keys::by_name("A").unwrap().linux_code, 0),
    ];
    let got = collect(&rx, &expected, Duration::from_secs(10));
    assert_eq!(
        got, expected,
        "the restarted backend must process input again"
    );
    stop_backend(&control, &done);
}
