//! Finds the role picker in the same desktop installation.

use std::path::PathBuf;

pub fn picker() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    #[cfg(target_os = "macos")]
    {
        let helper = exe.parent()?.parent()?.parent()?;
        let helpers = helper.parent()?;
        if helpers.file_name()? != "Helpers" {
            return None;
        }
        let app = helpers.parent()?.parent()?;
        app.join("Contents/MacOS/freemonplay-screen").is_file().then(|| app.to_path_buf())
    }
    #[cfg(target_os = "windows")]
    {
        let app = exe.with_file_name("freemonplay-screen.exe");
        app.is_file().then_some(app)
    }
}

pub fn open_picker() -> bool {
    let Some(path) = picker() else { return false };
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("/usr/bin/open").arg(path).status().and_then(|status| {
        if status.success() { Ok(()) } else { Err(std::io::Error::other(format!("open exited with {status}"))) }
    });
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new(path).spawn().map(|_| ());
    if let Err(error) = result {
        tracing::error!("couldn't open role picker: {error}");
        return false;
    }
    true
}
