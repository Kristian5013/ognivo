//! Repetition guard: stops a generation that has fallen into a loop instead of letting it
//! run to `max_tokens` (M1 stamp A: Gemma repeated one 56-byte fragment of a write argument
//! for 34 KB until the limit, and a 154-byte one in its reasoning until the run's timeout).
//!
//! A stream loops when its tail is exactly periodic: for some period p of at most
//! [`MAX_PERIOD`] bytes the last `span` bytes repeat with period p, where span is at least
//! [`MIN_SPAN`] and at least [`MIN_REPEATS`] periods. Calibrated on every recorded
//! generation of M0 and M1 stamp A (2192 texts, 4.7 MB): the longest such tail outside the
//! two loops was 289 bytes, the loops reached 4829 and 34644.
//!
//! Near-repetition is left alone on purpose: models rewrite a block they drafted (M0, a
//! good Gemma step with no new 8-gram over 4 KB), and long reasoning that keeps finding new
//! things is a budget question, not a loop (gpt-oss, cforge-cpuset-tests).

/// Longest period checked, in bytes.
pub const MAX_PERIOD: usize = 2048;
/// Shortest periodic tail that counts as a loop, in bytes.
pub const MIN_SPAN: usize = 2048;
/// Fewest whole periods in a loop.
pub const MIN_REPEATS: usize = 4;
/// Bytes of the period shown in [`Loop::sample`].
const SAMPLE_BYTES: usize = 120;

/// The loop a stream fell into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loop {
    /// Period in bytes.
    pub period: usize,
    /// Length of the periodic tail when the guard fired.
    pub span: usize,
    /// The text of one period, cut to 120 bytes.
    pub sample: String,
}

/// Watches one stream of text (reasoning, answer text, the arguments of one call).
#[derive(Debug, Clone, Default)]
pub struct Repeats {
    /// The last bytes, at least [`MAX_PERIOD`] + 1 of them once the stream is that long.
    tail: Vec<u8>,
    /// `run[p - 1]`: positions in a row, ending at the last byte, whose byte equals the one
    /// p bytes before it.
    run: Vec<u32>,
}

impl Repeats {
    /// Feeds the next fragment; `Some` once the stream loops.
    pub fn push(&mut self, fragment: &str) -> Option<Loop> {
        for &b in fragment.as_bytes() {
            if self.tail.len() > 4 * MAX_PERIOD {
                self.tail.drain(..self.tail.len() - MAX_PERIOD);
            }
            self.tail.push(b);
            let n = self.tail.len();
            let max = (n - 1).min(MAX_PERIOD);
            if self.run.len() < max {
                self.run.resize(max, 0);
            }
            // before[max - p] is the byte p positions back
            let before = &self.tail[n - 1 - max..n - 1];
            for (p, run) in (1..=max).zip(self.run.iter_mut()) {
                *run = if before[max - p] == b { *run + 1 } else { 0 };
                let span = *run as usize + p;
                if span >= MIN_SPAN.max(MIN_REPEATS * p) {
                    return Some(self.found(p, span));
                }
            }
        }
        None
    }

    fn found(&self, period: usize, span: usize) -> Loop {
        let one = &self.tail[self.tail.len() - period..];
        let shown = &one[..one.len().min(SAMPLE_BYTES)];
        let sample = String::from_utf8_lossy(shown)
            .trim_matches(char::REPLACEMENT_CHARACTER)
            .to_owned();
        Loop {
            period,
            span,
            sample,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `text` in pieces of `chunk` characters; the bytes fed when the guard fired.
    fn feed(r: &mut Repeats, text: &str, chunk: usize) -> Option<(usize, Loop)> {
        let chars: Vec<char> = text.chars().collect();
        let mut fed = 0;
        for piece in chars.chunks(chunk) {
            let piece: String = piece.iter().collect();
            fed += piece.len();
            if let Some(l) = r.push(&piece) {
                return Some((fed, l));
            }
        }
        None
    }

    /// Text without any period: a linear congruential generator over a..z.
    fn noise(len: usize, mut x: u32) -> String {
        (0..len)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                char::from(b'a' + ((x >> 16) % 26) as u8)
            })
            .collect()
    }

    #[test]
    fn the_stamp_a_loop_is_stopped_after_two_kilobytes() {
        // Gemma, misc-ai-chat-multiturn: the 56-byte period of its write argument
        let head = "def encode_chat(messages):\n    ids = [BOS]\n    for m in messages:\n";
        let period = "tok\":256,\"i for i, tok in enumerate(SPECIALS) if False,\n";
        assert_eq!(period.len(), 56);
        let text = format!("{head}{}", period.repeat(600));
        let (fed, l) = feed(&mut Repeats::default(), &text, 7).unwrap();
        assert_eq!((l.period, l.span), (56, MIN_SPAN));
        assert!(fed < head.len() + MIN_SPAN + 8, "{fed}");
        assert!(period.repeat(2).contains(&l.sample), "{:?}", l.sample);
    }

    #[test]
    fn long_periods_need_four_repeats() {
        let block = noise(700, 7);
        let mut r = Repeats::default();
        assert!(feed(&mut r, &block.repeat(3), 64).is_none());
        let (_, l) = feed(&mut r, &block, 64).unwrap();
        assert_eq!((l.period, l.span), (700, 2800));
        assert_eq!(l.sample.len(), 120);
    }

    #[test]
    fn ordinary_text_and_short_runs_pass() {
        let mut r = Repeats::default();
        // separators, padding and a table: periodic, but far below the span
        let text = format!(
            "{}\n{}\n| a | b |\n|---|---|\n{}\n",
            "=".repeat(200),
            "0x00, ".repeat(48),
            "    ".repeat(30)
        );
        assert!(feed(&mut r, &text, 5).is_none());
        let lines: String = (0..2000)
            .map(|i| {
                format!(
                    "step {i}: check value {} against {};\n",
                    i * 31 % 97,
                    i % 13
                )
            })
            .collect();
        assert!(feed(&mut r, &lines, 11).is_none());
        assert!(feed(&mut r, &noise(20_000, 1), 4).is_none());
    }

    #[test]
    fn multibyte_periods_give_a_clean_sample() {
        let (_, l) = feed(&mut Repeats::default(), &"повтор ".repeat(400), 3).unwrap();
        assert_eq!(l.period, "повтор ".len());
        assert!(!l.sample.is_empty() && !l.sample.contains(char::REPLACEMENT_CHARACTER));
    }
}
