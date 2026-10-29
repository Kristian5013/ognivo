use std::fmt;

use serde::{Deserialize, Serialize};

/// A chat-format family: everything that decides how prompts are rendered and how model
/// output is parsed. Each family gets one codec (docs/architecture.md §5.1). A model whose
/// family is unknown runs on the Generic path (llama.cpp's own templates and parsers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Family {
    /// gpt-oss: channels, `to=functions.NAME` recipients, one tool call per generation.
    Harmony,
    /// Gemma 4: `<|tool_call>call:NAME{k:<|"|>v<|"|>}<tool_call|>`, keys in sorted order.
    Gemma4,
    /// Qwen3.5+ / Qwen3.8: ChatML with `<function=NAME><parameter=K>` XML tool calls.
    QwenXml,
}

impl Family {
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Harmony => "harmony",
            Family::Gemma4 => "gemma4",
            Family::QwenXml => "qwen-xml",
        }
    }
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
