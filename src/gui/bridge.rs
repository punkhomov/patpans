use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::ipc::{Client, Request, StatusInfo};
use crate::paths;

use super::settings::Settings;

const STATUS_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Debug)]
pub enum StatusError {
    Unavailable(String),
    Protocol(anyhow::Error),
}

impl StatusError {
    pub fn message(&self) -> String {
        match self {
            Self::Unavailable(cause) => format!("daemon is not running ({cause})"),
            Self::Protocol(err) => format!("daemon protocol error: {err:#}"),
        }
    }
}

pub fn status() -> Result<StatusInfo, StatusError> {
    let mut client = Client::connect_with_timeout(None, STATUS_TIMEOUT)
        .map_err(|err| StatusError::Unavailable(err.root_cause().to_string()))?;
    let response = client
        .request(&Request::Status)
        .map_err(StatusError::Protocol)?;
    if !response.ok {
        return Err(StatusError::Protocol(anyhow::anyhow!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "the daemon rejected the status request".to_string())
        )));
    }
    response
        .status
        .ok_or_else(|| StatusError::Protocol(anyhow::anyhow!("the daemon returned no status")))
}

pub fn toggle() -> Result<StatusInfo> {
    request_status(&Request::Toggle)
}

pub fn reload() -> Result<StatusInfo> {
    request_status(&Request::Reload)
}

pub fn stop() -> Result<()> {
    let mut client = Client::connect(None)?;
    let response = client.request(&Request::Stop)?;
    if response.ok {
        Ok(())
    } else {
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "the daemon rejected the request".to_string())
        )
    }
}

pub fn capture_key() -> Result<Option<String>> {
    let mut client = Client::connect(None)?;
    let response = client.request(&Request::CaptureKey)?;
    if !response.ok {
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "the daemon rejected the request".to_string())
        );
    }
    Ok(response.key)
}

fn request_status(request: &Request) -> Result<StatusInfo> {
    let mut client = Client::connect(None)?;
    let response = client.request(request)?;
    if !response.ok {
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "the daemon rejected the request".to_string())
        );
    }
    response.status.context("the daemon returned no status")
}

pub fn config_path() -> PathBuf {
    paths::default_config_path()
}

pub fn save_settings(settings: &Settings, path: &Path) -> Result<()> {
    settings.validate()?;
    settings.to_file().save(path)
}

fn endpoint() -> Option<String> {
    std::env::var("PAT_PANS_PIPE")
        .or_else(|_| std::env::var("PAT_PANS_SOCKET"))
        .ok()
}

pub fn daemon_executable() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "patpans.exe"
    } else {
        "patpans"
    };
    let candidate = std::env::current_exe().ok()?.with_file_name(name);
    candidate.exists().then_some(candidate)
}

pub fn start_daemon() -> Result<()> {
    let exe = daemon_executable().context("cannot find the patpans executable next to the GUI")?;
    let mut command = Command::new(exe);
    command.arg("run").arg("--config").arg(config_path());
    if let Some(endpoint) = endpoint() {
        command.arg("--socket").arg(endpoint);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
        .spawn()
        .context("failed to start the patpans daemon")?;
    Ok(())
}

#[cfg(windows)]
#[expect(
    unsafe_code,
    reason = "ShellExecuteW is the only way to trigger a UAC elevation"
)]
pub fn start_daemon_elevated() -> Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    let exe = daemon_executable().context("cannot find the patpans executable next to the GUI")?;
    let mut params = format!("run --config \"{}\"", config_path().display());
    if let Some(endpoint) = endpoint() {
        use std::fmt::Write as _;

        let _ = write!(params, " --socket \"{endpoint}\"");
    }
    let file = wide(exe.as_os_str());
    let verb = wide(OsStr::new("runas"));
    let params = wide(OsStr::new(&params));
    // SAFETY: all pointers are null-terminated UTF-16 buffers that outlive the call;
    // a null window handle and SW_SHOWNORMAL are valid for ShellExecuteW.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.addr() <= 32 {
        bail!("failed to start the daemon as administrator (UAC declined?)");
    }
    Ok(())
}
