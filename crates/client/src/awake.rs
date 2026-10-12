//! Keeps this screen on while it's showing a host: no dimming, screensaver, lock or sleep.
//! Held for the length of a session and released when it ends.

/// Released on drop. On Windows the request belongs to the calling thread, so create and
/// drop it on the same (UI) thread.
pub struct KeepAwake(#[allow(dead_code)] imp::Inner);

impl KeepAwake {
    pub fn start() -> Option<Self> {
        match imp::Inner::start("Showing a screen from another computer (Freemonplay)") {
            Ok(inner) => {
                tracing::info!("keeping this screen awake while connected");
                Some(Self(inner))
            }
            Err(e) => {
                tracing::warn!("couldn't keep the screen awake ({e:#}); it may dim or lock while connected");
                None
            }
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use anyhow::{Result, bail};
    use windows::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState};

    pub struct Inner;

    impl Inner {
        pub fn start(_reason: &str) -> Result<Self> {
            // SAFETY: changes only this thread's power request.
            let previous = unsafe { SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED) };
            if previous.0 == 0 {
                bail!("SetThreadExecutionState failed");
            }
            Ok(Self)
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            // SAFETY: clears this thread's request again.
            unsafe { SetThreadExecutionState(ES_CONTINUOUS) };
        }
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use anyhow::Result;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};

    pub struct Inner(Retained<ProtocolObject<dyn NSObjectProtocol>>);

    impl Inner {
        pub fn start(reason: &str) -> Result<Self> {
            let options = NSActivityOptions::UserInitiated | NSActivityOptions::IdleDisplaySleepDisabled;
            Ok(Self(NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &NSString::from_str(reason))))
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            // SAFETY: ending the activity we began.
            unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    //! Asks the desktop over D-Bus: the freedesktop ScreenSaver interface (KDE, Xfce, Cinnamon
    //! and others), falling back to GNOME's session manager. The inhibit lasts while our D-Bus
    //! connection stays open, and we release it explicitly on drop.

    use anyhow::{Context, Result};
    use zbus::blocking::{Connection, Proxy};

    pub struct Inner {
        connection: Connection,
        release: Release,
    }

    enum Release {
        ScreenSaver(u32),
        Gnome(u32),
    }

    /// GNOME's "idle" inhibit flag (don't blank the screen or mark the session idle).
    const GNOME_INHIBIT_IDLE: u32 = 8;

    impl Inner {
        pub fn start(reason: &str) -> Result<Self> {
            let connection = Connection::session().context("no D-Bus session bus")?;
            let screensaver = || -> zbus::Result<u32> {
                let proxy = Proxy::new(
                    &connection,
                    "org.freedesktop.ScreenSaver",
                    "/org/freedesktop/ScreenSaver",
                    "org.freedesktop.ScreenSaver",
                )?;
                proxy.call("Inhibit", &("Freemonplay Screen", reason))
            };
            let gnome = || -> zbus::Result<u32> {
                let proxy = Proxy::new(
                    &connection,
                    "org.gnome.SessionManager",
                    "/org/gnome/SessionManager",
                    "org.gnome.SessionManager",
                )?;
                proxy.call("Inhibit", &("Freemonplay Screen", 0u32, reason, GNOME_INHIBIT_IDLE))
            };
            let release = match screensaver() {
                Ok(cookie) => Release::ScreenSaver(cookie),
                Err(first) => {
                    Release::Gnome(gnome().with_context(|| format!("no screensaver inhibit service ({first})"))?)
                }
            };
            Ok(Self { connection, release })
        }
    }

    impl Drop for Inner {
        fn drop(&mut self) {
            let result = match self.release {
                Release::ScreenSaver(cookie) => Proxy::new(
                    &self.connection,
                    "org.freedesktop.ScreenSaver",
                    "/org/freedesktop/ScreenSaver",
                    "org.freedesktop.ScreenSaver",
                )
                .and_then(|p| p.call::<_, _, ()>("UnInhibit", &(cookie,))),
                Release::Gnome(cookie) => Proxy::new(
                    &self.connection,
                    "org.gnome.SessionManager",
                    "/org/gnome/SessionManager",
                    "org.gnome.SessionManager",
                )
                .and_then(|p| p.call::<_, _, ()>("Uninhibit", &(cookie,))),
            };
            if let Err(e) = result {
                tracing::debug!("releasing the screen inhibit: {e}");
            }
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod imp {
    use anyhow::{Result, bail};

    pub struct Inner;

    impl Inner {
        pub fn start(_reason: &str) -> Result<Self> {
            bail!("not supported on this OS")
        }
    }
}
