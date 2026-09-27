use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use nix::poll::{PollFd, PollFlags, poll};

use crate::backend::Backend;
use crate::control::{Command, Control};
use crate::engine::{Edge, Engine, Event};
use crate::keys::{self, Key};

pub struct LinuxBackend {
    managed: Vec<Key>,
    tray: bool,
    control: Option<Control>,
    commands: Option<mpsc::Receiver<Command>>,
}

impl LinuxBackend {
    pub fn new(managed: &[Key]) -> Self {
        Self {
            managed: managed.to_vec(),
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

    fn grab_keyboards(
        &self,
        tx: &mpsc::SyncSender<(u16, i32)>,
        capabilities: &mut AttributeSet<KeyCode>,
        stop: &Arc<AtomicBool>,
    ) -> Result<(u32, Vec<JoinHandle<()>>)> {
        let mut devices = 0_u32;
        let mut readers = Vec::new();
        let mut first_error: Option<anyhow::Error> = None;
        for (path, mut device) in evdev::enumerate() {
            if device.name().is_some_and(|name| name.contains("patpans")) {
                continue;
            }
            let Some(supported) = device.supported_keys() else {
                continue;
            };
            if !supported
                .iter()
                .any(|code| self.managed.iter().any(|key| key.linux_code == code.code()))
            {
                continue;
            }
            for code in supported {
                capabilities.insert(code);
            }
            if let Err(err) = device.grab() {
                let context = format!(
                    "failed to grab `{}` ({}) — run as root or add your user to the `input` group",
                    path.display(),
                    device.name().unwrap_or("unnamed")
                );
                crate::log!("patpans: {context}: {err} — skipping this device");
                if first_error.is_none() {
                    first_error = Some(anyhow::Error::new(err).context(context));
                }
                continue;
            }
            let tx = tx.clone();
            let stop = Arc::clone(stop);
            readers.push(thread::spawn(move || {
                read_device(&mut device, &tx, &path, &stop);
            }));
            devices += 1;
        }
        if devices == 0
            && let Some(err) = first_error
        {
            return Err(err);
        }
        Ok((devices, readers))
    }
}

#[expect(unsafe_code, reason = "borrowing the raw fd of a live evdev device")]
fn read_device(
    device: &mut evdev::Device,
    tx: &mpsc::SyncSender<(u16, i32)>,
    path: &Path,
    stop: &AtomicBool,
) {
    let fd = device.as_raw_fd();
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let mut fds = [PollFd::new(
            // SAFETY: the fd stays valid for the whole loop because `device` owns it.
            unsafe { BorrowedFd::borrow_raw(fd) },
            PollFlags::POLLIN,
        )];
        match poll(&mut fds, 100_u16) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(err) => {
                crate::log!("patpans: `{}` poll failed: {err}", path.display());
                return;
            }
        }
        match device.fetch_events() {
            Ok(events) => {
                for event in events {
                    let pending = (event.code(), event.value());
                    loop {
                        match tx.try_send(pending) {
                            Ok(()) => break,
                            Err(mpsc::TrySendError::Full(_)) => {
                                if stop.load(Ordering::SeqCst) {
                                    return;
                                }
                                thread::sleep(Duration::from_millis(10));
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => return,
                        }
                    }
                }
            }
            Err(err) => {
                crate::log!("patpans: `{}` disconnected: {err}", path.display());
                return;
            }
        }
    }
}

struct Runtime {
    engine: Engine,
    capture: Option<mpsc::Sender<Option<Key>>>,
    shutdown: bool,
}

impl Runtime {
    fn new(engine: Engine) -> Self {
        Self {
            engine,
            capture: None,
            shutdown: false,
        }
    }

    fn apply(&mut self, command: Command, control: Option<&Control>) -> Vec<Event> {
        match command {
            Command::Toggle(reply) => {
                let target = !self.engine.enabled();
                let out = self.engine.set_enabled(target);
                reflect(&self.engine, control);
                log_state(&self.engine);
                let _ = reply.send(());
                out
            }
            Command::SetEnabled(value, reply) => {
                let out = self.engine.set_enabled(value);
                reflect(&self.engine, control);
                log_state(&self.engine);
                let _ = reply.send(());
                out
            }
            Command::Replace(config, reply) => {
                let held = self.engine.held_keys();
                let mut engine = Engine::new(config.groups.clone(), config.toggle, config.sticky);
                engine.resync_held(&held);
                self.engine = engine;
                reflect(&self.engine, control);
                crate::log!("patpans: settings applied");
                let _ = reply.send(());
                Vec::new()
            }
            Command::Capture(reply) => {
                self.capture = Some(reply);
                Vec::new()
            }
            Command::CaptureCancel => {
                self.capture = None;
                Vec::new()
            }
            Command::Stop => {
                self.shutdown = true;
                Vec::new()
            }
        }
    }

    fn on_input(&mut self, key: Key, value: i32, control: Option<&Control>) -> Vec<Event> {
        if self.capture.is_some() {
            if value != 0
                && let Some(reply) = self.capture.take()
            {
                let _ = reply.send(Some(key));
            }
            return Vec::new();
        }
        let edge = if value == 0 {
            Edge::Release
        } else {
            Edge::Press
        };
        let was_enabled = self.engine.enabled();
        let out = self.engine.handle(Event::new(key, edge));
        if was_enabled != self.engine.enabled() {
            reflect(&self.engine, control);
            log_state(&self.engine);
        }
        out
    }
}

impl Backend for LinuxBackend {
    fn run(&mut self, engine: Engine) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel::<(u16, i32)>(1024);
        let mut capabilities = AttributeSet::<KeyCode>::new();
        let stop = Arc::new(AtomicBool::new(false));
        let (devices, readers) = self.grab_keyboards(&tx, &mut capabilities, &stop)?;
        drop(tx);

        if devices == 0 {
            bail!(
                "no keyboard with the configured keys found under /dev/input (are you in the `input` group?)"
            );
        }

        for key in &self.managed {
            capabilities.insert(KeyCode::new(key.linux_code));
        }

        let mut virtual_device = VirtualDevice::builder()
            .context("cannot open /dev/uinput — load the `uinput` module and check permissions")?
            .name("patpans virtual keyboard")
            .with_keys(&capabilities)
            .context("failed to configure the virtual keyboard")?
            .build()
            .context("failed to create the virtual keyboard")?;

        if self.tray
            && let Err(err) = crate::tray::start(engine.enabled())
        {
            crate::log!("patpans: tray unavailable: {err:#}");
        }

        let mut runtime = Runtime::new(engine);
        reflect(&runtime.engine, self.control.as_ref());
        let toggle = runtime
            .engine
            .toggle()
            .map_or_else(|| "none".to_string(), |key| key.to_string());
        crate::log!(
            "patpans: snap tap {} — {devices} device(s) grabbed, toggle: {toggle}",
            state_label(runtime.engine.enabled())
        );

        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((code, value)) => {
                    let key =
                        keys::by_linux_code(code).unwrap_or_else(|| keys::unknown_linux(code));
                    let out = runtime.on_input(key, value, self.control.as_ref());
                    emit(&mut virtual_device, &out);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if let Some(commands) = &self.commands {
                while let Ok(command) = commands.try_recv() {
                    let out = runtime.apply(command, self.control.as_ref());
                    emit(&mut virtual_device, &out);
                    if runtime.shutdown {
                        break;
                    }
                }
            }
            if !runtime.shutdown {
                while let Some(name) = crate::tray::poll_menu_command() {
                    if let Some(command) = tray_command(&name) {
                        let out = runtime.apply(command, self.control.as_ref());
                        emit(&mut virtual_device, &out);
                        if runtime.shutdown {
                            break;
                        }
                    }
                }
            }
            if runtime.shutdown {
                break;
            }
        }

        stop.store(true, Ordering::SeqCst);
        for reader in readers {
            let _ = reader.join();
        }
        crate::tray::shutdown();
        reflect(&runtime.engine, self.control.as_ref());
        Ok(())
    }
}

fn tray_command(name: &str) -> Option<Command> {
    match name {
        "toggle" => {
            let (reply, _) = mpsc::channel();
            Some(Command::Toggle(reply))
        }
        "quit" => Some(Command::Stop),
        _ => None,
    }
}

fn reflect(engine: &Engine, control: Option<&Control>) {
    crate::tray::reflect(engine.enabled());
    if let Some(control) = control {
        control.set_status(engine.enabled());
    }
}

fn log_state(engine: &Engine) {
    crate::log!("patpans: snap tap {}", state_label(engine.enabled()));
}

const fn state_label(enabled: bool) -> &'static str {
    if enabled { "ON" } else { "OFF" }
}

fn emit(virtual_device: &mut VirtualDevice, events: &[Event]) {
    if events.is_empty() {
        return;
    }
    let inputs: Vec<InputEvent> = events
        .iter()
        .map(|event| {
            let value = i32::from(event.edge == Edge::Press);
            InputEvent::new(EventType::KEY.0, event.key.linux_code, value)
        })
        .collect();
    if let Err(err) = virtual_device.emit(&inputs) {
        crate::log!("patpans: failed to emit {} event(s): {err}", inputs.len());
    }
}
