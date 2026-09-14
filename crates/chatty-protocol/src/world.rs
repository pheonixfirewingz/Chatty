//! Reusable roleplay worlds. Common knowledge is always available; other facts
//! are activated by recent dialogue and expire naturally as the scene moves on.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct World {
    pub id: String,
    pub name: String,
    pub character_ids: Vec<String>,
    pub entries: Vec<WorldFact>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldFact {
    pub title: String,
    pub content: String,
    pub keywords: Vec<String>,
    pub common_knowledge: bool,
    pub enabled: bool,
    pub priority: i32,
}

impl World {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.len() > 128
            || self.name.trim().is_empty()
            || self.name.len() > 256
            || self.character_ids.len() > 128
            || self.entries.len() > 256
        {
            return Err("World name or size invalid");
        }
        for fact in &self.entries {
            if fact.title.trim().is_empty()
                || fact.title.len() > 256
                || fact.content.trim().is_empty()
                || fact.content.len() > 16384
                || fact.keywords.len() > 64
                || fact
                    .keywords
                    .iter()
                    .any(|k| k.trim().is_empty() || k.len() > 256)
                || (!fact.common_knowledge && fact.enabled && fact.keywords.is_empty())
            {
                return Err(
                    "Each fact needs a title, content, and keywords unless it is common knowledge",
                );
            }
        }
        Ok(())
    }
}

/// Deterministic, bounded selection. The caller supplies only immediate history.
/// Facts are data for the roleplay, never durable memories or instructions.
pub fn select_world_context(
    worlds: &[World],
    speaker: &str,
    recent: &str,
    budget: usize,
) -> String {
    let recent = recent.to_lowercase();
    let mut facts = Vec::new();
    for world in worlds
        .iter()
        .filter(|w| w.character_ids.iter().any(|id| id == speaker))
    {
        for fact in &world.entries {
            if fact.enabled
                && (fact.common_knowledge
                    || fact.keywords.iter().any(|key| {
                        let key = key.trim().to_lowercase();
                        !key.is_empty() && recent.contains(&key)
                    }))
            {
                facts.push((world, fact));
            }
        }
    }
    facts.sort_by(|a, b| {
        b.1.common_knowledge
            .cmp(&a.1.common_knowledge)
            .then(b.1.priority.cmp(&a.1.priority))
            .then(a.0.id.cmp(&b.0.id))
    });
    let mut result = String::new();
    for (world, fact) in facts {
        let text = format!(
            "[{} / {} / {}]\n{}\n",
            world.name,
            if fact.common_knowledge {
                "Common knowledge"
            } else {
                "Scene lore"
            },
            fact.title,
            fact.content
        );
        if result.len() + text.len() <= budget {
            result.push_str(&text);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_triggers_and_keeps_common_knowledge_first() {
        let common = WorldFact {
            title: "Common".into(),
            content: "Known".into(),
            common_knowledge: true,
            enabled: true,
            ..Default::default()
        };
        let mut world = World {
            id: "w".into(),
            name: "World".into(),
            character_ids: vec!["c".into()],
            entries: vec![common.clone()],
        };
        assert!(world.validate().is_ok());
        world.entries.push(WorldFact {
            title: "Specific".into(),
            content: "Scene".into(),
            enabled: true,
            priority: 100,
            ..Default::default()
        });
        assert!(world.validate().is_err());
        world.entries[1].keywords = vec!["scene".into()];
        assert!(world.validate().is_ok());
        let selected = select_world_context(&[world], "c", "scene", 4096);
        assert!(selected.find("Known").unwrap() < selected.find("Scene\n").unwrap());
    }

    #[test]
    fn context_is_scoped_temporary_and_bounded() {
        let world = World {
            id: "w".into(),
            name: "Realm".into(),
            character_ids: vec!["c".into()],
            entries: vec![
                WorldFact {
                    title: "Sky".into(),
                    content: "Two moons".into(),
                    common_knowledge: true,
                    enabled: true,
                    ..Default::default()
                },
                WorldFact {
                    title: "Gate".into(),
                    content: "Locked at dusk".into(),
                    keywords: vec!["gate".into()],
                    enabled: true,
                    ..Default::default()
                },
                WorldFact {
                    title: "Secret".into(),
                    content: "Hidden".into(),
                    common_knowledge: true,
                    enabled: false,
                    ..Default::default()
                },
            ],
        };
        let worlds = [world];
        let active = select_world_context(&worlds, "c", "The GATE", 4096);
        assert!(active.contains("Two moons") && active.contains("Locked at dusk"));
        assert!(!active.contains("Hidden"));
        assert!(!select_world_context(&worlds, "c", "forest", 4096).contains("Locked"));
        assert!(select_world_context(&worlds, "other", "gate", 4096).is_empty());
        assert!(select_world_context(&worlds, "c", "gate", 8).is_empty());
    }
}
