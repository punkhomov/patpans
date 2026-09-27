use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result, bail};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};

use crate::backend::Backend;
use crate::engine::{Edge, Engine, Event};
use crate::keys::{self, Key};

pub struct LinuxBackend {
    managed: Vec<Key>,
}

impl LinuxBackend {
    pub fn new(managed: &[Key]) -> Self {
        Self {
            managed: managed.to_vec(),
        }
    }
}

impl Backend for LinuxBackend {
    fn run(&mut self, mut engine: Engine) -> Result<()> {
        let (tx, rx) = mpsc::channel::<(u16, i32)>();
        let mut capabilities = AttributeSet::<KeyCode>::new();
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

        let toggle = engine
            .toggle()
            .map_or_else(|| "none".to_string(), |key| key.to_string());
        let state = if engine.enabled() { "ON" } else { "OFF" };
        eprintln!("patpans: snap tap {state} — {devices} device(s) grabbed, toggle: {toggle}");

        while let Ok((code, value)) = rx.recv() {
            let key = keys::by_linux_code(code).unwrap_or_else(|| keys::unknown_linux(code));
            let edge = if value == 0 {
                Edge::Release
            } else {
                Edge::Press
            };
            let was_enabled = engine.enabled();
            let out = engine.handle(Event::new(key, edge));
            if was_enabled != engine.enabled() {
                let state = if engine.enabled() { "ON" } else { "OFF" };
                eprintln!("patpans: snap tap {state}");
            }
            let mut inputs = Vec::with_capacity(out.len());
            for event in out {
                let value = i32::from(event.edge == Edge::Press);
                inputs.push(InputEvent::new(
                    EventType::KEY.0,
                    event.key.linux_code,
                    value,
                ));
            }
            if !inputs.is_empty() {
                if let Err(err) = virtual_device.emit(&inputs) {
                    eprintln!("patpans: failed to emit {} event(s): {err}", inputs.len());
                }
            }
        }

        Ok(())
    }
}
