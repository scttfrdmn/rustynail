use agenkit::{AgentError, Tool, ToolResult};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;

pub struct FileSystemTool {
    root: PathBuf,
}

impl FileSystemTool {
    pub fn new(root_path: PathBuf) -> Self {
        Self { root: root_path }
    }

    /// Resolve and validate that the path stays within the sandbox root.
    fn resolve(&self, path: &str) -> Result<PathBuf, String> {
        // Reject any path containing a parent-directory (`..`) component
        let input = std::path::Path::new(path);
        for component in input.components() {
            if component == std::path::Component::ParentDir {
                return Err(format!("path '{}' escapes the sandbox root", path));
            }
        }

        let joined = self.root.join(path);

        // Canonicalize the root so we have a consistent base
        let canonical_root = self
            .root
            .canonicalize()
            .map_err(|e| format!("cannot canonicalize root: {}", e))?;

        // For existing paths use canonicalize; for non-existent ones join to canonical root
        let canonical_target = if joined.exists() {
            joined
                .canonicalize()
                .map_err(|e| format!("cannot resolve path: {}", e))?
        } else {
            canonical_root.join(path.trim_start_matches('/').trim_start_matches('\\'))
        };

        if !canonical_target.starts_with(&canonical_root) {
            return Err(format!("path '{}' escapes the sandbox root", path));
        }

        Ok(canonical_target)
    }
}

#[async_trait]
impl Tool for FileSystemTool {
    fn name(&self) -> &str {
        "filesystem"
    }

    fn description(&self) -> &str {
        "Read, write, list, and check existence of files within a sandboxed directory."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["read", "write", "list", "exists"],
                    "description": "Operation to perform"
                },
                "path": {
                    "type": "string",
                    "description": "Relative path within the sandbox root"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write (required for write op)"
                }
            },
            "required": ["op", "path"]
        }))
    }

    async fn execute(
        &self,
        params: HashMap<String, serde_json::Value>,
    ) -> Result<ToolResult, AgentError> {
        let op = match params.get("op").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => return Ok(ToolResult::error("missing required parameter 'op'")),
        };

        let path_str = match params.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return Ok(ToolResult::error("missing required parameter 'path'")),
        };

        let resolved = match self.resolve(path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(e)),
        };

        match op {
            "read" => match std::fs::read_to_string(&resolved) {
                Ok(content) => Ok(ToolResult::success(
                    serde_json::json!({ "content": content }),
                )),
                Err(e) => Ok(ToolResult::error(format!("read failed: {}", e))),
            },
            "write" => {
                let content = match params.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => {
                        return Ok(ToolResult::error(
                            "missing required parameter 'content' for write op",
                        ))
                    }
                };
                // Create parent directories if needed
                if let Some(parent) = resolved.parent() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        return Ok(ToolResult::error(format!(
                            "failed to create directories: {}",
                            e
                        )));
                    }
                }
                match std::fs::write(&resolved, content) {
                    Ok(()) => Ok(ToolResult::success(
                        serde_json::json!({ "written": content.len() }),
                    )),
                    Err(e) => Ok(ToolResult::error(format!("write failed: {}", e))),
                }
            }
            "list" => match std::fs::read_dir(&resolved) {
                Ok(entries) => {
                    let names: Vec<String> = entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .collect();
                    Ok(ToolResult::success(serde_json::json!({ "entries": names })))
                }
                Err(e) => Ok(ToolResult::error(format!("list failed: {}", e))),
            },
            "exists" => Ok(ToolResult::success(
                serde_json::json!({ "exists": resolved.exists() }),
            )),
            unknown => Ok(ToolResult::error(format!("unknown op '{}'", unknown))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_tool(root: &std::path::Path) -> FileSystemTool {
        FileSystemTool::new(root.to_path_buf())
    }

    async fn run(tool: &FileSystemTool, op: &str, path: &str, content: Option<&str>) -> ToolResult {
        let mut params = HashMap::new();
        params.insert("op".to_string(), serde_json::json!(op));
        params.insert("path".to_string(), serde_json::json!(path));
        if let Some(c) = content {
            params.insert("content".to_string(), serde_json::json!(c));
        }
        tool.execute(params).await.unwrap()
    }

    #[tokio::test]
    async fn test_write_and_read() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());

        let w = run(&tool, "write", "hello.txt", Some("hello world")).await;
        assert!(w.success, "{:?}", w.error);
        assert_eq!(w.output["written"].as_u64().unwrap(), 11);

        let r = run(&tool, "read", "hello.txt", None).await;
        assert!(r.success);
        assert_eq!(r.output["content"].as_str().unwrap(), "hello world");
    }

    #[tokio::test]
    async fn test_exists() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());

        let r = run(&tool, "exists", "missing.txt", None).await;
        assert!(r.success);
        assert!(!r.output["exists"].as_bool().unwrap());

        run(&tool, "write", "present.txt", Some("x")).await;
        let r = run(&tool, "exists", "present.txt", None).await;
        assert!(r.success);
        assert!(r.output["exists"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_list() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());

        run(&tool, "write", "a.txt", Some("a")).await;
        run(&tool, "write", "b.txt", Some("b")).await;

        let r = run(&tool, "list", ".", None).await;
        assert!(r.success);
        let entries: Vec<String> = serde_json::from_value(r.output["entries"].clone()).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[tokio::test]
    async fn test_path_escape_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());

        let r = run(&tool, "read", "../secret.txt", None).await;
        assert!(!r.success);
        assert!(r.error.unwrap().contains("escapes"));
    }

    #[tokio::test]
    async fn test_read_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());

        let r = run(&tool, "read", "nonexistent.txt", None).await;
        assert!(!r.success);
    }
}
