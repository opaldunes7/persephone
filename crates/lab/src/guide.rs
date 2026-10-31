//! The guide for agents, served by Lab so an agent with no access to the docs folder can still read
//! it, a topic at a time so it costs little of an agent's context.

use persephone_research::settings::Settings;
use serde_json::{Value, json};

const GUIDE: &str = include_str!("../../../docs/lab-agents.md");

/// The guide's opening, before its first topic.
fn overview() -> &'static str {
    GUIDE.split("\n## ").next().unwrap_or(GUIDE).trim()
}

/// Each topic's name and text, in order. A topic is a `##` heading.
#[must_use]
pub fn topics() -> Vec<(&'static str, &'static str)> {
    GUIDE
        .split("\n## ")
        .skip(1)
        .filter_map(|section| {
            let (name, text) = section.split_once('\n')?;
            Some((name.trim(), text.trim()))
        })
        .collect()
}

/// The topic's first sentence, or its first line if that comes first, to say what's in it.
fn summary(text: &str) -> &str {
    let paragraph = text.split("\n\n").next().unwrap_or_default();
    match paragraph.find(". ") {
        Some(end) => &paragraph[..=end],
        None => paragraph.lines().next().unwrap_or_default(),
    }
}

/// The `get_guide` tool: the overview and list of topics, one topic, or all of it.
///
/// # Errors
///
/// Returns a message listing the topics if there's no such topic.
pub fn get_guide(args: &Value) -> Result<Value, String> {
    let all = topics();
    let names: Vec<&str> = all.iter().map(|(name, _)| *name).collect();
    match args.get("topic").and_then(Value::as_str) {
        None => {
            let listed: Vec<Value> = all
                .iter()
                .map(|(name, text)| json!({"topic": name, "about": summary(text)}))
                .collect();
            Ok(json!({"guide": overview(), "topics": listed}))
        }
        Some("all") => Ok(json!({"guide": GUIDE, "topics": names})),
        Some(topic) => all
            .iter()
            .find(|(name, _)| *name == topic)
            .map(|(_, text)| json!({"guide": text, "topics": names}))
            .ok_or_else(|| format!("`topic`: there's no `{topic}`; there are {names:?} and `all`")),
    }
}

/// The `get_settings` tool.
#[must_use]
pub fn get_settings() -> Value {
    let settings = serde_json::to_value(Settings::default()).unwrap_or(Value::Null);
    json!({"settings": settings})
}
