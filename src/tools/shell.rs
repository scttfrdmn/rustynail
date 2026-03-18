use agenkit::{AgentError, Tool, ToolResult};
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::process::Command;
use tokio::time::timeout;

const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Shell configuration injected at construction time.
#[derive(Clone, Debug)]
pub struct ShellToolConfig {
    /// Whether to require user approval before executing (default: true).
    pub require_approval: bool,
    /// If non-empty, commands must match one of these prefixes.
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

        // Two-step approval gate
        if self.config.require_approval && !approved {
            return Ok(ToolResult::success(serde_json::json!(format!(
                "Pending approval: `{}`\n\nCall again with approved=true to execute.",
                command
            ))));
        }

        // Allowed-command prefix check
        if !self.config.allowed_commands.is_empty() {
            let allowed = self
                .config
                .allowed_commands
                .iter()
                .any(|prefix| command.starts_with(prefix.as_str()));
            if !allowed {
                return Ok(ToolResult::error(format!(
                    "Command not in allowlist: `{}`",
                    command
                )));
            }
        }

        // Build the subprocess
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(&command);
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
            Ok(Err(e)) => Ok(ToolResult::error(format!(
                "Failed to spawn command: {}",
                e
            ))),
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
        assert!(result.output.as_str().unwrap_or("").contains("Pending approval"));
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
}
