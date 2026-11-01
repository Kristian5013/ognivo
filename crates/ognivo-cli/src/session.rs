//! What `run` and `chat` share: the server and its profile, the project root, the data
//! directory and the agent's options.

use std::fmt::Display;
use std::path::{Path, PathBuf};

use clap::Args;
use ognivo_core::agent::AgentOptions;
use ognivo_core::approval::WorkspaceState;
use ognivo_models::llama::LlamaServer;
use ognivo_models::{ChatDriver, Family, doctor, profile};

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
    /// Where logs go; ~/.ognivo/data by default
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

#[derive(Debug)]
pub struct Session {
    pub driver: ChatDriver,
    pub family: Family,
    /// The profile's id.
    pub profile: String,
    /// The same server, for questions outside the agent's steps.
    pub server: LlamaServer,
    pub opts: AgentOptions,
}

impl SessionArgs {
    /// `--cwd` or the current directory, canonical.
    pub fn root(&self) -> Result<PathBuf, Failure> {
        let dir = match &self.cwd {
            Some(d) => d.clone(),
            None => std::env::current_dir().map_err(Failure::usage)?,
        };
        std::fs::canonicalize(&dir)
            .map_err(|_| Failure::usage(format!("{}: no such directory", dir.display())))
    }

    /// `--data-dir` or `~/.ognivo/data`.
    pub fn data_dir(&self) -> Result<PathBuf, Failure> {
        match (&self.data_dir, std::env::var_os("HOME")) {
            (Some(d), _) => Ok(d.clone()),
            (None, Some(home)) => Ok(PathBuf::from(home).join(".ognivo/data")),
            (None, None) => Err(Failure::usage("HOME is not set; pass --data-dir")),
        }
    }

    /// Finds the profile (from `--profile` or by asking the server) and builds the driver.
    /// The options have no time limit.
    pub async fn connect(&self, root: PathBuf) -> Result<Session, Failure> {
        let data_dir = self.data_dir()?;
        let server = LlamaServer::new(&self.url, self.api_key.clone()).map_err(Failure::usage)?;
        let profiles = profile::builtin().map_err(Failure::usage)?;
        let chosen = match &self.profile {
            Some(id) => profiles.iter().find(|p| &p.id == id),
            None => doctor::identify(&server, &profiles)
                .await
                .map_err(|e| Failure::server(format!("{}: {e}", self.url)))?
                .map(|(p, _)| p),
        };
        let Some(profile) = chosen.cloned() else {
            return Err(Failure::usage(
                "no profile matches this server; pass --profile (see ognivo doctor)",
            ));
        };
        let (family, id, max_tokens) = (
            profile.family,
            profile.id.clone(),
            profile.context.max_tokens,
        );
        let driver = ChatDriver::new(server.clone(), profile, self.level.as_deref(), self.slot)
            .map_err(Failure::usage)?;
        let seed = self.seed.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(1, |d| d.subsec_nanos())
        });
        Ok(Session {
            driver,
            family,
            profile: id,
            server,
            opts: AgentOptions {
                workspace: start_state(&root),
                root,
                data_dir,
                max_steps: self.max_steps,
                max_tokens,
                seed,
                time_limit: None,
            },
        })
    }
}

/// Clean only when git says nothing is changed or untracked; otherwise, or outside git,
/// assume unreviewed changes exist (§8: never assume clean).
fn start_state(root: &Path) -> WorkspaceState {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "--no-pager",
            "status",
            "--porcelain",
        ])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() && o.stdout.is_empty() => WorkspaceState::CLEAN,
        _ => WorkspaceState::DIRTY,
    }
}
