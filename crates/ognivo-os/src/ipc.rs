//! The core's socket (docs/architecture.md §3), the user's alone.
//!
//! - Unix: a socket of mode 0600 in a directory of mode 0700, and every client checked to
//!   be the user, `$XDG_RUNTIME_DIR/ognivo/core.sock` by default.
//! - Windows: a named pipe. Its default name is random and new each time a core starts;
//!   the core writes it to `core.pipe` in its data directory, in the user's profile, for
//!   the window to read. Another user may open a pipe for reading only, never for writing,
//!   and the core says nothing to a client before that client's requests; a process that
//!   takes the name first makes the core fail, not share it.

use std::io;
use std::path::{Path, PathBuf};

/// The core's socket file or pipe, and the file that names it for the window.
#[derive(Debug, Clone)]
pub struct Address {
    path: PathBuf,
    /// `<data dir>/core.pipe`, written once the pipe is there.
    #[cfg(windows)]
    named_in: Option<PathBuf>,
}

impl Address {
    /// Exactly `path`: a socket file, or on Windows a pipe (`\\.\pipe\...`).
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            #[cfg(windows)]
            named_in: None,
        }
    }

    /// The default for the core of `data_dir`: `$XDG_RUNTIME_DIR/ognivo/core.sock`; on
    /// Windows a new pipe name, which [`listen`] writes to `<data_dir>/core.pipe`.
    pub fn default_for(data_dir: &Path) -> Result<Self, String> {
        imp::default_for(data_dir)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Why the core cannot listen.
#[derive(Debug)]
pub enum Listen {
    /// Another core, or another process, is there.
    Taken,
    Failed(io::Error),
}

/// Listens at `address`. A socket file nobody answers at is a dead core's, and goes.
pub fn listen(address: &Address) -> Result<Listener, Listen> {
    imp::listen(address).map(Listener)
}

/// What [`listen`] made.
#[derive(Debug)]
pub struct Listener(imp::Listener);

/// A client of the core, as the core has it.
pub type Connection = imp::Connection;

/// The core, as a client has it.
pub type Client = imp::Client;

impl Listener {
    /// The next client: on Unix only one of the user's.
    pub async fn accept(&mut self) -> io::Result<Connection> {
        self.0.accept().await
    }

    /// Gives the address up: the socket file goes; on Windows `core.pipe`, if it still
    /// names this pipe.
    pub fn close(self) {
        self.0.close();
    }
}

/// Opens the core at `path`, a socket file or a pipe.
pub async fn connect(path: &Path) -> io::Result<Client> {
    imp::connect(path).await
}

/// The pipe of the core of `data_dir`, as `core.pipe` names it (Windows); the default
/// socket (Unix).
pub fn find(data_dir: &Path) -> Option<PathBuf> {
    imp::find(data_dir)
}

#[cfg(unix)]
mod imp {
    use tokio::net::{UnixListener, UnixStream};

    use super::*;

    pub type Connection = UnixStream;
    pub type Client = UnixStream;

    #[derive(Debug)]
    pub struct Listener {
        inner: UnixListener,
        path: PathBuf,
        uid: u32,
    }

    pub fn default_for(_data_dir: &Path) -> Result<Address, String> {
        match std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            Some(dir) => Ok(Address::at(PathBuf::from(dir).join("ognivo/core.sock"))),
            None => Err("XDG_RUNTIME_DIR is not set; pass --socket".into()),
        }
    }

    pub fn find(data_dir: &Path) -> Option<PathBuf> {
        default_for(data_dir).ok().map(|a| a.path)
    }

    pub fn listen(address: &Address) -> Result<Listener, Listen> {
        let path = &address.path;
        if let Some(dir) = path.parent() {
            crate::private_dir(dir).map_err(Listen::Failed)?;
        }
        if path.exists() {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(Listen::Taken);
            }
            std::fs::remove_file(path).map_err(Listen::Failed)?;
        }
        let inner = UnixListener::bind(path).map_err(Listen::Failed)?;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(Listen::Failed)?;
        Ok(Listener {
            inner,
            path: path.clone(),
            uid: rustix::process::getuid().as_raw(),
        })
    }

    impl Listener {
        pub async fn accept(&mut self) -> io::Result<Connection> {
            loop {
                let (stream, _) = self.inner.accept().await?;
                // the socket's mode keeps others out; this is the check that does not
                // rely on it
                if stream.peer_cred().is_ok_and(|c| c.uid() == self.uid) {
                    return Ok(stream);
                }
            }
        }

        pub fn close(self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    pub async fn connect(path: &Path) -> io::Result<Client> {
        UnixStream::connect(path).await
    }
}

#[cfg(windows)]
mod imp {
    use std::hash::BuildHasher as _;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{
        ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
    };

    use super::*;

    pub type Connection = NamedPipeServer;
    pub type Client = NamedPipeClient;

    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_PIPE_BUSY: i32 = 231;
    const NAME_FILE: &str = "core.pipe";

    #[derive(Debug)]
    pub struct Listener {
        address: Address,
        next: NamedPipeServer,
    }

    pub fn default_for(data_dir: &Path) -> Result<Address, String> {
        // 128 bits from the OS's generator: RandomState's keys are, and SipHash of a
        // known input under them reveals nothing of them
        let keys = std::collections::hash_map::RandomState::new();
        let name = format!(
            r"\\.\pipe\ognivo-core-{:016x}{:016x}",
            keys.hash_one(1u8),
            keys.hash_one(2u8)
        );
        Ok(Address {
            path: PathBuf::from(name),
            named_in: Some(data_dir.join(NAME_FILE)),
        })
    }

    pub fn find(data_dir: &Path) -> Option<PathBuf> {
        let name = std::fs::read_to_string(data_dir.join(NAME_FILE)).ok()?;
        let name = name.trim();
        name.starts_with(r"\\.\pipe\").then(|| PathBuf::from(name))
    }

    fn server(path: &Path, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(path)
    }

    pub fn listen(address: &Address) -> Result<Listener, Listen> {
        let next = server(&address.path, true).map_err(|e| match e.raw_os_error() {
            Some(ERROR_ACCESS_DENIED | ERROR_PIPE_BUSY) => Listen::Taken,
            _ => Listen::Failed(e),
        })?;
        if let Some(file) = &address.named_in {
            // whole or not at all: the window reads it while a core starts
            let tmp = file.with_extension("pipe.tmp");
            let name = address.path.to_string_lossy();
            crate::private_dir(file.parent().unwrap_or(Path::new(".")))
                .and_then(|()| std::fs::write(&tmp, name.as_bytes()))
                .and_then(|()| std::fs::rename(&tmp, file))
                .map_err(Listen::Failed)?;
        }
        Ok(Listener {
            address: address.clone(),
            next,
        })
    }

    impl Listener {
        pub async fn accept(&mut self) -> io::Result<Connection> {
            let connected = self.next.connect().await;
            let next = server(&self.address.path, false)?;
            let this = std::mem::replace(&mut self.next, next);
            connected.map(|()| this)
        }

        pub fn close(self) {
            if let Some(file) = &self.address.named_in {
                let ours = std::fs::read_to_string(file)
                    .is_ok_and(|n| Path::new(n.trim()) == self.address.path);
                if ours {
                    let _ = std::fs::remove_file(file);
                }
            }
        }
    }

    pub async fn connect(path: &Path) -> io::Result<Client> {
        // every instance may be taking a client just now: a new one comes at once
        let mut tries = 0;
        loop {
            match ClientOptions::new().open(path) {
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && tries < 250 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                other => return other,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

    use super::*;

    /// A socket or pipe of the test's own.
    fn address(dir: &Path) -> Address {
        #[cfg(unix)]
        let a = Address::at(dir.join("run/core.sock"));
        #[cfg(windows)]
        let a = Address::default_for(dir).unwrap();
        a
    }

    #[tokio::test]
    async fn a_client_talks_to_the_listener_and_a_second_listener_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let address = address(dir.path());
        let mut listener = listen(&address).unwrap();
        #[cfg(windows)]
        assert_eq!(find(dir.path()).as_deref(), Some(address.path()));
        let path = address.path().to_owned();
        let client = tokio::spawn(async move {
            let mut c = connect(&path).await.unwrap();
            c.write_all(b"ping\n").await.unwrap();
            let mut line = String::new();
            BufReader::new(c).read_line(&mut line).await.unwrap();
            line
        });
        let mut server = listener.accept().await.unwrap();
        let mut line = String::new();
        let (read, mut write) = tokio::io::split(&mut server);
        BufReader::new(read).read_line(&mut line).await.unwrap();
        assert_eq!(line, "ping\n");
        write.write_all(b"pong\n").await.unwrap();
        assert_eq!(client.await.unwrap(), "pong\n");
        drop(server);
        // a second core finds the first there (on Unix its probe is a client that leaves)
        let second = listen(&address);
        assert!(matches!(second, Err(Listen::Taken)), "{second:?}");
        listener.close();
        #[cfg(windows)]
        assert_eq!(find(dir.path()), None);
        #[cfg(unix)]
        assert!(!address.path().exists());
    }
}
