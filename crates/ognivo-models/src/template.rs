//! Chat templates as a server reports them.
//!
//! llama-server (and ik_llama.cpp) rewrite a template before using it, and `/props`
//! returns the rewritten text, so the hash of a committed file is not what a server
//! reports. [`served_form`] repeats those rewrites: the patches in llama.cpp
//! `common/chat.cpp` (`common_chat_templates_init`), then the normalisation in
//! `common/jinja/lexer.cpp`. Verified against all three reference GGUFs in M0.

use sha2::{Digest, Sha256};

const GPT_OSS_RAISE: &str = "{%- if \"<|channel|>analysis<|message|>\" in message.content or \
                             \"<|channel|>final<|message|>\" in message.content %}";
const MISTRAL_RAISE: &str = "{%- if (message['content'] is none or message['content'] == '' or \
                             message['content']|length == 0) and (message['tool_calls'] is not defined or \
                             message['tool_calls'] is none or message['tool_calls']|length == 0) %}";

/// The template text a llama.cpp server would report for `template`.
pub fn served_form(template: &str) -> String {
    let mut t = template.to_owned();
    if t.contains("<|channel|>") && t.contains("in message.content or") {
        t = t.replace(GPT_OSS_RAISE, "{%- if false %}");
    }
    if t.contains("[TOOL_CALLS]") && t.contains("if (message['content'] is none or") {
        t = t.replace(MISTRAL_RAISE, "{%- if false %}");
    }
    // the lexer checks the last byte before normalising line ends
    let strip_newline = t.ends_with('\n');
    let mut t = t.replace("\r\n", "\n").replace('\r', "\n");
    if strip_newline {
        t.pop();
    }
    t
}

pub fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalisation() {
        assert_eq!(served_form("a\r\nb\rc\n"), "a\nb\nc");
        assert_eq!(served_form("x\n\n"), "x\n");
        let gpt = format!("<|channel|> {GPT_OSS_RAISE} raise {{% endif %}}");
        assert_eq!(
            served_form(&gpt),
            "<|channel|> {%- if false %} raise {% endif %}"
        );
    }
}
