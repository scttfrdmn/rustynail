pub mod calculator;
pub mod filesystem;
pub mod web_search;

pub use agenkit::{Tool, ToolResult};

use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::Arc;

/// Registry of tools available to agents.
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool. Returns an error if a tool with the same name is already registered.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<()> {
        let name = tool.name().to_string();
        if self.tools.contains_key(&name) {
            return Err(anyhow!("tool '{}' is already registered", name));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    pub fn all(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.values().cloned().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::calculator::CalculatorTool;

    #[test]
    fn test_register_and_get() {
        let mut registry = ToolRegistry::new();
        assert!(registry.is_empty());

        let tool = Arc::new(CalculatorTool);
        registry.register(tool).unwrap();

        assert!(!registry.is_empty());
        assert!(registry.get("calculator").is_some());
        assert_eq!(registry.names().len(), 1);
    }

    #[test]
    fn test_duplicate_registration_fails() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CalculatorTool)).unwrap();
        let err = registry.register(Arc::new(CalculatorTool)).unwrap_err();
        assert!(err.to_string().contains("already registered"));
    }

    #[test]
    fn test_all_returns_all_tools() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(CalculatorTool)).unwrap();
        assert_eq!(registry.all().len(), 1);
    }
}
