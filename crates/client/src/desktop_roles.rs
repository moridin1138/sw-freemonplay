//! Starts the host shipped in the same desktop installation.

use std::path::{Path, PathBuf};

pub fn host_app() -> Option<PathBuf> {
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        #[cfg(target_os = "macos")]
        let host = dir.parent()?.join("Helpers/Freemonplay Host.app");
        #[cfg(target_os = "windows")]
        let host = dir.join("freemonplay-host.exe");
        host.exists().then_some(host)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    None
}

pub fn start_host(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/open").arg(path).status()?;
        if !status.success() {
            return Err(std::io::Error::other(format!("open exited with {status}")));
        }
    }
    #[cfg(target_os = "windows")]
    std::process::Command::new(path).spawn()?;
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, path.display().to_string()));
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    Ok(())
}
