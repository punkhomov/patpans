use std::io::BufReader;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::config::{Config, FileConfig};
use crate::control::{Command, Control};
use crate::engine::Engine;
use crate::ipc::{self, Request, Response, StatusInfo};
use crate::{log, paths};

pub struct Options {
    pub config_path: PathBuf,
    pub endpoint: Option<String>,
    pub tray: bool,
}

struct Shared {
    control: Control,
    config: Mutex<Config>,
    config_path: PathBuf,
    tray: bool,
    shutdown: mpsc::Sender<()>,
}

pub struct Daemon {
    shared: Arc<Shared>,
    endpoint: Option<String>,
    backend: Option<JoinHandle<Result<()>>>,
    accept: Option<JoinHandle<()>>,
    accept_stop: Arc<AtomicBool>,
    shutdown_rx: mpsc::Receiver<()>,
}

impl Daemon {
    pub fn spawn<F>(config: Config, options: Options, make_backend: F) -> Result<Self>
    where
        F: FnOnce(Control, mpsc::Receiver<Command>) -> Result<Box<dyn Backend>> + Send + 'static,
    {
        let listener = ipc::transport::listen(options.endpoint.as_deref())
            .context("failed to start the daemon IPC endpoint")?;
        let (command_tx, command_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let status = Arc::new(AtomicBool::new(true));
        let control = Control::new(command_tx, status);
        let shared = Arc::new(Shared {
            control: control.clone(),
            config: Mutex::new(config.clone()),
            config_path: options.config_path.clone(),
            tray: options.tray,
            shutdown: shutdown_tx.clone(),
        });

        let engine = Engine::new(config.groups, config.toggle, config.sticky);
        let backend_control = control.clone();
        let backend_shutdown = shutdown_tx;
        let backend = thread::spawn(move || {
            let result = match make_backend(backend_control, command_rx) {
                Ok(mut backend) => backend.run(engine),
                Err(err) => Err(err),
            };
            let _ = backend_shutdown.send(());
            result
        });

        let accept_stop = Arc::new(AtomicBool::new(false));
        let accept_shared = Arc::clone(&shared);
        let accept_stop_thread = Arc::clone(&accept_stop);
        let accept = thread::spawn(move || {
            accept_loop(&listener, &accept_shared, &accept_stop_thread);
        });

        log!(
            "patpans: daemon started (config: {})",
            shared.config_path.display()
        );
        Ok(Self {
            shared,
            endpoint: options.endpoint,
            backend: Some(backend),
            accept: Some(accept),
            accept_stop,
            shutdown_rx,
        })
    }

    pub fn control(&self) -> Control {
        self.shared.control.clone()
    }

    pub fn enabled(&self) -> bool {
        self.shared.control.enabled()
    }

    pub fn wait(mut self) -> Result<()> {
        let _ = self.shutdown_rx.recv();
        self.shutdown()
    }

    pub fn shutdown(&mut self) -> Result<()> {
        self.accept_stop.store(true, Ordering::SeqCst);
        let _ = ipc::transport::connect(self.endpoint.as_deref());
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        self.shared.control.send(Command::Stop);
        if let Some(backend) = self.backend.take() {
            match backend.join() {
                Ok(Ok(())) => {}
                Ok(Err(err)) => log!("patpans: backend stopped with an error: {err:#}"),
                Err(_) => log!("patpans: backend thread panicked"),
            }
        }
        log!("patpans: daemon stopped");
        Ok(())
    }
}

fn accept_loop(listener: &ipc::transport::Listener, shared: &Arc<Shared>, stop: &Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok(stream) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let shared = Arc::clone(shared);
                thread::spawn(move || handle_client(stream, &shared));
            }
            Err(err) => {
                log!("patpans: ipc accept error: {err}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn handle_client(stream: ipc::transport::Stream, shared: &Shared) {
    let mut reader = BufReader::new(stream);
    loop {
        let request = match ipc::read_request(&mut reader) {
            Ok(Some(request)) => request,
            Ok(None) => break,
            Err(err) => {
                let _ = ipc::write_response(reader.get_mut(), &Response::error(format!("{err:#}")));
                break;
            }
        };
        let stop_after = matches!(request, Request::Stop);
        let response = dispatch(&request, shared);
        if ipc::write_response(reader.get_mut(), &response).is_err() {
            break;
        }
        if stop_after {
            break;
        }
    }
}

fn dispatch(request: &Request, shared: &Shared) -> Response {
    match request {
        Request::Ping => Response::ok(),
        Request::Status => Response::with_status(status_info(shared)),
        Request::Toggle => {
            if apply(shared, Command::Toggle) {
                Response::with_status(status_info(shared))
            } else {
                Response::error("the daemon is shutting down")
            }
        }
        Request::SetEnabled { enabled } => {
            let enabled = *enabled;
            if apply(shared, move |reply| Command::SetEnabled(enabled, reply)) {
                Response::with_status(status_info(shared))
            } else {
                Response::error("the daemon is shutting down")
            }
        }
        Request::Reload => match reload(shared) {
            Ok(()) => Response::with_status(status_info(shared)),
            Err(err) => Response::error(format!("{err:#}")),
        },
        Request::CaptureKey => {
            let (reply_tx, reply_rx) = mpsc::channel();
            if !shared.control.send(Command::Capture(reply_tx)) {
                return Response::error("the daemon is shutting down");
            }
            if let Ok(key) = reply_rx.recv_timeout(Duration::from_secs(5)) {
                Response::with_key(key.map(|key| key.name.to_string()))
            } else {
                shared.control.send(Command::CaptureCancel);
                Response::with_key(None)
            }
        }
        Request::Stop => {
            shared.control.send(Command::Stop);
            let _ = shared.shutdown.send(());
            Response::ok()
        }
    }
}

fn apply(shared: &Shared, make: impl FnOnce(mpsc::Sender<()>) -> Command) -> bool {
    let (reply_tx, reply_rx) = mpsc::channel();
    if !shared.control.send(make(reply_tx)) {
        return false;
    }
    reply_rx.recv_timeout(Duration::from_secs(1)).is_ok()
}

fn reload(shared: &Shared) -> Result<()> {
    if !shared.config_path.exists() {
        bail!("config file `{}` not found", shared.config_path.display());
    }
    let file = FileConfig::load(&shared.config_path)?;
    let config = file.into_config()?;
    if let Ok(mut current) = shared.config.lock() {
        *current = config.clone();
    }
    apply(shared, move |reply| {
        Command::Replace(Box::new(config), reply)
    });
    log!(
        "patpans: settings reloaded from {}",
        shared.config_path.display()
    );
    Ok(())
}

fn status_info(shared: &Shared) -> StatusInfo {
    let config = shared
        .config
        .lock()
        .map_or_else(|_| Config::default(), |config| config.clone());
    StatusInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        enabled: shared.control.enabled(),
        elevated: elevated(),
        sticky: config.sticky,
        toggle: config.toggle.map(|key| key.name.to_string()),
        groups: config
            .groups
            .iter()
            .map(|group| {
                [
                    group.keys[0].name.to_string(),
                    group.keys[1].name.to_string(),
                ]
            })
            .collect(),
        tray: shared.tray,
        config_path: shared.config_path.display().to_string(),
    }
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "Win32 token query to report elevation in the daemon status"
)]
fn elevated() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = std::ptr::null_mut();
    // SAFETY: the current process pseudo-handle is always valid and `token` is an out-param.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let size = u32::try_from(std::mem::size_of::<TOKEN_ELEVATION>()).unwrap_or_default();
    let mut returned = 0_u32;
    // SAFETY: `token` is a valid handle from OpenProcessToken and the buffer matches
    // the requested TOKEN_ELEVATION size.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&raw mut elevation).cast(),
            size,
            &raw mut returned,
        )
    };
    // SAFETY: `token` was opened above and is closed exactly once.
    unsafe { CloseHandle(token) };
    ok != 0 && elevation.TokenIsElevated != 0
}

#[cfg(not(windows))]
const fn elevated() -> bool {
    false
}

pub fn default_options(config_path: PathBuf, tray: bool) -> Options {
    Options {
        config_path,
        endpoint: None,
        tray,
    }
}

pub fn log_path_for(config_path: &std::path::Path) -> PathBuf {
    paths::default_log_path(config_path)
}
