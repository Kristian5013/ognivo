//! Compaction in the middle of a turn (docs/architecture.md §5.6).
//!
//! Before each step the agent estimates the prompt: the tokens the server held after the
//! last step and the messages added since, at three bytes a token. Past the [`limit`] (the
//! profile's `working_max × compact_at`, and never so close to the slot's `n_ctx` that a
//! step could not generate its `max_tokens`) the history is rebuilt as one new cache
//! epoch:
//! - the static prefix as it was: the system message and the tools;
//! - one user message ([`note`]) with the user's requests, the model's own summary and what
//!   ognivo knows for certain: the unreviewed changes and the builds and tests of the turn;
//! - the last steps of the turn as they were, reasoning included ([`kept_from`]), within a
//!   quarter of the limit; a result too long for that is cut ([`fit`]).
//!
//! The model writes the summary in a request with the same prefix and slot, so the server
//! takes the history from its cache. Without a summary the note says so, and the facts and
//! the steps remain. Compaction is skipped when it would free too little to matter, and the
//! turn stops after [`MAX_IDLE`] compactions in a row with nothing new read or changed
//! between them: then the files the work needs do not fit the context together.

use ognivo_models::profile::ContextBudget;
use ognivo_models::{ChatMessage, Image};

use crate::journal::FileChange;

/// Bytes per token in estimates: low, so the estimates are high.
const BYTES_PER_TOKEN: u64 = 3;
/// Tokens of a summary request beyond the history: its instruction and the template.
const SUMMARY_PROMPT: u64 = 512;
/// When the slot leaves less than this beside `max_tokens`, compaction cannot help, and the
/// working budget alone decides.
const MIN_ROOM: u64 = 8192;
/// Steps kept as they were, at most.
const MAX_KEPT_STEPS: usize = 4;
/// Requests quoted in the note, the latest ones; the summary covers the others.
const MAX_REQUESTS: usize = 8;
/// Characters of one request in the note.
const REQUEST_CHARS: usize = 2000;
/// Idle compactions in a row (nothing new read, nothing changed since the one before)
/// that end the turn: the work does not fit the context.
pub const MAX_IDLE: u32 = 2;

/// The last message of a summary request.
pub const SUMMARY_REQUEST: &str = "[ognivo] The context is nearly full, so the conversation will now be compacted. Write a summary from which you can go on with the work with nothing else: the user's requests and your latest steps are kept, and ognivo adds the changed files and the results of builds and tests itself. Do not call tools; answer with the summary only, in this form:\n\
Task: what the user wants, and the constraints they set.\n\
Done: what you found and changed so far, with file paths and the exact names of the functions, types and commands involved.\n\
Failed: what you tried that did not work, and why, so that you do not try it again.\n\
Now: where the work stands, with the exact text of any open error.\n\
Next: the next concrete steps.\n\
At most 400 words.";

/// Tokens the prompt may hold before the history is compacted.
pub fn limit(budget: &ContextBudget, n_ctx: Option<u32>) -> u64 {
    let working = (f64::from(budget.working_max) * f64::from(budget.compact_at)) as u64;
    n_ctx
        .and_then(|n| room(budget, n))
        .map_or(working, |room| room.min(working))
}

/// What a slot of `n_ctx` leaves the prompt beside a step's `max_tokens` and a summary
/// request; `None` when that is too little for compaction to help.
fn room(budget: &ContextBudget, n_ctx: u32) -> Option<u64> {
    Some(u64::from(n_ctx).saturating_sub(u64::from(budget.max_tokens) + SUMMARY_PROMPT))
        .filter(|&room| room >= MIN_ROOM)
}

/// The budget of a session ([`ContextBudget::working`]): its working context is `chosen`,
/// or else the slot's window, or else the profile's. A window that leaves compaction too
/// little room is no working context, and the profile's stands.
pub fn session(budget: ContextBudget, chosen: Option<u32>, n_ctx: Option<u32>) -> ContextBudget {
    let window = n_ctx.filter(|&n| room(&budget, n).is_some());
    budget.working(chosen, window)
}

/// Estimated tokens of messages.
pub fn estimate(messages: &[ChatMessage]) -> u64 {
    messages.iter().map(bytes).sum::<u64>() / BYTES_PER_TOKEN
}

fn bytes(m: &ChatMessage) -> u64 {
    // an image counts as the tokens its projector makes of it
    let images: u64 = m.images().iter().map(Image::tokens).sum();
    let n = match m {
        ChatMessage::System(t) | ChatMessage::User { text: t, .. } => t.len(),
        ChatMessage::Assistant {
            content,
            reasoning,
            tool_calls,
            ..
        } => {
            content.len()
                + reasoning.as_ref().map_or(0, String::len)
                + tool_calls
                    .iter()
                    .map(|c| c.name.len() + c.arguments.len())
                    .sum::<usize>()
        }
        ChatMessage::Tool { content, .. } => content.len(),
    };
    n as u64 + images * BYTES_PER_TOKEN
}

/// Where the kept steps begin: at the earliest of the turn's last steps that fit in
/// `budget` tokens together, at most [`MAX_KEPT_STEPS`] of them and always the last one. A
/// step begins with the model's message; `turn` is the index of the turn's request. With
/// no step after it, nothing is kept: the end of `messages`.
pub fn kept_from(messages: &[ChatMessage], turn: usize, budget: u64) -> usize {
    let starts: Vec<usize> = (turn + 1..messages.len())
        .filter(|&i| matches!(messages[i], ChatMessage::Assistant { .. }))
        .collect();
    let Some(&last) = starts.last() else {
        return messages.len();
    };
    let mut from = last;
    for &start in starts.iter().rev().skip(1).take(MAX_KEPT_STEPS - 1) {
        if estimate(&messages[start..]) > budget {
            break;
        }
        from = start;
    }
    from
}

/// Cuts the tool results of kept steps to an even share of `budget` tokens when together
/// they do not fit in it.
pub fn fit(kept: &mut [ChatMessage], budget: u64) {
    if estimate(kept) <= budget {
        return;
    }
    let results = kept
        .iter()
        .filter(|m| matches!(m, ChatMessage::Tool { .. }))
        .count();
    if results == 0 {
        return;
    }
    let share = (budget * BYTES_PER_TOKEN) as usize / results;
    for m in kept {
        if let ChatMessage::Tool { content, .. } = m
            && content.len() > share
        {
            let head = head(content, share);
            let more = content.len() - head.len();
            *content = format!(
                "{head}\n[ognivo] Cut in compaction: {more} more bytes of this result. Make the call again if you need them."
            );
        }
    }
}

/// The start of `text` within `max` bytes, ending at a line end when one is near.
fn head(text: &str, max: usize) -> &str {
    let mut end = max.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    match text[..end].rfind('\n') {
        Some(nl) if nl >= end / 2 => &text[..nl],
        _ => &text[..end],
    }
}

/// The message that replaces the earlier history. `steps_follow`: the turn's last steps
/// come after it.
pub fn note(
    requests: &[String],
    summary: Option<&str>,
    facts: &[String],
    steps_follow: bool,
) -> String {
    let mut s = String::from(
        "[ognivo] The conversation was compacted to fit the context: this message replaces the earlier ones",
    );
    s.push_str(if steps_follow {
        ", and your latest steps follow as they were."
    } else {
        "."
    });
    if !requests.is_empty() {
        let skipped = requests.len().saturating_sub(MAX_REQUESTS);
        s.push_str("\n\nThe user's requests, oldest first");
        if skipped > 0 {
            s.push_str(&format!(" (the {skipped} before these are in the summary)"));
        }
        s.push_str(":\n");
        for (i, r) in requests.iter().enumerate().skip(skipped) {
            s.push_str(&format!("{}. {}\n", i + 1, clip(r.trim(), REQUEST_CHARS)));
        }
    }
    match summary {
        Some(text) => s.push_str(&format!(
            "\nYour summary of the work so far, written before the compaction:\n{}\n",
            text.trim()
        )),
        None => s.push_str(
            "\nThere is no summary: writing it failed. Rely on the facts below, and read files again where you need them.\n",
        ),
    }
    if !facts.is_empty() {
        s.push_str("\nWhat ognivo knows for certain:\n");
        for f in facts {
            s.push_str(&format!("- {f}\n"));
        }
    }
    s.push_str(if steps_follow {
        "\nGo on with the latest request from where the steps below leave off."
    } else {
        "\nGo on with the latest request."
    });
    s
}

/// `+3 -1`, or `new, 25 lines`, for the facts of a note.
pub fn counts(c: &FileChange) -> String {
    let removed: usize = c.hunks.iter().map(|h| h.removed.len()).sum();
    let added: usize = c.hunks.iter().map(|h| h.added.len()).sum();
    if c.created {
        format!("new, {added} line{}", if added == 1 { "" } else { "s" })
    } else {
        format!("+{added} -{removed}")
    }
}

fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use ognivo_models::ToolCall;

    use super::*;

    fn budget(working_max: u32, compact_at: f32, max_tokens: u32) -> ContextBudget {
        ContextBudget {
            working_max,
            compact_at,
            max_tokens,
            ceiling: working_max,
        }
    }

    #[test]
    fn a_session_works_with_the_window_that_leaves_room() {
        let mut b = budget(65_536, 0.65, 16_384);
        b.ceiling = 262_144;
        // the window, and its share
        let s = session(b, None, Some(262_144));
        assert_eq!(s.working_max, 262_144);
        assert_eq!(limit(&s, Some(262_144)), 170_393);
        // the user's choice, within the window
        assert_eq!(session(b, Some(32_768), Some(262_144)).working_max, 32_768);
        // a window too small to compact in is no working context
        assert_eq!(session(b, None, Some(4096)).working_max, 65_536);
        assert_eq!(session(b, None, None).working_max, 65_536);
    }

    fn step(id: &str, result: &str) -> [ChatMessage; 2] {
        [
            ChatMessage::Assistant {
                content: String::new(),
                reasoning: Some("think".into()),
                tool_calls: vec![ToolCall {
                    id: id.into(),
                    name: "read".into(),
                    arguments: "{}".into(),
                }],
                tokens: None,
            },
            ChatMessage::tool(id, result),
        ]
    }

    #[test]
    fn the_limit_is_the_working_budget_or_the_room_the_slot_leaves() {
        let gpt_oss = budget(98_304, 0.70, 16_384);
        assert_eq!(limit(&gpt_oss, None), 68_812);
        assert_eq!(limit(&gpt_oss, Some(131_072)), 68_812);
        // a 64K slot must keep room for a whole step and the summary request
        assert_eq!(limit(&gpt_oss, Some(65_536)), 65_536 - 16_384 - 512);
        // a slot too small for that cannot be helped: the budget decides
        assert_eq!(limit(&gpt_oss, Some(16_384)), 68_812);
    }

    #[test]
    fn an_image_counts_as_the_tokens_its_projector_makes() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(1280u32.to_be_bytes());
        png.extend(720u32.to_be_bytes());
        let image = Image::new(&png).unwrap();
        let m = ChatMessage::User {
            text: "x".repeat(400),
            images: vec![image.clone(), image],
        };
        assert_eq!(estimate(&[m]), 400 / BYTES_PER_TOKEN + 2 * 900);
    }

    #[test]
    fn the_last_steps_are_kept_as_far_as_they_fit() {
        let mut m = vec![
            ChatMessage::System("s".into()),
            ChatMessage::user("old request"),
            ChatMessage::Assistant {
                content: "old answer".into(),
                reasoning: None,
                tool_calls: Vec::new(),
                tokens: None,
            },
            ChatMessage::user("fix it"),
        ];
        // the turn has no step yet: nothing to keep
        assert_eq!(kept_from(&m, 3, 1000), 4);
        m.extend(step("a", &"x".repeat(3000)));
        m.push(ChatMessage::user("[ognivo] a note"));
        m.extend(step("b", "short"));
        m.extend(step("c", "short"));
        // all three fit
        assert_eq!(kept_from(&m, 3, 2000), 4);
        // the big first one does not
        assert_eq!(kept_from(&m, 3, 100), 7);
        // the last step always stays, even alone over the budget
        assert_eq!(kept_from(&m, 3, 0), 9);
    }

    #[test]
    fn results_too_long_for_the_budget_are_cut_with_a_note() {
        let long = "line\n".repeat(1000);
        let mut kept: Vec<ChatMessage> = step("a", &long).into_iter().collect();
        fit(&mut kept, 100);
        let ChatMessage::Tool { content, .. } = &kept[1] else {
            panic!();
        };
        assert!(content.starts_with("line\nline\n"));
        assert!(
            content.ends_with(
                "\n[ognivo] Cut in compaction: 4701 more bytes of this result. Make the call again if you need them."
            ),
            "{content}"
        );
        let mut small: Vec<ChatMessage> = step("a", "ok").into_iter().collect();
        let before = small.clone();
        fit(&mut small, 100);
        assert_eq!(small, before);
    }

    #[test]
    fn the_note_holds_the_requests_the_summary_and_the_facts() {
        let requests: Vec<String> = (1..=10).map(|i| format!("request {i}")).collect();
        let note = note(
            &requests,
            Some("Task: x\nNext: y\n"),
            &["verified: `cargo test` passed after the last change".into()],
            true,
        );
        assert!(note.starts_with(
            "[ognivo] The conversation was compacted to fit the context: this message replaces the earlier ones, and your latest steps follow as they were.\n\nThe user's requests, oldest first (the 2 before these are in the summary):\n3. request 3\n"
        ), "{note}");
        assert!(note.contains("10. request 10\n\nYour summary of the work so far, written before the compaction:\nTask: x\nNext: y\n\nWhat ognivo knows for certain:\n- verified: `cargo test` passed after the last change\n\nGo on with the latest request from where the steps below leave off."), "{note}");
        let bare = super::note(&["only".into()], None, &[], false);
        assert!(
            bare.contains("There is no summary: writing it failed."),
            "{bare}"
        );
        assert!(bare.ends_with("\nGo on with the latest request."), "{bare}");
    }
}
