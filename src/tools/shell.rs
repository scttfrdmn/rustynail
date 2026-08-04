use agenkit::{AgentError, Tool, ToolResult};
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::process::Command;
use tokio::time::timeout;

const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Shell metacharacters rejected while an allowlist is active.
///
/// An allowlisted command is exec'd directly rather than through `sh -c`, so
/// these characters no longer carry meaning. Rejecting them outright — instead
/// of passing them through as literal argv bytes — means a caller attempting
/// `git status; rm -rf ~` gets a clear error rather than a puzzling one.
const SHELL_METACHARS: &[char] = &[';', '|', '&', '$', '`', '>', '<', '\n', '\r'];

/// Shell configuration injected at construction time.
#[derive(Clone, Debug)]
pub struct ShellToolConfig {
    /// Whether to require user approval before executing (default: true).
    pub require_approval: bool,
    /// If non-empty, restricts execution to these commands.
    ///
    /// Entries are matched token-wise against the parsed argv, not as raw
    /// string prefixes: `git` permits any git invocation, while `git status`
    /// permits only that subcommand. A non-empty allowlist also switches
    /// execution from `sh -c` to a direct exec, so shell features
    /// (pipes, redirection, substitution, globbing) are unavailable.
    pub allowed_commands: Vec<String>,
}

impl Default for ShellToolConfig {
    fn default() -> Self {
        Self {
            require_approval: true,
            allowed_commands: Vec::new(),
        }
    }
}

/// Split a command into argv, honouring single and double quotes.
///
/// Deliberately not a shell parser: quotes group arguments containing spaces,
/// and nothing else is interpreted. Callers reject metacharacters before this
/// runs, so there is no expansion, substitution, or operator handling to do.
fn tokenize(command: &str) -> Result<Vec<String>, String> {
    let mut argv = Vec::new();
    let mut current = String::new();
    let mut has_token = false;
    let mut quote: Option<char> = None;

    for ch in command.chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => {
                quote = Some(ch);
                has_token = true;
            }
            None if ch.is_whitespace() => {
                if has_token {
                    argv.push(std::mem::take(&mut current));
                    has_token = false;
                }
            }
            None => {
                current.push(ch);
                has_token = true;
            }
        }
    }

    if quote.is_some() {
        return Err("unterminated quote".to_string());
    }
    if has_token {
        argv.push(current);
    }
    Ok(argv)
}

/// Whether `argv` is permitted by `allowlist`.
///
/// Each entry is tokenized and compared element-wise against the head of
/// `argv`, so an entry constrains the program and any leading subcommands it
/// names while leaving later arguments free.
fn is_allowed(argv: &[String], allowlist: &[String]) -> bool {
    allowlist.iter().any(|entry| {
        let expected = match tokenize(entry) {
            Ok(t) if !t.is_empty() => t,
            _ => return false,
        };
        argv.len() >= expected.len() && argv[..expected.len()] == expected[..]
    })
}

pub struct ShellTool {
    config: ShellToolConfig,
}

impl ShellTool {
    pub fn new(config: ShellToolConfig) -> Self {
        Self { config }
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new(ShellToolConfig::default())
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Executes a shell command and returns combined stdout + stderr. \
        Requires approved=true on second call when require_approval is enabled. \
        Parameters: command (required), working_dir (optional), \
        timeout_seconds (optional, default 30), approved (optional bool)."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "working_dir": {
                    "type": "string",
                    "description": "Working directory for the command"
                },
                "timeout_seconds": {
                    "type": "integer",
                    "description": "Execution timeout in seconds (default 30)"
                },
                "approved": {
                    "type": "boolean",
                    "description": "Set to true to confirm execution when require_approval is enabled"
                }
            },
            "required": ["command"]
        }))
    }

    async fn execute(
        &self,
        params: HashMap<String, serde_json::Value>,
    ) -> Result<ToolResult, AgentError> {
        let command = match params.get("command").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return Ok(ToolResult::error("command parameter is required")),
        };

        let working_dir = params
            .get("working_dir")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let timeout_secs = params
            .get("timeout_seconds")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let approved = params
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Allowlist check runs before the approval gate so a command that can
        // never execute is rejected outright rather than queued for approval.
        //
        // With an allowlist active the command is exec'd directly, so it must
        // be a single program invocation: metacharacters are rejected rather
        // than silently passed through as literal argv.
        let argv = if self.config.allowed_commands.is_empty() {
            None
        } else {
            if let Some(bad) = command.chars().find(|c| SHELL_METACHARS.contains(c)) {
                return Ok(ToolResult::error(format!(
                    "Command contains shell metacharacter {:?}, which is not permitted \
                     when an allowlist is configured: `{}`",
                    bad, command
                )));
            }

            let argv = match tokenize(&command) {
                Ok(a) => a,
                Err(e) => {
                    return Ok(ToolResult::error(format!(
                        "Could not parse command ({}): `{}`",
                        e, command
                    )))
                }
            };
            if argv.is_empty() {
                return Ok(ToolResult::error("command is empty"));
            }
            if !is_allowed(&argv, &self.config.allowed_commands) {
                return Ok(ToolResult::error(format!(
                    "Command not in allowlist: `{}`",
                    command
                )));
            }
            Some(argv)
        };

        // Two-step approval gate
        if self.config.require_approval && !approved {
            return Ok(ToolResult::success(serde_json::json!(format!(
                "Pending approval: `{}`\n\nCall again with approved=true to execute.",
                command
            ))));
        }

        // Build the subprocess. An allowlisted command is exec'd directly so
        // the shell never gets a chance to reinterpret it; without an allowlist
        // the caller has opted into full shell semantics via `sh -c`.
        let mut cmd = match argv {
            Some(argv) => {
                let mut cmd = Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                cmd
            }
            None => {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(&command);
                cmd
            }
        };
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        if let Some(ref dir) = working_dir {
            cmd.current_dir(dir);
        }

        // Execute with timeout
        let duration = std::time::Duration::from_secs(timeout_secs);
        let result = timeout(duration, cmd.output()).await;

        match result {
            Err(_) => Ok(ToolResult::error(format!(
                "Command timed out after {}s: `{}`",
                timeout_secs, command
            ))),
            Ok(Err(e)) => Ok(ToolResult::error(format!("Failed to spawn command: {}", e))),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
                let mut combined = String::new();
                if !stdout.is_empty() {
                    combined.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str("stderr: ");
                    combined.push_str(&stderr);
                }
                if !output.status.success() {
                    let code = output.status.code().unwrap_or(-1);
                    combined.push_str(&format!("\n[exit code: {}]", code));
                }
                if combined.is_empty() {
                    combined = "(no output)".to_string();
                }
                Ok(ToolResult::success(serde_json::json!(combined)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_name() {
        assert_eq!(ShellTool::default().name(), "shell");
    }

    #[tokio::test]
    async fn test_approval_gate() {
        let tool = ShellTool::new(ShellToolConfig {
            require_approval: true,
            allowed_commands: vec![],
        });
        let mut params = HashMap::new();
        params.insert("command".to_string(), serde_json::json!("echo hello"));
        let result = tool.execute(params).await.unwrap();
        // Without approved=true, should return pending message
        assert!(result
            .output
            .as_str()
            .unwrap_or("")
            .contains("Pending approval"));
    }

    #[tokio::test]
    async fn test_execute_with_approval() {
        let tool = ShellTool::new(ShellToolConfig {
            require_approval: false,
            allowed_commands: vec![],
        });
        let mut params = HashMap::new();
        params.insert("command".to_string(), serde_json::json!("echo hello"));
        let result = tool.execute(params).await.unwrap();
        assert!(result.output.as_str().unwrap_or("").contains("hello"));
    }

    // ── Allowlist enforcement ─────────────────────────────────────────────────

    /// Run `command` against a tool allowing exactly `allowed`, no approval gate.
    async fn run_allowlisted(allowed: &[&str], command: &str) -> ToolResult {
        let tool = ShellTool::new(ShellToolConfig {
            require_approval: false,
            allowed_commands: allowed.iter().map(|s| s.to_string()).collect(),
        });
        let mut params = HashMap::new();
        params.insert("command".to_string(), serde_json::json!(command));
        tool.execute(params).await.unwrap()
    }

    #[test]
    fn test_tokenize_splits_on_whitespace() {
        assert_eq!(tokenize("git status").unwrap(), vec!["git", "status"]);
        assert_eq!(tokenize("  ls   -la  ").unwrap(), vec!["ls", "-la"]);
    }

    #[test]
    fn test_tokenize_honours_quotes() {
        assert_eq!(
            tokenize("git commit -m 'two words'").unwrap(),
            vec!["git", "commit", "-m", "two words"]
        );
        // An empty quoted string is still an argument.
        assert_eq!(tokenize("echo ''").unwrap(), vec!["echo", ""]);
    }

    #[test]
    fn test_tokenize_rejects_unterminated_quote() {
        assert!(tokenize("echo 'unclosed").is_err());
    }

    #[test]
    fn test_is_allowed_matches_token_wise() {
        let allow = vec!["git".to_string()];
        assert!(is_allowed(&tokenize("git status").unwrap(), &allow));
        // Prefix matching would have accepted this; token matching must not.
        assert!(!is_allowed(&tokenize("gitleaks detect").unwrap(), &allow));
    }

    #[test]
    fn test_is_allowed_multi_token_entry_constrains_subcommand() {
        let allow = vec!["git status".to_string()];
        assert!(is_allowed(&tokenize("git status --short").unwrap(), &allow));
        assert!(!is_allowed(&tokenize("git push").unwrap(), &allow));
    }

    /// Regression test for the prefix-matching flaw: an allowlist entry of
    /// `git` previously permitted `git status; rm -rf ~` because the whole
    /// string was handed to `sh -c` after a `starts_with` check.
    #[tokio::test]
    async fn test_allowlist_rejects_chained_command() {
        let result = run_allowlisted(&["git"], "git status; echo pwned").await;
        assert!(!result.success, "chained command must be rejected");
        let msg = result.error.as_deref().unwrap_or("");
        assert!(
            msg.contains("metacharacter"),
            "expected metacharacter rejection, got: {}",
            msg
        );
    }

    #[tokio::test]
    async fn test_allowlist_rejects_pipe_and_substitution() {
        for command in [
            "echo hi | tee /tmp/x",
            "echo $(whoami)",
            "echo `whoami`",
            "echo hi > /tmp/x",
            "echo a && echo b",
        ] {
            let result = run_allowlisted(&["echo"], command).await;
            assert!(!result.success, "must reject: {}", command);
        }
    }

    #[tokio::test]
    async fn test_allowlist_rejects_non_allowlisted_program() {
        let result = run_allowlisted(&["echo"], "whoami").await;
        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("not in allowlist"));
    }

    #[tokio::test]
    async fn test_allowlist_permits_plain_invocation() {
        let result = run_allowlisted(&["echo"], "echo hello").await;
        assert!(result.success, "got: {:?}", result.error);
        assert!(result.output.as_str().unwrap_or("").contains("hello"));
    }

    #[tokio::test]
    async fn test_allowlisted_args_are_not_shell_interpreted() {
        // Exec'd directly, so the metacharacter-free `*` stays literal rather
        // than being glob-expanded by a shell.
        let result = run_allowlisted(&["echo"], "echo a*b").await;
        assert!(result.success);
        assert!(result.output.as_str().unwrap_or("").contains("a*b"));
    }

    #[tokio::test]
    async fn test_empty_allowlist_still_allows_shell_features() {
        // No allowlist = caller opted into full `sh -c` semantics.
        let tool = ShellTool::new(ShellToolConfig {
            require_approval: false,
            allowed_commands: vec![],
        });
        let mut params = HashMap::new();
        params.insert("command".to_string(), serde_json::json!("echo a && echo b"));
        let result = tool.execute(params).await.unwrap();
        assert!(result.success);
        let out = result.output.as_str().unwrap_or("");
        assert!(out.contains('a') && out.contains('b'), "got: {}", out);
    }

    #[tokio::test]
    async fn test_allowlist_rejection_precedes_approval_gate() {
        // A command that can never run should be rejected, not queued.
        let tool = ShellTool::new(ShellToolConfig {
            require_approval: true,
            allowed_commands: vec!["echo".to_string()],
        });
        let mut params = HashMap::new();
        params.insert("command".to_string(), serde_json::json!("rm -rf /"));
        let result = tool.execute(params).await.unwrap();
        assert!(!result.success);
        assert!(!result
            .output
            .as_str()
            .unwrap_or("")
            .contains("Pending approval"));
    }
}
