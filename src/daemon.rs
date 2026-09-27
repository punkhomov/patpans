use std::io::{self, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::backend::Backend;
use crate::config::{Config, FileConfig};
use crate::control::{Command, Control};
use crate::engine::Engine;
use crate::ipc::{self, Request, Response, StatusInfo};
use crate::log;

const MAX_CLIENTS: usize = 32;
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const REPLY_TIMEOUT: Duration = Duration::from_secs(1);

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
    clients: AtomicUsize,
}

pub struct Daemon {
    shared: Arc<Shared>,
    listener: Arc<ipc::transport::Listener>,
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
        let Options {
            config_path,
            endpoint,
            tray,
        } = options;
        let listener = Arc::new(
            ipc::transport::listen(endpoint.as_deref())
                .context("failed to start the daemon IPC endpoint")?,
        );
        let (command_tx, command_rx) = mpsc::channel();
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let status = Arc::new(AtomicBool::new(true));
        let control = Control::new(command_tx, status);
        let shared = Arc::new(Shared {
            control: control.clone(),
            config: Mutex::new(config.clone()),
            config_path,
            tray,
            shutdown: shutdown_tx.clone(),
            clients: AtomicUsize::new(0),
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
        let accept_listener = Arc::clone(&listener);
        let accept = thread::spawn(move || {
            accept_loop(&accept_listener, &accept_shared, &accept_stop_thread);
        });

        log!(
            "patpans: daemon started (config: {})",
            shared.config_path.display()
        );
        Ok(Self {
            shared,
            listener,
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
        let _ = self.listener.wake();
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
            Ok(mut stream) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if shared.clients.load(Ordering::SeqCst) >= MAX_CLIENTS {
                    let _ = ipc::write_response(
                        &mut stream,
                        &Response::error("too many concurrent IPC clients"),
                    );
                    continue;
                }
                shared.clients.fetch_add(1, Ordering::SeqCst);
                let shared = Arc::clone(shared);
                thread::spawn(move || {
                    handle_client(stream, &shared);
                    shared.clients.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(err) => {
                log!("patpans: ipc accept error: {err}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn handle_client(mut stream: ipc::transport::Stream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(CLIENT_IDLE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));
    let mut reader = BufReader::new(stream);
    loop {
        let envelope = match ipc::read_request(&mut reader) {
            Ok(Some(envelope)) => envelope,
            Ok(None) => break,
            Err(err) if is_timeout(&err) => break,
            Err(err) => {
                let _ = ipc::write_response(reader.get_mut(), &Response::error(format!("{err:#}")));
                break;
            }
        };
        if envelope.version != ipc::PROTOCOL_VERSION {
            let _ = ipc::write_response(
                reader.get_mut(),
                &Response::error(format!(
                    "protocol version mismatch: the client speaks v{} and this daemon speaks v{}",
                    envelope.version,
                    ipc::PROTOCOL_VERSION
                )),
            );
            break;
        }
        let request = envelope.request;
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

fn is_timeout(err: &anyhow::Error) -> bool {
    err.downcast_ref::<io::Error>().is_some_and(|err| {
        matches!(
            err.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        )
    })
}

fn dispatch(request: &Request, shared: &Shared) -> Response {
    match request {
        Request::Ping => Response::ok(),
        Request::Status => Response::with_status(status_info(shared)),
        Request::Toggle => match apply(shared, Command::Toggle) {
            Ok(()) => Response::with_status(status_info(shared)),
            Err(err) => Response::error(err.message()),
        },
        Request::SetEnabled { enabled } => {
            let enabled = *enabled;
            match apply(shared, move |reply| Command::SetEnabled(enabled, reply)) {
                Ok(()) => Response::with_status(status_info(shared)),
                Err(err) => Response::error(err.message()),
            }
        }
        Request::Reload => match reload(shared) {
            Ok(()) => Response::with_status(status_info(shared)),
            Err(err) => Response::error(format!("{err:#}")),
        },
        Request::CaptureKey => {
            let (reply_tx, reply_rx) = mpsc::channel();
            if !shared.control.send(Command::Capture(reply_tx)) {
                return Response::error(ApplyError::ShuttingDown.message());
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

enum ApplyError {
    ShuttingDown,
    Timeout,
}

impl ApplyError {
    const fn message(&self) -> &'static str {
        match self {
            Self::ShuttingDown => "the daemon is shutting down",
            Self::Timeout => "the daemon did not respond in time",
        }
    }
}

fn apply(
    shared: &Shared,
    make: impl FnOnce(mpsc::Sender<()>) -> Command,
) -> Result<(), ApplyError> {
    let (reply_tx, reply_rx) = mpsc::channel();
    if !shared.control.send(make(reply_tx)) {
        return Err(ApplyError::ShuttingDown);
    }
    reply_rx
        .recv_timeout(REPLY_TIMEOUT)
        .map_err(|_| ApplyError::Timeout)
}

fn reload(shared: &Shared) -> Result<()> {
    if !shared.config_path.exists() {
        bail!("config file `{}` not found", shared.config_path.display());
    }
    let file = FileConfig::load(&shared.config_path)?;
    let config = file.into_config()?;
    let next = config.clone();
    apply(shared, move |reply| Command::Replace(Box::new(next), reply))
        .map_err(|err| anyhow::anyhow!("{}; the settings were not applied", err.message()))?;
    if let Ok(mut current) = shared.config.lock() {
        *current = config;
    }
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
