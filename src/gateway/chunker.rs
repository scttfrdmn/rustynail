use std::collections::HashMap;

/// Platform-default character limits when not overridden by config.
const DEFAULTS: &[(&str, usize)] = &[
    ("discord", 2000),
    ("slack", 4000),
    ("teams", 1024),
    ("telegram", 4096),
    ("whatsapp", 4096),
];

/// Splits long messages into ≤limit-char chunks, breaking on whitespace when possible.
pub struct MessageChunker {
    limits: HashMap<String, usize>,
}

impl MessageChunker {
    pub fn new(limits: HashMap<String, usize>) -> Self {
        Self { limits }
    }

    /// Resolve the character limit for a given channel_id.
    fn limit_for(&self, channel_id: &str) -> Option<usize> {
        // Try exact match first
        if let Some(&n) = self.limits.get(channel_id) {
            return Some(n);
        }
        // Try config prefix match
        for (prefix, &n) in &self.limits {
            if channel_id.starts_with(prefix.as_str()) {
                return Some(n);
            }
        }
        // Fall back to built-in platform defaults
        for (prefix, n) in DEFAULTS {
            if channel_id.starts_with(prefix) {
                return Some(*n);
            }
        }
        None
    }

    /// Split `text` into chunks of at most `limit` bytes, breaking on whitespace
    /// when possible to avoid cutting in the middle of a word.
    ///
    /// The limit is counted in **bytes**, matching the platform limits in
    /// [`DEFAULTS`] conservatively: a limit read as characters could produce a
    /// chunk that is under the character count and over the byte count, which is
    /// the direction that gets a message rejected by the platform.
    ///
    /// Every split lands on a UTF-8 character boundary. Slicing by byte index
    /// against a text containing any multi-byte character — `£`, `—`, `…`, an
    /// emoji, any non-Latin script — panics when the index falls inside one, and a
    /// panic here takes down the message-handling task rather than truncating a
    /// message. quarry plan messages carry `£`/`—` routinely and Teams' limit is
    /// 1024, so this was reachable in normal use.
    pub fn chunk(&self, channel_id: &str, text: &str) -> Vec<String> {
        let limit = match self.limit_for(channel_id) {
            Some(n) => n,
            None => return vec![text.to_string()],
        };

        // A zero limit cannot make progress: every chunk would be empty and the
        // loop below would never shorten `remaining`. Treated as "do not chunk"
        // rather than spinning forever on a misconfigured limit.
        if limit == 0 || text.len() <= limit {
            return vec![text.to_string()];
        }

        let mut chunks = Vec::new();
        let mut remaining = text;

        while !remaining.is_empty() {
            if remaining.len() <= limit {
                chunks.push(remaining.to_string());
                break;
            }

            // The largest character boundary at or below the limit. `floor_char_boundary`
            // is not yet stable, so this walks back from `limit` — at most three bytes
            // for any UTF-8 sequence.
            let mut hard = limit;
            while hard > 0 && !remaining.is_char_boundary(hard) {
                hard -= 1;
            }

            // Prefer a whitespace break within that window; fall back to the hard
            // boundary when a single word fills the chunk.
            let split_at = match remaining[..hard].rfind(char::is_whitespace) {
                // A leading space would split off an empty chunk and loop forever.
                Some(0) | None => hard,
                Some(ws) => ws,
            };
            let (chunk, rest) = remaining.split_at(split_at);
            chunks.push(chunk.to_string());
            remaining = rest.trim_start();
        }

        chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_message_not_split() {
        let chunker = MessageChunker::new(HashMap::new());
        let chunks = chunker.chunk("discord-main", "hello");
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn long_discord_message_split() {
        let chunker = MessageChunker::new(HashMap::new());
        let text = "word ".repeat(500); // 2500 chars, limit 2000
        let chunks = chunker.chunk("discord-main", text.trim());
        assert!(chunks.len() >= 2);
        for chunk in &chunks {
            assert!(chunk.len() <= 2000);
        }
    }

    #[test]
    fn config_override_limit() {
        let mut limits = HashMap::new();
        limits.insert("discord".to_string(), 10);
        let chunker = MessageChunker::new(limits);
        let chunks = chunker.chunk("discord-test", "hello world foo bar");
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= 10);
        }
    }

    #[test]
    fn unknown_platform_no_split() {
        let chunker = MessageChunker::new(HashMap::new());
        let text = "x".repeat(10_000);
        let chunks = chunker.chunk("custom-channel", &text);
        assert_eq!(chunks.len(), 1);
    }

    /// A multi-byte character straddling the limit must not panic.
    ///
    /// Before the char-boundary walk, `&remaining[..limit]` panicked with "byte
    /// index N is not a char boundary" whenever a multi-byte character crossed the
    /// limit. That aborts the message-handling task, so the sender gets no reply at
    /// all rather than a split one — and it is reachable from ordinary text: `£` in
    /// a spend cap, an em dash, any emoji, any non-Latin script.
    #[test]
    fn a_multibyte_character_at_the_limit_does_not_panic() {
        let mut limits = HashMap::new();
        limits.insert("discord".to_string(), 10);
        let chunker = MessageChunker::new(limits);

        // '£' occupies bytes 9..11, so byte 10 — the limit — is inside it.
        let chunks = chunker.chunk("discord-x", "abcdefgh £5 and more text past the limit");
        assert!(chunks.len() > 1);
        assert_eq!(
            chunks.concat().replace(' ', ""),
            "abcdefgh£5andmoretextpastthelimit"
        );
    }

    /// Every chunk boundary is a valid character boundary, and no chunk exceeds the
    /// limit, across a text made entirely of multi-byte characters.
    #[test]
    fn chunks_never_split_a_character_or_exceed_the_limit() {
        let mut limits = HashMap::new();
        limits.insert("teams".to_string(), 7);
        let chunker = MessageChunker::new(limits);

        // 4-byte characters: no split at 7 can land on a boundary by luck.
        let text = "🎉🎉🎉🎉🎉🎉🎉🎉";
        let chunks = chunker.chunk("teams-1", text);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= 7, "chunk over limit: {} bytes", chunk.len());
            // Reconstructing proves nothing was cut mid-character: an invalid
            // split would have panicked above rather than produced a bad String.
            assert!(!chunk.is_empty());
        }
        assert_eq!(chunks.concat(), text);
    }

    /// A single unbroken word longer than the limit still terminates.
    ///
    /// The whitespace search finds nothing, so this exercises the hard-boundary
    /// fallback. Worth a test because the natural fix for the leading-space case —
    /// splitting at the found whitespace unconditionally — makes a leading space
    /// produce an empty chunk and loop forever.
    #[test]
    fn an_unbreakable_word_still_terminates() {
        let mut limits = HashMap::new();
        limits.insert("discord".to_string(), 5);
        let chunker = MessageChunker::new(limits);

        let chunks = chunker.chunk("discord-x", "aaaaaaaaaaaaaaaaa");
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks.concat(), "aaaaaaaaaaaaaaaaa");
    }

    /// A zero limit is treated as "do not chunk" rather than looping forever.
    #[test]
    fn a_zero_limit_does_not_hang() {
        let mut limits = HashMap::new();
        limits.insert("discord".to_string(), 0);
        let chunker = MessageChunker::new(limits);
        assert_eq!(chunker.chunk("discord-x", "hello"), vec!["hello"]);
    }
}
