//! What a connected server can do (docs/architecture.md §4, docs/m0/measurements.md).

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ServerKind {
    /// Upstream llama.cpp; `/props.build_info` looks like `b11924-12cc59ba4`.
    LlamaCpp {
        build: Option<u32>,
        commit: Option<String>,
    },
    /// ik_llama.cpp: a successful `/props` that has no `build_info`.
    IkLlama,
    /// llama-server in router mode (`/props.role == "router"`): requests must name a model.
    Router {
        build: Option<u32>,
        commit: Option<String>,
    },
    /// `/props` was unavailable, so nothing is known about the server.
    Unknown,
}

impl ServerKind {
    /// From a successful `/props` response; a failed one means [`ServerKind::Unknown`].
    pub fn from_props(build_info: Option<&str>, role: Option<&str>) -> Self {
        let parsed = build_info.map(parse_build_info);
        match (role, parsed) {
            (Some("router"), p) => {
                let (build, commit) = p.unwrap_or((None, None));
                ServerKind::Router { build, commit }
            }
            (_, Some((build, commit))) => ServerKind::LlamaCpp { build, commit },
            (_, None) => ServerKind::IkLlama,
        }
    }
}

fn parse_build_info(info: &str) -> (Option<u32>, Option<String>) {
    let number = |s: &str| s.strip_prefix('b').and_then(|n| n.parse().ok());
    match info.split_once('-') {
        Some((b, c)) => (number(b), Some(c.to_owned())),
        None => (number(info), None),
    }
}

/// How far the Native path can trust a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ServerClass {
    /// Streams every sampled token id: generated tokens are replayed verbatim.
    A,
    /// Drops ids of tokens that end in incomplete UTF-8: recover by re-tokenizing.
    B,
    /// Streams no ids: Native only in degraded mode, the Generic path is the full path.
    C,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum StreamIds {
    Complete,
    Lossy {
        received: usize,
        predicted: usize,
    },
    Missing,
    /// The probe ran, but its output never needed a multi-token character, or the counts
    /// made no sense: it proves nothing either way.
    Inconclusive,
    NotProbed,
}

/// Full SHAs of builds known to stream complete ids: ours with PR #31244
/// (scripts/llama/build-pinned.sh). Upstream builds join once the fix is merged.
const COMPLETE_ID_COMMITS: &[&str] = &["12cc59ba491447e9c7e58360e857bff11801f3b5"];

/// First upstream build with complete id streaming; set once PR #31244 is merged.
const COMPLETE_IDS_SINCE_BUILD: Option<u32> = None;

/// `build_info` carries `git rev-parse --short`, whose length depends on the clone.
fn known_complete(commit: &str) -> bool {
    commit.len() >= 7
        && COMPLETE_ID_COMMITS
            .iter()
            .any(|full| full.starts_with(commit))
}

/// Class from a conclusive stream probe when there is one, otherwise from the build.
pub fn classify(kind: &ServerKind, stream: StreamIds) -> ServerClass {
    match stream {
        StreamIds::Complete => ServerClass::A,
        StreamIds::Lossy { .. } => ServerClass::B,
        StreamIds::Missing => ServerClass::C,
        StreamIds::Inconclusive | StreamIds::NotProbed => match kind {
            ServerKind::IkLlama | ServerKind::Unknown => ServerClass::C,
            ServerKind::LlamaCpp { build, commit } | ServerKind::Router { build, commit }
                if commit.as_deref().is_some_and(known_complete)
                    || build
                        .zip(COMPLETE_IDS_SINCE_BUILD)
                        .is_some_and(|(b, since)| b >= since) =>
            {
                ServerClass::A
            }
            ServerKind::LlamaCpp { .. } | ServerKind::Router { .. } => ServerClass::B,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineCaps {
    pub kind: ServerKind,
    pub class: ServerClass,
    pub stream_ids: StreamIds,
    pub n_ctx: Option<u32>,
    pub total_slots: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_from_props() {
        assert_eq!(
            ServerKind::from_props(Some("b11924-12cc59ba4"), None),
            ServerKind::LlamaCpp {
                build: Some(11924),
                commit: Some("12cc59ba4".into())
            }
        );
        assert_eq!(ServerKind::from_props(None, None), ServerKind::IkLlama);
        assert!(matches!(
            ServerKind::from_props(Some("b11924-12cc59ba4"), Some("router")),
            ServerKind::Router { .. }
        ));
    }

    #[test]
    fn classes() {
        let pinned = ServerKind::from_props(Some("b11924-12cc59ba4"), None);
        let short = ServerKind::from_props(Some("b11924-12cc59b"), None);
        let old = ServerKind::from_props(Some("b11681-5e03a1ff4"), None);
        assert_eq!(classify(&pinned, StreamIds::NotProbed), ServerClass::A);
        assert_eq!(classify(&short, StreamIds::NotProbed), ServerClass::A);
        assert_eq!(classify(&old, StreamIds::NotProbed), ServerClass::B);
        assert_eq!(classify(&old, StreamIds::Inconclusive), ServerClass::B);
        assert_eq!(
            classify(&ServerKind::IkLlama, StreamIds::NotProbed),
            ServerClass::C
        );
        assert_eq!(
            classify(&ServerKind::Unknown, StreamIds::NotProbed),
            ServerClass::C
        );
        assert_eq!(
            classify(
                &pinned,
                StreamIds::Lossy {
                    received: 57,
                    predicted: 72
                }
            ),
            ServerClass::B
        );
    }
}
