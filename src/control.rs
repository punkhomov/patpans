use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};

use crate::config::Config;
use crate::keys::Key;

#[derive(Debug)]
pub enum Command {
    Toggle(mpsc::Sender<()>),
    SetEnabled(bool, mpsc::Sender<()>),
    Replace(Box<Config>, mpsc::Sender<()>),
    Capture(mpsc::Sender<Option<Key>>),
    CaptureCancel,
    Stop,
}

pub struct Control {
    tx: mpsc::Sender<Command>,
    wake: Arc<OnceLock<Box<dyn Fn() + Send + Sync>>>,
    status: Arc<AtomicBool>,
}

impl Clone for Control {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            wake: Arc::clone(&self.wake),
            status: Arc::clone(&self.status),
        }
    }
}

impl Control {
    pub fn new(tx: mpsc::Sender<Command>, status: Arc<AtomicBool>) -> Self {
        Self {
            tx,
            wake: Arc::new(OnceLock::new()),
            status,
        }
    }

    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        let _ = self.wake.set(Box::new(wake));
    }

    pub fn send(&self, command: Command) -> bool {
        let sent = self.tx.send(command).is_ok();
        if sent {
            if let Some(wake) = self.wake.get() {
                wake();
            }
        } else {
            crate::log!("patpans: control command dropped — the backend is not running");
        }
        sent
    }

    pub fn set_status(&self, enabled: bool) {
        self.status.store(enabled, Ordering::SeqCst);
    }

    pub fn enabled(&self) -> bool {
        self.status.load(Ordering::SeqCst)
    }
}
