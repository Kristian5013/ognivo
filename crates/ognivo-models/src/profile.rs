//! Per-model behaviour as data (docs/architecture.md §5.1, "Policies").
//!
//! The built-in profiles live in `profiles/*.toml` at the repository root; every value
//! there carries its source in a comment.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::Family;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    pub id: String,
    pub family: Family,
    pub display_name: String,
    pub detect: Detect,
    pub tokens: TokenTable,
    pub sampling: SamplingTable,
    pub reasoning: Reasoning,
    pub context: ContextBudget,
    pub template: TemplatePin,
    pub server: ServerHint,
}

impl ModelProfile {
    /// `chat_template_kwargs` for the Generic path. `[reasoning]` is the single source for
    /// thinking and effort; `[template].kwargs` only adds keys unrelated to reasoning.
    pub fn chat_template_kwargs(&self, level: Option<&str>) -> Map<String, Value> {
        let level = level.unwrap_or(&self.reasoning.default);
        let preserve = self.reasoning.history == ReasoningHistory::Preserve;
        let mut kw = self.template.kwargs.clone();
        match self.family {
            Family::Harmony => {
                kw.insert("reasoning_effort".into(), json!(level));
            }
            Family::Gemma4 => {
                kw.insert("enable_thinking".into(), json!(level != "off"));
                kw.insert("preserve_thinking".into(), json!(preserve));
            }
            Family::QwenXml => {
                kw.insert("enable_thinking".into(), json!(level != "off"));
                kw.insert("preserve_thinking".into(), json!(preserve));
                // Qwen3.6 thinks or does not; Qwen3.8 has levels
                if level != "off" && self.reasoning.control == ReasoningControl::Effort {
                    kw.insert("reasoning_effort".into(), json!(level));
                }
            }
        }
        kw
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Detect {
    /// Text that must tokenize to exactly `id` (with special-token parsing on).
    pub probe: ProbeToken,
    /// Lowercase substrings of the GGUF file name that pick this profile within its family.
    #[serde(default)]
    pub name_hints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeToken {
    pub text: String,
    pub id: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenTable {
    /// Structural token strings; their ids must never occur inside content spans.
    pub structural: Vec<String>,
    /// The structural strings that frame messages and calls, without the reasoning tags:
    /// unbroken inside tool arguments they mean the model wrote past the end of its call
    /// (§5.3). The reasoning tags stay out because tasks quote them (M0, chat-multiturn).
    pub framing: Vec<String>,
    /// Every other named control token (bos/eos, media, FIM...). Not used for structure,
    /// but just as injectable: the content guard covers `structural` and `specials`.
    #[serde(default)]
    pub specials: Vec<String>,
    /// End-of-generation ids.
    pub eog: Vec<u32>,
    /// BOS id the agent prepends itself (token-array prompts get none from the server).
    #[serde(default)]
    pub bos: Option<u32>,
}

impl TokenTable {
    /// All strings the content guard must break before tokenization.
    pub fn guarded(&self) -> impl Iterator<Item = &str> {
        self.structural
            .iter()
            .chain(&self.specials)
            .map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SamplingTable {
    pub main: Sampling,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sampling {
    pub temperature: f32,
    pub top_p: f32,
    pub top_k: u32,
    pub min_p: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reasoning {
    pub control: ReasoningControl,
    /// The levels this model accepts, e.g. low/medium/high or on/off.
    pub levels: Vec<String>,
    pub default: String,
    pub history: ReasoningHistory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningControl {
    /// A level in the system message (gpt-oss, Qwen): fixed for the session.
    Effort,
    /// Thinking on or off for the whole session (Gemma 4).
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningHistory {
    /// Keep reasoning inside a tool loop; drop it once a final answer exists.
    DropAfterFinal,
    /// Keep reasoning of tool steps across turns.
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextBudget {
    /// Tokens of working context. The profile's is what it was measured at, and a session's
    /// when the server does not say its window; a session works with the window, or with
    /// what the user chose ([`ContextBudget::working`]).
    pub working_max: u32,
    /// Fraction of `working_max` at which compaction starts.
    pub compact_at: f32,
    /// `max_tokens` of a main step: fixed, never the window remainder.
    pub max_tokens: u32,
    /// The most the model works with: the window it was trained for, or less where it
    /// breaks. A session's working context stays under it, whatever the server's window.
    pub ceiling: u32,
}

impl ContextBudget {
    /// The budget of a session: its working context is `chosen`, or else the server slot's
    /// window `n_ctx`, or else the profile's own, and never past the ceiling.
    pub fn working(mut self, chosen: Option<u32>, n_ctx: Option<u32>) -> Self {
        self.working_max = chosen
            .or(n_ctx)
            .unwrap_or(self.working_max)
            .min(self.ceiling);
        self
    }
}

/// The vetted chat template for the Generic path (docs/research/2026-10-29/templates.md).
/// The Native path renders prompts itself and does not depend on it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplatePin {
    /// Repository-relative directory with `canonical.jinja`, its provenance and goldens.
    pub dir: String,
    /// sha256 of the committed `canonical.jinja` file.
    pub file_sha256: String,
    /// Extra `chat_template_kwargs` unrelated to reasoning.
    #[serde(default)]
    pub kwargs: Map<String, Value>,
    /// Templates seen in the wild, keyed by the sha256 of their served form
    /// ([`crate::template::served_form`]), which is what `/props` reports.
    #[serde(default)]
    pub known: Vec<KnownTemplate>,
}

impl TemplatePin {
    pub fn lookup(&self, served_sha256: &str) -> Option<&KnownTemplate> {
        self.known
            .iter()
            .find(|k| k.sha256.eq_ignore_ascii_case(served_sha256))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnownTemplate {
    pub sha256: String,
    pub verdict: TemplateVerdict,
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TemplateVerdict {
    Canonical,
    /// Differs from canonical but renders the same or harmlessly.
    Ok,
    /// Known to corrupt prompts on the Generic path.
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerHint {
    /// Advisory llama-server flags shown by `ognivo doctor`; ognivo never launches servers.
    pub recommended_flags: String,
    pub notes: String,
}

#[derive(Debug, thiserror::Error)]
#[error("profile {file}: {source}")]
pub struct ProfileError {
    pub file: String,
    #[source]
    pub source: toml::de::Error,
}

const BUILTIN: &[(&str, &str)] = &[
    (
        "gpt-oss-20b.toml",
        include_str!("../../../profiles/gpt-oss-20b.toml"),
    ),
    (
        "gemma-4-26b-a4b.toml",
        include_str!("../../../profiles/gemma-4-26b-a4b.toml"),
    ),
    (
        "qwen3.8-27b.toml",
        include_str!("../../../profiles/qwen3.8-27b.toml"),
    ),
    (
        "qwen3.6-35b-a3b.toml",
        include_str!("../../../profiles/qwen3.6-35b-a3b.toml"),
    ),
];

/// The profiles compiled into the binary.
pub fn builtin() -> Result<Vec<ModelProfile>, ProfileError> {
    BUILTIN
        .iter()
        .map(|(file, text)| {
            toml::from_str(text).map_err(|source| ProfileError {
                file: (*file).into(),
                source,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{served_form, sha256_hex};

    #[test]
    fn builtin_profiles_are_consistent() {
        let profiles = builtin().unwrap();
        let families: Vec<Family> = profiles.iter().map(|p| p.family).collect();
        assert_eq!(
            families,
            [
                Family::Harmony,
                Family::Gemma4,
                Family::QwenXml,
                Family::QwenXml
            ]
        );
        for p in &profiles {
            assert!(
                p.tokens.structural.contains(&p.detect.probe.text),
                "{}: probe must be structural",
                p.id
            );
            assert!(!p.tokens.framing.is_empty(), "{}: no framing", p.id);
            for f in &p.tokens.framing {
                assert!(
                    p.tokens.structural.contains(f),
                    "{}: {f} is not structural",
                    p.id
                );
            }
            assert!(
                p.context.compact_at > 0.0 && p.context.compact_at < 1.0,
                "{}",
                p.id
            );
            assert!(
                p.context.working_max <= p.context.ceiling,
                "{}: measured past its ceiling",
                p.id
            );
            assert!(
                p.reasoning.levels.contains(&p.reasoning.default),
                "{}: default level",
                p.id
            );
            for key in ["reasoning_effort", "enable_thinking", "preserve_thinking"] {
                assert!(
                    !p.template.kwargs.contains_key(key),
                    "{}: {key} belongs to [reasoning]",
                    p.id
                );
            }
        }
    }

    #[test]
    fn a_session_works_with_its_choice_or_the_window_under_the_ceiling() {
        let b = ContextBudget {
            working_max: 65_536,
            compact_at: 0.65,
            max_tokens: 16_384,
            ceiling: 262_144,
        };
        assert_eq!(b.working(None, None).working_max, 65_536);
        assert_eq!(b.working(None, Some(262_144)).working_max, 262_144);
        assert_eq!(b.working(Some(32_768), Some(262_144)).working_max, 32_768);
        assert_eq!(b.working(None, Some(1_048_576)).working_max, 262_144);
        assert_eq!(b.working(Some(500_000), None).working_max, 262_144);
    }

    #[test]
    fn template_kwargs_follow_reasoning() {
        let p = builtin().unwrap();
        assert_eq!(
            Value::Object(p[0].chat_template_kwargs(None)),
            json!({"reasoning_effort": "high"})
        );
        assert_eq!(
            Value::Object(p[1].chat_template_kwargs(None)),
            json!({"enable_thinking": true, "preserve_thinking": true})
        );
        assert_eq!(
            Value::Object(p[2].chat_template_kwargs(None)),
            json!({"enable_thinking": true, "preserve_thinking": true, "reasoning_effort": "medium"})
        );
        assert_eq!(
            Value::Object(p[2].chat_template_kwargs(Some("off"))),
            json!({"enable_thinking": false, "preserve_thinking": true})
        );
        // Qwen3.6: thinking on or off, no effort
        assert_eq!(
            Value::Object(p[3].chat_template_kwargs(None)),
            json!({"enable_thinking": true, "preserve_thinking": true})
        );
    }

    /// The pinned file hash matches the committed template, and its served form is the
    /// one listed as canonical.
    #[test]
    fn canonical_templates_match_their_pins() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for p in builtin().unwrap() {
            let file = repo.join(&p.template.dir).join("canonical.jinja");
            let text = std::fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            assert_eq!(sha256_hex(&text), p.template.file_sha256, "{}", p.id);
            let served = p
                .template
                .lookup(&sha256_hex(&served_form(&text)))
                .expect("served canonical is listed");
            assert_eq!(served.verdict, TemplateVerdict::Canonical, "{}", p.id);
        }
    }
}
