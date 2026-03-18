/// Transforms markdown responses into platform-native formats.
pub struct ResponseFormatter {
    enabled: bool,
}

impl ResponseFormatter {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }

    /// Format `text` for delivery on `channel_id`.
    ///
    /// When formatting is disabled, the text is returned unchanged.
    /// Code blocks are protected from inline substitutions.
    pub fn format(&self, text: &str, channel_id: &str) -> String {
        if !self.enabled {
            return text.to_string();
        }

        let platform = channel_id.split('-').next().unwrap_or("");

        match platform {
            "discord" | "teams" => text.to_string(),
            "slack" => format_slack(text),
            "telegram" => format_telegram(text),
            "whatsapp" => format_whatsapp(text),
            _ => text.to_string(),
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Replace fenced code blocks with unique placeholders, apply `f`, then restore.
fn protect_code_blocks(text: &str, f: impl Fn(&str) -> String) -> String {
    let mut placeholders: Vec<String> = Vec::new();
    let mut working = String::new();
    let mut rest = text;

    while let Some(start) = rest.find("```") {
        working.push_str(&rest[..start]);
        rest = &rest[start + 3..];
        if let Some(end) = rest.find("```") {
            let block = &rest[..end + 3];
            let token = format!("\x00CODEBLOCK{}\x00", placeholders.len());
            placeholders.push(format!("```{}", block));
            working.push_str(&token);
            rest = &rest[end + 3..];
        } else {
            // Unclosed code block — pass through as-is
            working.push_str("```");
            working.push_str(rest);
            rest = "";
        }
    }
    working.push_str(rest);

    let transformed = f(&working);

    // Restore code blocks
    let mut result = transformed;
    for (i, block) in placeholders.iter().enumerate() {
        result = result.replace(&format!("\x00CODEBLOCK{}\x00", i), block);
    }
    result
}

fn format_slack(text: &str) -> String {
    protect_code_blocks(text, |s| {
        // **bold** → *bold*
        let s = replace_bold_slack(s);
        // [text](url) → <url|text>
        replace_md_links_slack(&s)
    })
}

fn format_telegram(text: &str) -> String {
    protect_code_blocks(text, |s| {
        // **bold** → *bold* (Telegram MarkdownV2)
        let s = replace_bold_telegram(s);
        // Escape Telegram special characters outside code blocks
        escape_telegram_special(&s)
    })
}

fn format_whatsapp(text: &str) -> String {
    protect_code_blocks(text, |s| {
        // **bold** → *bold*
        let s = replace_bold_whatsapp(s);
        // Strip markdown links → text (url)
        replace_md_links_whatsapp(&s)
    })
}

/// `**word**` → `*word*` for Slack.
fn replace_bold_slack(s: &str) -> String {
    replace_bold_markers(s, "*")
}

/// `**word**` → `*word*` for Telegram MarkdownV2.
fn replace_bold_telegram(s: &str) -> String {
    replace_bold_markers(s, "*")
}

/// `**word**` → `*word*` for WhatsApp.
fn replace_bold_whatsapp(s: &str) -> String {
    replace_bold_markers(s, "*")
}

/// Replace `**...**` with `{marker}...{marker}`.
fn replace_bold_markers(s: &str, marker: &str) -> String {
    let mut result = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("**") {
        result.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        if let Some(end) = rest.find("**") {
            result.push_str(marker);
            result.push_str(&rest[..end]);
            result.push_str(marker);
            rest = &rest[end + 2..];
        } else {
            // Unmatched — pass through
            result.push_str("**");
            result.push_str(rest);
            rest = "";
        }
    }
    result.push_str(rest);
    result
}

/// `[text](url)` → `<url|text>` for Slack.
fn replace_md_links_slack(s: &str) -> String {
    replace_md_links(s, |text, url| format!("<{}|{}>", url, text))
}

/// `[text](url)` → `text (url)` for WhatsApp (strip markdown links).
fn replace_md_links_whatsapp(s: &str) -> String {
    replace_md_links(s, |text, url| format!("{} ({})", text, url))
}

/// Generic markdown link replacer.  Calls `transform(text, url)` for each match.
fn replace_md_links(s: &str, transform: impl Fn(&str, &str) -> String) -> String {
    let mut result = String::new();
    let mut rest = s;
    while let Some(bracket_start) = rest.find('[') {
        let before = &rest[..bracket_start];
        let after_bracket = &rest[bracket_start + 1..];
        if let Some(bracket_end) = after_bracket.find(']') {
            let link_text = &after_bracket[..bracket_end];
            let after_text = &after_bracket[bracket_end + 1..];
            if after_text.starts_with('(') {
                if let Some(paren_end) = after_text.find(')') {
                    let url = &after_text[1..paren_end];
                    result.push_str(before);
                    result.push_str(&transform(link_text, url));
                    rest = &after_text[paren_end + 1..];
                    continue;
                }
            }
        }
        // No match — emit up through the bracket and continue
        result.push_str(&rest[..bracket_start + 1]);
        rest = &rest[bracket_start + 1..];
    }
    result.push_str(rest);
    result
}

/// Escape Telegram MarkdownV2 special characters outside of bold/italic markers.
fn escape_telegram_special(s: &str) -> String {
    // Characters that must be escaped in Telegram MarkdownV2 (outside entities)
    const SPECIALS: &[char] = &['.', '!', '+', '-', '=', '|', '{', '}', '(', ')', '#'];
    let mut result = String::with_capacity(s.len());
    for ch in s.chars() {
        if SPECIALS.contains(&ch) {
            result.push('\\');
        }
        result.push(ch);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_passthrough() {
        let f = ResponseFormatter::new(false);
        let text = "**hello** [link](https://example.com)";
        assert_eq!(f.format(text, "slack-main"), text);
    }

    #[test]
    fn discord_passthrough() {
        let f = ResponseFormatter::new(true);
        let text = "**hello**";
        assert_eq!(f.format(text, "discord-general"), text);
    }

    #[test]
    fn slack_bold() {
        let f = ResponseFormatter::new(true);
        assert_eq!(f.format("**bold**", "slack-main"), "*bold*");
    }

    #[test]
    fn slack_link() {
        let f = ResponseFormatter::new(true);
        assert_eq!(
            f.format("[click](https://example.com)", "slack-main"),
            "<https://example.com|click>"
        );
    }

    #[test]
    fn whatsapp_bold_and_link() {
        let f = ResponseFormatter::new(true);
        assert_eq!(
            f.format("**hi** [link](https://x.com)", "whatsapp-main"),
            "*hi* link (https://x.com)"
        );
    }

    #[test]
    fn code_block_protected_slack() {
        let f = ResponseFormatter::new(true);
        let text = "**bold** ```**not bold**``` **also bold**";
        let result = f.format(text, "slack-main");
        // Code block content must be unchanged
        assert!(result.contains("```**not bold**```"));
        // Outside the code block bold should be converted
        assert!(result.starts_with("*bold*"));
    }
}
