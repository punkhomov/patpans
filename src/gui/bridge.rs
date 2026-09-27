use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::ipc::{Client, Request, StatusInfo};
use crate::paths;

use super::settings::Settings;

pub fn status() -> Option<StatusInfo> {
    let mut client = Client::connect(None).ok()?;
    client.request(&Request::Status).ok()?.status
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

pub fn save_settings(settings: &Settings) -> Result<()> {
    settings.validate()?;
    settings.to_file().save(&config_path())
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
    command.arg("run");
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
#[allow(unsafe_code)]
pub fn start_daemon_elevated() -> Result<()> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }

    let exe = daemon_executable().context("cannot find the patpans executable next to the GUI")?;
    let file = wide(exe.as_os_str());
    let verb = wide(OsStr::new("runas"));
    let params = wide(OsStr::new("run"));
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
