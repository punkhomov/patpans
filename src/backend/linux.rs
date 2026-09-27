use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};

use crate::backend::Backend;
use crate::engine::{Edge, Engine, Event};
use crate::keys::{self, Key};

pub struct LinuxBackend {
    managed: Vec<Key>,
    tray: bool,
}

impl LinuxBackend {
    pub fn new(managed: &[Key]) -> Self {
        Self {
            managed: managed.to_vec(),
            tray: false,
        }
    }

    #[must_use]
    pub const fn with_tray(mut self, tray: bool) -> Self {
        self.tray = tray;
        self
    }

    fn grab_keyboards(
        &self,
        tx: &mpsc::SyncSender<(u16, i32)>,
        capabilities: &mut AttributeSet<KeyCode>,
    ) -> Result<u32> {
        let mut devices = 0_u32;
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
            device.grab().with_context(|| {
                format!(
                    "failed to grab `{}` ({}) — run as root or add your user to the `input` group",
                    path.display(),
                    device.name().unwrap_or("unnamed")
                )
            })?;
            let tx = tx.clone();
            let path = path.clone();
            thread::spawn(move || {
                loop {
                    match device.fetch_events() {
                        Ok(events) => {
                            for event in events {
                                if tx.send((event.code(), event.value())).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(err) => {
                            eprintln!("patpans: `{}` disconnected: {err}", path.display());
                            return;
                        }
                    }
                }
            });
            devices += 1;
        }
        Ok(devices)
    }
}

impl Backend for LinuxBackend {
    fn run(&mut self, mut engine: Engine) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel::<(u16, i32)>(1024);
        let mut capabilities = AttributeSet::<KeyCode>::new();
        let devices = self.grab_keyboards(&tx, &mut capabilities)?;
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
            eprintln!("patpans: tray unavailable: {err:#}");
        }

        let toggle = engine
            .toggle()
            .map_or_else(|| "none".to_string(), |key| key.to_string());
        eprintln!(
            "patpans: snap tap {} — {devices} device(s) grabbed, toggle: {toggle}",
            state_label(engine.enabled())
        );

        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((code, value)) => {
                    let key =
                        keys::by_linux_code(code).unwrap_or_else(|| keys::unknown_linux(code));
                    let edge = if value == 0 {
                        Edge::Release
                    } else {
                        Edge::Press
                    };
                    let was_enabled = engine.enabled();
                    let out = engine.handle(Event::new(key, edge));
                    if was_enabled != engine.enabled() {
                        crate::tray::reflect(engine.enabled());
                        eprintln!("patpans: snap tap {}", state_label(engine.enabled()));
                    }
                    emit(&mut virtual_device, &out);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            while let Some(command) = crate::tray::poll_menu_command() {
                match command.as_str() {
                    "toggle" => {
                        let target = !engine.enabled();
                        let out = engine.set_enabled(target);
                        crate::tray::reflect(engine.enabled());
                        eprintln!("patpans: snap tap {}", state_label(engine.enabled()));
                        emit(&mut virtual_device, &out);
                    }
                    "quit" => {
                        crate::tray::shutdown();
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }

        crate::tray::shutdown();
        Ok(())
    }
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
        eprintln!("patpans: failed to emit {} event(s): {err}", inputs.len());
    }
}
