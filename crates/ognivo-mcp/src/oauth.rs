//! Signing in to a remote server, as MCP's authorization asks (spec 2026-07-28), with
//! rmcp's OAuth client: OAuth 2.1 with PKCE, the user's browser, and a redirect back to a
//! loopback port of this machine (RFC 8252).
//!
//! - The client is the one the config names (`oauth.clientId`), or else one the server's
//!   authorization server registers there and then (RFC 7591). A Client ID Metadata
//!   Document would need a public HTTPS page of ognivo's, which there is not yet.
//! - Each server's sign-in is kept in its own file, `<dir>/<name>.json`, with the server's
//!   URL, readable by the user alone (0600, in a directory of 0700). A file kept for another
//!   URL is not used: a name given to another server in a config does not take the token.
//! - A session's gateway takes the token from there ([`client`]) and refreshes it when it
//!   runs out, one refresh at a time across ognivo's processes (a lock beside the file).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rmcp::transport::auth::{
    AuthClient, AuthError, AuthorizationManager, AuthorizationRequest, CredentialRefreshGuard,
    CredentialStore, OAuthState, StoredCredentials,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::config::{OAuth, ServerConfig, Transport};

/// How long a sign-in waits for the browser to come back.
pub const SIGN_IN_WAIT: Duration = Duration::from_secs(300);
/// The name the client registers under.
const CLIENT_NAME: &str = "ognivo";

/// `~/.ognivo/mcp-auth`, where the sign-ins of the user whose home is `home` are kept.
pub fn credentials_dir(home: &Path) -> PathBuf {
    home.join(".ognivo/mcp-auth")
}

/// The file of `server`'s sign-in under `dir`: its name, with what a file name cannot hold
/// made `_`.
pub fn credentials_file(dir: &Path, server: &str) -> PathBuf {
    let name: String = server
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{name}.json"))
}

/// What a sign-in's file holds.
#[derive(Serialize, Deserialize)]
struct Kept {
    /// The server it is for.
    url: String,
    credentials: StoredCredentials,
}

/// One server's sign-in, in its file.
#[derive(Debug, Clone)]
struct FileStore {
    path: PathBuf,
    url: String,
}

impl FileStore {
    fn new(dir: &Path, name: &str, url: &str) -> Self {
        Self {
            path: credentials_file(dir, name),
            url: url.to_owned(),
        }
    }

    fn read(&self) -> Result<Option<StoredCredentials>, String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{}: {e}", self.path.display())),
        };
        let kept: Kept =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", self.path.display()))?;
        Ok((kept.url == self.url).then_some(kept.credentials))
    }

    /// Writes the file whole, through a file beside it, so a reader never sees half of it.
    fn write(&self, credentials: StoredCredentials) -> Result<(), String> {
        use std::io::Write as _;
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
        let shown = |e: std::io::Error| format!("{}: {e}", self.path.display());
        if let Some(dir) = self.path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(shown)?;
        }
        let text = serde_json::to_string_pretty(&Kept {
            url: self.url.clone(),
            credentials,
        })
        .map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(shown)?;
        f.write_all(text.as_bytes()).map_err(shown)?;
        f.sync_all().map_err(shown)?;
        std::fs::rename(&tmp, &self.path).map_err(shown)
    }
}

fn store_error(e: String) -> AuthError {
    AuthError::CredentialStoreError(e)
}

#[async_trait::async_trait]
impl CredentialStore for FileStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        self.read().map_err(store_error)
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        self.write(credentials).map_err(store_error)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(store_error(format!("{}: {e}", self.path.display()))),
        }
    }

    /// One refresh at a time: another ognivo with the same sign-in waits for this one's
    /// token rather than spend the refresh token a second time.
    async fn acquire_refresh_guard(&self) -> Result<Option<CredentialRefreshGuard>, AuthError> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let lock = self.path.with_extension("lock");
        let held = tokio::task::spawn_blocking(move || -> Result<std::fs::File, String> {
            if let Some(dir) = lock.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            let f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&lock)
                .map_err(|e| format!("{}: {e}", lock.display()))?;
            rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive)
                .map_err(|e| format!("{}: {e}", lock.display()))?;
            Ok(f)
        })
        .await
        .map_err(|e| store_error(e.to_string()))?
        .map_err(store_error)?;
        Ok(Some(CredentialRefreshGuard::new(held)))
    }
}

/// The URL and the config's `oauth` of a remote server.
fn remote(server: &ServerConfig) -> Result<(&str, &OAuth), String> {
    match &server.transport {
        Transport::Http { url, oauth, .. } => Ok((url, oauth)),
        Transport::Stdio { .. } => Err(format!(
            "{} is a local server: it has nothing to sign in to",
            server.name
        )),
    }
}

/// Whether a sign-in to `server` is kept under `dir`, for its URL.
pub fn signed_in(dir: &Path, server: &ServerConfig) -> bool {
    remote(server).is_ok_and(|(url, _)| {
        FileStore::new(dir, &server.name, url)
            .read()
            .is_ok_and(|c| c.is_some_and(|c| c.token_response.is_some()))
    })
}

/// Forgets the sign-in to `server`; whether there was one.
pub fn sign_out(dir: &Path, server: &ServerConfig) -> Result<bool, String> {
    let path = credentials_file(dir, &server.name);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The client of `server`'s transport, with the token kept under `dir`; `None` when there
/// is no sign-in for it.
pub(crate) async fn client(
    dir: &Path,
    server: &ServerConfig,
) -> Result<Option<AuthClient<reqwest::Client>>, String> {
    let (url, _) = remote(server)?;
    if !signed_in(dir, server) {
        return Ok(None);
    }
    let mut manager = AuthorizationManager::new(url)
        .await
        .map_err(|e| format!("its sign-in: {e}"))?;
    manager.set_credential_store(FileStore::new(dir, &server.name, url));
    if !manager
        .initialize_from_store()
        .await
        .map_err(|e| format!("its sign-in: {e}"))?
    {
        return Ok(None);
    }
    Ok(Some(AuthClient::new(reqwest::Client::new(), manager)))
}

/// Signs in to `server` and keeps the token under `dir`. `challenge` is the server's
/// `WWW-Authenticate`, when it gave one; `open` gets the page to open in the user's browser,
/// which comes back to a loopback port within [`SIGN_IN_WAIT`].
pub async fn sign_in(
    dir: &Path,
    server: &ServerConfig,
    challenge: Option<&str>,
    open: impl FnOnce(&str),
) -> Result<(), String> {
    let (url, oauth) = remote(server)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", oauth.callback_port.unwrap_or(0)))
        .await
        .map_err(|e| format!("the redirect's port: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let shown = |e: AuthError| e.to_string();
    let mut manager = AuthorizationManager::new(url).await.map_err(shown)?;
    manager.set_credential_store(FileStore::new(dir, &server.name, url));
    let mut state = OAuthState::Unauthorized(manager);
    let mut request = AuthorizationRequest::new(&redirect).with_client_name(CLIENT_NAME);
    if let Some(c) = challenge.filter(|c| !c.trim().is_empty()) {
        request = request.with_challenge(c);
    }
    if let Some(id) = &oauth.client_id {
        request = request.with_preregistered_client(id);
    }
    if let Some(secret) = &oauth.client_secret {
        request = request.with_client_secret(secret);
    }
    if !oauth.scopes.is_empty() {
        request = request.with_scopes(oauth.scopes.iter().cloned());
    }
    state.start_authorization(request).await.map_err(shown)?;
    let page = state.get_authorization_url().await.map_err(shown)?;
    open(&page);
    let back = tokio::time::timeout(SIGN_IN_WAIT, came_back(&listener, port, &server.name))
        .await
        .map_err(|_| {
            format!(
                "the browser did not come back within {} min",
                SIGN_IN_WAIT.as_secs() / 60
            )
        })??;
    state.handle_callback_url(&back).await.map_err(shown)
}

/// The URL the browser comes back to, with the code; what else comes gets a 404.
async fn came_back(
    listener: &tokio::net::TcpListener,
    port: u16,
    name: &str,
) -> Result<String, String> {
    loop {
        let (mut conn, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut head = Vec::new();
        let mut buf = [0u8; 2048];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 16 * 1024 {
            match conn.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        let head = String::from_utf8_lossy(&head);
        let target = head
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("GET "))
            .and_then(|l| l.split(' ').next())
            .unwrap_or_default()
            .to_owned();
        if !target.starts_with("/callback") {
            let _ = conn
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let back = format!("http://127.0.0.1:{port}{target}");
        let refused = reqwest::Url::parse(&back).ok().and_then(|u| {
            let pairs: Vec<(String, String)> = u.query_pairs().into_owned().collect();
            let get = |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| key == k)
                    .map(|(_, v)| v.clone())
            };
            get("error").map(|e| match get("error_description") {
                Some(d) => format!("{e}: {d}"),
                None => e,
            })
        });
        let text = match &refused {
            Some(why) => format!("ognivo: {name} did not let you in ({why})."),
            None => format!("ognivo: you are signed in to {name}. This tab can be closed."),
        };
        let page = format!(
            "<!doctype html><meta charset=\"utf-8\"><title>ognivo</title><p>{}</p>",
            html(&text)
        );
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = conn.write_all(reply.as_bytes()).await;
        let _ = conn.shutdown().await;
        return match refused {
            Some(why) => Err(format!("the server refused the sign-in: {why}")),
            None => Ok(back),
        };
    }
}

fn html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sign_in_is_kept_for_its_url_alone_and_for_the_user_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let store = FileStore::new(dir.path(), "my docs/v2", "https://a.example/mcp");
        assert!(store.read().unwrap().is_none());
        store
            .write(StoredCredentials::new(
                "c1".into(),
                None,
                vec!["read".into()],
                Some(1),
            ))
            .unwrap();
        let path = credentials_file(dir.path(), "my docs/v2");
        assert!(path.ends_with("my_docs_v2.json"), "{}", path.display());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let kept = store.read().unwrap().unwrap();
        assert_eq!(kept.client_id, "c1");
        // the same name for another server finds nothing
        let other = FileStore::new(dir.path(), "my docs/v2", "https://b.example/mcp");
        assert!(other.read().unwrap().is_none());
        assert_eq!(html("<b>&"), "&lt;b&gt;&amp;");
    }
}
