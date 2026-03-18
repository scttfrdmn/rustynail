use std::path::Path;
use tracing::{debug, info, warn};

/// A single skill loaded from a `SKILL.md` file on disk.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Directory name used as the skill identifier.
    pub name: String,
    /// Full contents of the `SKILL.md` file.
    pub content: String,
}

/// Registry of skills discovered from configured paths.
///
/// Skills are SKILL.md files nested one level inside search paths:
/// ```text
/// skills/
///   rustynail-assistant/SKILL.md
///   formatting/SKILL.md
/// ```
#[derive(Default)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Walk each path in `dirs`, discover `<dir>/<name>/SKILL.md` files, and
    /// load them into the registry. Returns the number of skills loaded.
    pub fn discover_skills(&mut self, dirs: &[String]) -> usize {
        let mut loaded = 0;
        for dir in dirs {
            // Expand ~ in paths
            let expanded = expand_tilde(dir);
            let base = Path::new(&expanded);
            if !base.exists() {
                debug!("Skills path does not exist, skipping: {}", expanded);
                continue;
            }

            let read_dir = match std::fs::read_dir(base) {
                Ok(rd) => rd,
                Err(e) => {
                    warn!("Failed to read skills directory '{}': {}", expanded, e);
                    continue;
                }
            };

            for entry in read_dir.flatten() {
                let skill_dir = entry.path();
                if !skill_dir.is_dir() {
                    continue;
                }
                let skill_file = skill_dir.join("SKILL.md");
                if !skill_file.exists() {
                    continue;
                }
                match std::fs::read_to_string(&skill_file) {
                    Ok(content) => {
                        let name = skill_dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        info!("Loaded skill: {} ({})", name, skill_file.display());
                        self.skills.push(Skill { name, content });
                        loaded += 1;
                    }
                    Err(e) => {
                        warn!(
                            "Failed to read skill file '{}': {}",
                            skill_file.display(),
                            e
                        );
                    }
                }
            }
        }
        loaded
    }

    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }

    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Select up to `max_active` skills and build a system-prompt addendum.
    ///
    /// The returned string is intended to be appended to the agent's system
    /// prompt so the LLM is aware of available skills and their guidance.
    pub fn build_skill_context(&self, max_active: usize) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let selected = &self.skills[..self.skills.len().min(max_active)];
        let mut parts = vec![
            "\n\n--- Skills / Behavioral Guidance ---".to_string(),
        ];
        for skill in selected {
            parts.push(format!("\n## {}\n{}", skill.name, skill.content));
        }
        Some(parts.join(""))
    }
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        std::env::var("HOME")
            .map(|home| format!("{}/{}", home, rest))
            .unwrap_or_else(|_| path.to_string())
    } else {
        path.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_discover_skills() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("test-skill");
        std::fs::create_dir(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "Be helpful.").unwrap();

        let mut registry = SkillRegistry::new();
        let count = registry.discover_skills(&[tmp.path().to_str().unwrap().to_string()]);
        assert_eq!(count, 1);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.skills()[0].name, "test-skill");
        assert_eq!(registry.skills()[0].content, "Be helpful.");
    }

    #[test]
    fn test_build_skill_context() {
        let mut registry = SkillRegistry::new();
        registry.skills.push(Skill {
            name: "test".to_string(),
            content: "Always be kind.".to_string(),
        });
        let ctx = registry.build_skill_context(3).unwrap();
        assert!(ctx.contains("Always be kind."));
    }
}
