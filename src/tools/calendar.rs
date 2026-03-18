use agenkit::{AgentError, Tool, ToolResult};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

/// A single calendar event stored in the local JSON store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarEntry {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub tags: Vec<String>,
}

/// Manages a personal calendar backed by a local JSON file.
///
/// Supports `create`, `list`, `get`, `delete`, and `upcoming` operations.
/// The data directory defaults to `~/.rustynail/` and can be overridden via
/// the `RUSTYNAIL_DATA_DIR` environment variable or the `data_dir` constructor.
pub struct CalendarTool {
    store_path: PathBuf,
}

impl CalendarTool {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            store_path: data_dir.join("calendar.json"),
        }
    }

    /// Resolve the data dir: `RUSTYNAIL_DATA_DIR` env var → `~/.rustynail`.
    pub fn with_default_dir() -> Self {
        let dir = std::env::var("RUSTYNAIL_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".rustynail")
            });
        Self::new(dir)
    }

    fn load_entries(&self) -> Vec<CalendarEntry> {
        if !self.store_path.exists() {
            return Vec::new();
        }
        match std::fs::read_to_string(&self.store_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    fn save_entries(&self, entries: &[CalendarEntry]) -> Result<(), String> {
        if let Some(parent) = self.store_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create data dir: {}", e))?;
        }
        let json = serde_json::to_string_pretty(entries)
            .map_err(|e| format!("serialization error: {}", e))?;
        std::fs::write(&self.store_path, json).map_err(|e| format!("write error: {}", e))?;
        Ok(())
    }
}

#[async_trait]
impl Tool for CalendarTool {
    fn name(&self) -> &str {
        "calendar"
    }

    fn description(&self) -> &str {
        "Manage a personal calendar. Operations: create, list, get, delete, upcoming. \
         Events have a title, optional description, start_time (RFC3339), optional end_time, and tags."
    }

    fn parameters_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "type": "object",
            "properties": {
                "op": {
                    "type": "string",
                    "enum": ["create", "list", "get", "delete", "upcoming"],
                    "description": "Operation to perform"
                },
                "id": {
                    "type": "string",
                    "description": "Event ID (required for get, delete)"
                },
                "title": {
                    "type": "string",
                    "description": "Event title (required for create)"
                },
                "description": {
                    "type": "string",
                    "description": "Optional event description"
                },
                "start_time": {
                    "type": "string",
                    "description": "Start time in RFC3339 format, e.g. 2026-04-01T14:00:00Z (required for create)"
                },
                "end_time": {
                    "type": "string",
                    "description": "Optional end time in RFC3339 format"
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of tags"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max number of upcoming events to return (default 10)"
                }
            },
            "required": ["op"]
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

        match op {
            "create" => {
                let title = match params.get("title").and_then(|v| v.as_str()) {
                    Some(t) => t.to_string(),
                    None => return Ok(ToolResult::error("missing required parameter 'title'")),
                };
                let start_time_str = match params.get("start_time").and_then(|v| v.as_str()) {
                    Some(s) => s,
                    None => {
                        return Ok(ToolResult::error("missing required parameter 'start_time'"))
                    }
                };
                let start_time: DateTime<Utc> = match start_time_str.parse() {
                    Ok(t) => t,
                    Err(_) => {
                        return Ok(ToolResult::error(
                            "invalid start_time; expected RFC3339 format",
                        ))
                    }
                };
                let end_time: Option<DateTime<Utc>> = params
                    .get("end_time")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok());
                let description = params
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let tags: Vec<String> = params
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();

                let entry = CalendarEntry {
                    id: Uuid::new_v4().to_string(),
                    title,
                    description,
                    start_time,
                    end_time,
                    tags,
                };
                let id = entry.id.clone();
                let mut entries = self.load_entries();
                entries.push(entry);
                if let Err(e) = self.save_entries(&entries) {
                    return Ok(ToolResult::error(e));
                }
                Ok(ToolResult::success(
                    serde_json::json!({ "id": id, "created": true }),
                ))
            }

            "list" => {
                let entries = self.load_entries();
                Ok(ToolResult::success(
                    serde_json::json!({ "events": serde_json::to_value(&entries).unwrap_or_default() }),
                ))
            }

            "get" => {
                let id = match params.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return Ok(ToolResult::error("missing required parameter 'id'")),
                };
                let entries = self.load_entries();
                match entries.iter().find(|e| e.id == id) {
                    Some(e) => Ok(ToolResult::success(
                        serde_json::to_value(e).unwrap_or_default(),
                    )),
                    None => Ok(ToolResult::error(format!("event '{}' not found", id))),
                }
            }

            "delete" => {
                let id = match params.get("id").and_then(|v| v.as_str()) {
                    Some(id) => id,
                    None => return Ok(ToolResult::error("missing required parameter 'id'")),
                };
                let mut entries = self.load_entries();
                let before = entries.len();
                entries.retain(|e| e.id != id);
                if entries.len() == before {
                    return Ok(ToolResult::error(format!("event '{}' not found", id)));
                }
                if let Err(e) = self.save_entries(&entries) {
                    return Ok(ToolResult::error(e));
                }
                Ok(ToolResult::success(serde_json::json!({ "deleted": true })))
            }

            "upcoming" => {
                let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                let now = Utc::now();
                let mut entries = self.load_entries();
                entries.retain(|e| e.start_time >= now);
                entries.sort_by_key(|e| e.start_time);
                entries.truncate(limit);
                Ok(ToolResult::success(
                    serde_json::json!({ "events": serde_json::to_value(&entries).unwrap_or_default() }),
                ))
            }

            unknown => Ok(ToolResult::error(format!("unknown op '{}'", unknown))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool() -> (CalendarTool, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let tool = CalendarTool::new(dir.path().to_path_buf());
        (tool, dir)
    }

    async fn run(tool: &CalendarTool, params: serde_json::Value) -> ToolResult {
        let map: HashMap<String, serde_json::Value> = serde_json::from_value(params).unwrap();
        tool.execute(map).await.unwrap()
    }

    #[tokio::test]
    async fn test_create_and_list() {
        let (tool, _dir) = make_tool();
        let r = run(
            &tool,
            serde_json::json!({
                "op": "create",
                "title": "Team Meeting",
                "start_time": "2030-01-15T10:00:00Z"
            }),
        )
        .await;
        assert!(r.success, "{:?}", r.error);

        let r = run(&tool, serde_json::json!({ "op": "list" })).await;
        assert!(r.success);
        let events = r.output["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["title"], "Team Meeting");
    }

    #[tokio::test]
    async fn test_get_and_delete() {
        let (tool, _dir) = make_tool();
        let r = run(
            &tool,
            serde_json::json!({
                "op": "create",
                "title": "Lunch",
                "start_time": "2030-02-01T12:00:00Z"
            }),
        )
        .await;
        let id = r.output["id"].as_str().unwrap().to_string();

        let r = run(&tool, serde_json::json!({ "op": "get", "id": id })).await;
        assert!(r.success);
        assert_eq!(r.output["title"], "Lunch");

        let r = run(&tool, serde_json::json!({ "op": "delete", "id": id })).await;
        assert!(r.success);

        let r = run(&tool, serde_json::json!({ "op": "get", "id": id })).await;
        assert!(!r.success);
    }

    #[tokio::test]
    async fn test_upcoming() {
        let (tool, _dir) = make_tool();
        run(
            &tool,
            serde_json::json!({ "op": "create", "title": "Past", "start_time": "2000-01-01T00:00:00Z" }),
        )
        .await;
        run(
            &tool,
            serde_json::json!({ "op": "create", "title": "Future", "start_time": "2099-01-01T00:00:00Z" }),
        )
        .await;

        let r = run(&tool, serde_json::json!({ "op": "upcoming" })).await;
        assert!(r.success);
        let events = r.output["events"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["title"], "Future");
    }
}
