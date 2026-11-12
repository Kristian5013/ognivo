//! What asks ognivo from outside to stop: SIGINT, SIGTERM and SIGQUIT on Unix; on Windows
//! Ctrl+C, Ctrl+Break, and the console closing, the user logging off or the system
//! shutting down.

use std::io;

/// Signals of one kind, received from when it is made until it is dropped.
#[derive(Debug)]
pub struct Signal {
    name: &'static str,
    inner: imp::Signal,
}

/// SIGINT; Ctrl+C on Windows.
pub fn interrupt() -> io::Result<Signal> {
    imp::interrupt().map(|(name, inner)| Signal { name, inner })
}

/// SIGTERM; on Windows the console closing, the user logging off, the system shutting
/// down.
pub fn terminate() -> io::Result<Signal> {
    imp::terminate().map(|(name, inner)| Signal { name, inner })
}

/// SIGQUIT; Ctrl+Break on Windows.
pub fn quit() -> io::Result<Signal> {
    imp::quit().map(|(name, inner)| Signal { name, inner })
}

impl Signal {
    /// The next one; `None` once none can come.
    pub async fn recv(&mut self) -> Option<()> {
        self.inner.recv().await
    }

    /// How the user knows it: `SIGTERM`; `Ctrl+C` on Windows.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

/// The keys that send [`interrupt`] and [`quit`] from a terminal: `^C` and `^\`; on
/// Windows Ctrl+C and Ctrl+Break.
pub const KEYS: (&str, &str) = if cfg!(windows) {
    ("Ctrl+C", "Ctrl+Break")
} else {
    ("^C", "^\\")
};

#[cfg(unix)]
mod imp {
    use tokio::signal::unix::{SignalKind, signal};

    use super::*;

    #[derive(Debug)]
    pub struct Signal(tokio::signal::unix::Signal);

    impl Signal {
        pub async fn recv(&mut self) -> Option<()> {
            self.0.recv().await
        }
    }

    pub fn interrupt() -> io::Result<(&'static str, Signal)> {
        Ok(("SIGINT", Signal(signal(SignalKind::interrupt())?)))
    }

    pub fn terminate() -> io::Result<(&'static str, Signal)> {
        Ok(("SIGTERM", Signal(signal(SignalKind::terminate())?)))
    }

    pub fn quit() -> io::Result<(&'static str, Signal)> {
        Ok(("SIGQUIT", Signal(signal(SignalKind::quit())?)))
    }
}

#[cfg(windows)]
mod imp {
    use tokio::signal::windows::{
        CtrlBreak, CtrlC, CtrlClose, CtrlLogoff, CtrlShutdown, ctrl_break, ctrl_c, ctrl_close,
        ctrl_logoff, ctrl_shutdown,
    };

    use super::*;

    #[derive(Debug)]
    pub enum Signal {
        Interrupt(CtrlC),
        Quit(CtrlBreak),
        End(CtrlClose, CtrlLogoff, CtrlShutdown),
    }

    impl Signal {
        pub async fn recv(&mut self) -> Option<()> {
            match self {
                Self::Interrupt(s) => s.recv().await,
                Self::Quit(s) => s.recv().await,
                Self::End(close, logoff, shutdown) => tokio::select! {
                    r = close.recv() => r,
                    r = logoff.recv() => r,
                    r = shutdown.recv() => r,
                },
            }
        }
    }

    pub fn interrupt() -> io::Result<(&'static str, Signal)> {
        Ok(("Ctrl+C", Signal::Interrupt(ctrl_c()?)))
    }

    pub fn terminate() -> io::Result<(&'static str, Signal)> {
        Ok((
            "its console closing",
            Signal::End(ctrl_close()?, ctrl_logoff()?, ctrl_shutdown()?),
        ))
    }

    pub fn quit() -> io::Result<(&'static str, Signal)> {
        Ok(("Ctrl+Break", Signal::Quit(ctrl_break()?)))
    }
}
