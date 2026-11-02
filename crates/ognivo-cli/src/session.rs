//! What `run`, `chat` and `sessions` share: the server and its profile, the project root,
//! the data directory with the session store, and the agent's options.

use std::ffi::OsString;
use std::fmt::Display;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::Args;
use ognivo_core::agent::{Agent, AgentOptions, Approver};
use ognivo_core::approval::WorkspaceState;
use ognivo_core::instructions::UserInstructions;
use ognivo_core::session::Session;
use ognivo_models::llama::LlamaServer;
use ognivo_models::{ChatDriver, Family, doctor, profile};
use ognivo_store::{NewSession, SessionRow, Store};

/// The session store, in the data directory.
const STORE_FILE: &str = "ognivo.db";
/// How long the start waits for the server's `/props`; without them only the profile's
/// budget decides when to compact.
const PROPS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Args)]
pub struct SessionArgs {
    /// Server base URL, e.g. http://127.0.0.1:8090
    #[arg(long)]
    pub url: String,
    /// Bearer token if the server was started with --api-key
    #[arg(long, env = "OGNIVO_API_KEY", hide_env_values = true)]
    pub api_key: Option<String>,
    /// Profile id (gpt-oss-20b, gemma-4-26b-a4b, qwen3.8-27b); detected from the server when omitted
    #[arg(long)]
    pub profile: Option<String>,
    /// Project root; the current directory by default
    #[arg(long)]
    pub cwd: Option<PathBuf>,
    /// Steps of one turn
    #[arg(long, default_value_t = 200)]
    pub max_steps: u32,
    /// Seed of the first step (random when omitted)
    #[arg(long)]
    pub seed: Option<u32>,
    /// Reasoning level from the profile (e.g. low/medium/high for gpt-oss)
    #[arg(long)]
    pub level: Option<String>,
    /// Server slot to pin, so the session keeps its KV cache
    #[arg(long)]
    pub slot: Option<u32>,
    /// Where sessions and logs go; ~/.ognivo/data by default
    #[arg(long)]
    pub data_dir: Option<PathBuf>,
}

/// Why a session cannot start, with its exit code: 2 bad usage, 3 the server failed.
#[derive(Debug)]
pub struct Failure {
    pub code: u8,
    pub message: String,
}

impl Failure {
    pub fn usage(message: impl Display) -> Self {
        Self {
            code: 2,
            message: message.to_string(),
        }
    }

    fn server(message: impl Display) -> Self {
        Self {
            code: 3,
            message: message.to_string(),
        }
    }
}

/// A server ready for the agent, and what the agent starts with.
#[derive(Debug)]
pub struct Connected {
    pub driver: ChatDriver,
    pub family: Family,
    /// The profile's id.
    pub profile: String,
    pub level: Option<String>,
    /// What git says is changed in the project now; `None` outside git.
    changed: Option<Vec<PathBuf>>,
    pub opts: AgentOptions,
}

impl Connected {
    /// The agent of a new session, kept in `store` from the start.
    pub fn new_agent(
        self,
        kind: &str,
        url: &str,
        store: Store,
        approver: Box<dyn Approver>,
    ) -> Result<Agent, Failure> {
        let new = NewSession {
            kind: kind.to_owned(),
            root: self.opts.root.clone(),
            profile: self.profile,
            level: self.level,
            server: url.to_owned(),
            seed: self.opts.seed,
        };
        let session = Session::create(store, &new).map_err(store_failure)?;
        let mut agent = Agent::new(Box::new(self.driver), self.family, approver, self.opts);
        agent.keep_in(session);
        Ok(agent)
    }

    /// The agent of a stored session, picked up where it ended. Changes in the project
    /// that its edit journal does not hold count as unreviewed.
    pub fn resumed_agent(
        mut self,
        row: &SessionRow,
        store: Store,
        approver: Box<dyn Approver>,
    ) -> Result<Agent, Failure> {
        let (session, left) = Session::open(store, row.id).map_err(Failure::usage)?;
        let known: Vec<PathBuf> = left.journal.files.iter().map(|f| f.path.clone()).collect();
        self.opts.workspace = state(self.changed.as_deref(), &known);
        Ok(Agent::resume(
            Box::new(self.driver),
            self.family,
            approver,
            self.opts,
            session,
            left,
        ))
    }
}

impl SessionArgs {
    /// `--cwd` or the current directory, canonical.
    pub fn root(&self) -> Result<PathBuf, Failure> {
        root(self.cwd.as_deref())
    }

    /// `--data-dir` or `~/.ognivo/data`.
    pub fn data_dir(&self) -> Result<PathBuf, Failure> {
        data_dir(self.data_dir.as_deref())
    }

    pub fn store(&self) -> Result<Store, Failure> {
        open_store(&self.data_dir()?)
    }

    /// The project of a stored session: its own root, which `--cwd` may only repeat.
    pub fn root_of(&self, row: &SessionRow) -> Result<PathBuf, Failure> {
        if self.cwd.is_some() && self.root()? != row.root {
            return Err(Failure::usage(format!(
                "session {} belongs to {}",
                row.id,
                row.root.display()
            )));
        }
        if !row.root.is_dir() {
            return Err(Failure::usage(format!(
                "the project of session {} is gone: {}",
                row.id,
                row.root.display()
            )));
        }
        Ok(row.root.clone())
    }

    /// Finds the profile (from `--profile` or by asking the server) and builds the driver.
    /// A stored session keeps its profile, reasoning level and seed: flags may only repeat
    /// them, and the server must run its model. The options have no time limit.
    pub async fn connect(
        &self,
        root: PathBuf,
        stored: Option<&SessionRow>,
    ) -> Result<Connected, Failure> {
        let data_dir = self.data_dir()?;
        if let Some(row) = stored {
            let kept_seed = row.seed.to_string();
            for (flag, given, kept) in [
                (
                    "profile",
                    self.profile.as_deref(),
                    Some(row.profile.as_str()),
                ),
                ("level", self.level.as_deref(), row.level.as_deref()),
                (
                    "seed",
                    self.seed.map(|s| s.to_string()).as_deref(),
                    Some(kept_seed.as_str()),
                ),
            ] {
                if given.is_some() && given != kept {
                    return Err(Failure::usage(format!(
                        "session {} goes on with {flag} {}; drop --{flag}",
                        row.id,
                        kept.unwrap_or("the profile's default")
                    )));
                }
            }
        }
        let server = LlamaServer::new(&self.url, self.api_key.clone()).map_err(Failure::usage)?;
        let profiles = profile::builtin().map_err(Failure::usage)?;
        let chosen = match &self.profile {
            Some(id) => profiles.iter().find(|p| &p.id == id),
            None => {
                let found = doctor::identify(&server, &profiles)
                    .await
                    .map_err(|e| Failure::server(format!("{}: {e}", self.url)))?
                    .map(|(p, _)| p);
                if let (Some(row), Some(p)) = (stored, found)
                    && p.id != row.profile
                {
                    return Err(Failure::usage(format!(
                        "{} runs {}; session {} was with {}",
                        self.url, p.id, row.id, row.profile
                    )));
                }
                found
            }
        };
        let Some(profile) = chosen.cloned() else {
            return Err(Failure::usage(
                "no profile matches this server; pass --profile (see ognivo doctor)",
            ));
        };
        let level = self
            .level
            .clone()
            .or_else(|| stored.and_then(|r| r.level.clone()));
        let (family, id, context) = (profile.family, profile.id.clone(), profile.context);
        // the slot's context bounds when the history is compacted
        let n_ctx = tokio::time::timeout(PROPS_TIMEOUT, server.props())
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|p| p.n_ctx);
        let driver = ChatDriver::new(server, profile, level.as_deref(), self.slot)
            .map_err(Failure::usage)?;
        let seed = stored.map(|r| r.seed).or(self.seed).unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.subsec_nanos())
        });
        let changed = changed_files(&root);
        Ok(Connected {
            driver,
            family,
            profile: id,
            level,
            opts: AgentOptions {
                workspace: state(changed.as_deref(), &[]),
                root,
                data_dir,
                max_steps: self.max_steps,
                context,
                n_ctx,
                seed,
                time_limit: None,
                user_instructions: user_instructions(),
            },
            changed,
        })
    }
}

/// `cwd` or the current directory, canonical.
pub fn root(cwd: Option<&Path>) -> Result<PathBuf, Failure> {
    let dir = match cwd {
        Some(d) => d.to_owned(),
        None => std::env::current_dir().map_err(Failure::usage)?,
    };
    std::fs::canonicalize(&dir)
        .map_err(|_| Failure::usage(format!("{}: no such directory", dir.display())))
}

/// `dir` or `~/.ognivo/data`.
pub fn data_dir(dir: Option<&Path>) -> Result<PathBuf, Failure> {
    match (dir, std::env::var_os("HOME")) {
        (Some(d), _) => Ok(d.to_owned()),
        (None, Some(home)) => Ok(PathBuf::from(home).join(".ognivo/data")),
        (None, None) => Err(Failure::usage("HOME is not set; pass --data-dir")),
    }
}

/// The session store in the data directory, created if it is not there.
pub fn open_store(data_dir: &Path) -> Result<Store, Failure> {
    Store::open(&data_dir.join(STORE_FILE)).map_err(store_failure)
}

fn store_failure(e: impl Display) -> Failure {
    Failure::usage(format!("the session store: {e}"))
}

/// Clean only when git says nothing is changed or untracked but the files in `known` (an
/// edit journal's); otherwise, or outside git, assume unreviewed changes exist (§8: never
/// assume clean).
fn state(changed: Option<&[PathBuf]>, known: &[PathBuf]) -> WorkspaceState {
    match changed {
        Some(changed) if changed.iter().all(|p| known.contains(p)) => WorkspaceState::CLEAN,
        _ => WorkspaceState::DIRTY,
    }
}

/// The files git says are changed or untracked, absolute; `None` outside git or when git
/// fails.
fn changed_files(root: &Path) -> Option<Vec<PathBuf>> {
    let top = git(root, &["rev-parse", "--show-toplevel"])?;
    let top = PathBuf::from(OsString::from_vec(top.trim_ascii_end().to_vec()));
    let status = git(
        root,
        &["status", "--porcelain", "-z", "--untracked-files=all"],
    )?;
    let mut changed = Vec::new();
    let mut fields = status.split(|&b| b == 0).filter(|f| !f.is_empty());
    while let Some(entry) = fields.next() {
        // `XY path`; a rename or copy names the old path in the next field
        let path = entry.get(3..)?;
        changed.push(top.join(OsString::from_vec(path.to_vec())));
        if matches!(entry[0], b'R' | b'C') {
            fields.next();
        }
    }
    Some(changed)
}

/// What a git command prints, if it succeeds.
fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = std::process::Command::new("git")
        .args(["-c", "core.fsmonitor=false", "--no-pager"])
        .args(args)
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// The user's instruction files under `$HOME`, Claude Code's in `CLAUDE_CONFIG_DIR` when
/// that is set.
fn user_instructions() -> UserInstructions {
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) else {
        return UserInstructions::default();
    };
    let home = PathBuf::from(home);
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from);
    UserInstructions::at(std::fs::canonicalize(&home).unwrap_or(home), claude)
}
