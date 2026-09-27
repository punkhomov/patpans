use anyhow::Result;

use crate::engine::Engine;
use crate::keys::Key;

pub mod hook;
pub mod sim;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(windows)]
pub mod windows;

pub trait Backend {
    fn run(&mut self, engine: Engine) -> Result<()>;
}

#[allow(unused_variables)]
pub fn default_backend(managed: &[Key]) -> Result<Box<dyn Backend>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::LinuxBackend::new(managed)))
    }
    #[cfg(windows)]
    {
        Ok(Box::new(windows::WindowsBackend::new()))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        anyhow::bail!("patpans supports Linux and Windows only")
    }
}
