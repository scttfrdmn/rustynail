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

    /// Split `text` into chunks of at most `limit` characters, breaking on whitespace
    /// when possible to avoid cutting in the middle of a word.
    pub fn chunk(&self, channel_id: &str, text: &str) -> Vec<String> {
        let limit = match self.limit_for(channel_id) {
            Some(n) => n,
            None => return vec![text.to_string()],
        };

        if text.len() <= limit {
            return vec![text.to_string()];
        }

        let mut chunks = Vec::new();
        let mut remaining = text;

        while !remaining.is_empty() {
            if remaining.len() <= limit {
                chunks.push(remaining.to_string());
                break;
            }

            // Find a whitespace boundary within the limit
            let slice = &remaining[..limit];
            let split_at = slice.rfind(char::is_whitespace).unwrap_or(limit);
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
}
