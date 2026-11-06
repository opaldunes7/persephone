//! Reading a session as it goes: Claude Code's `--output-format stream-json` lines turned into
//! what the app shows. A member's lines carry the id of the Agent call that started it, which is
//! how each is put on its own card.

use std::collections::HashMap;

use serde_json::Value;

/// Who in a session did something.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Who {
    /// The agent at the top.
    Lead,
    /// A member, by its key.
    Member(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The session started: its id, its model, and how its Lab came up, if it has one.
    Started {
        session_id: String,
        model: String,
        lab: Option<String>,
    },
    Said {
        who: Who,
        text: String,
    },
    /// A tool call, with its main arguments.
    Called {
        who: Who,
        tool: String,
        about: String,
    },
    /// Work handed to a member.
    Delegated {
        who: Who,
        to: String,
        task: String,
    },
    /// A tool call's answer.
    Answered {
        who: Who,
        tool: String,
        ok: bool,
        text: String,
    },
    /// What a member brought back.
    Returned {
        who: Who,
        from: String,
        text: String,
    },
    /// A call the sandbox or the wrap-up refused.
    Refused {
        who: Who,
        tool: String,
        why: String,
    },
    Finished(Finish),
}

impl Event {
    /// Whether it counts against a budget: a tool call, or handing out work.
    #[must_use]
    pub fn is_call(&self) -> bool {
        matches!(self, Self::Called { .. } | Self::Delegated { .. })
    }

    /// Who did it; the lead for the start and the end.
    #[must_use]
    pub fn who(&self) -> &Who {
        match self {
            Self::Said { who, .. }
            | Self::Called { who, .. }
            | Self::Delegated { who, .. }
            | Self::Answered { who, .. }
            | Self::Returned { who, .. }
            | Self::Refused { who, .. } => who,
            Self::Started { .. } | Self::Finished(_) => &Who::Lead,
        }
    }
}

/// How a session ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Finish {
    pub ok: bool,
    pub turns: u32,
    /// In US dollars at API prices, as Claude Code counts it.
    pub spend: f64,
    /// Its last words, or what went wrong.
    pub text: String,
    /// The tools it was refused, by name.
    pub refused: Vec<String>,
    pub session_id: String,
}

/// Reads one session's lines in order.
#[derive(Debug, Default)]
pub struct Reader {
    /// The member each Agent call started, by the call's id.
    started: HashMap<String, String>,
    /// Calls not answered yet: who made them, and the tool.
    open: HashMap<String, (Who, String)>,
}

/// How much of an answer is kept.
const KEEP: usize = 600;

impl Reader {
    /// What one line says; nothing for a line that isn't JSON or isn't shown.
    pub fn read(&mut self, line: &str) -> Vec<Event> {
        let Ok(line) = serde_json::from_str::<Value>(line.trim()) else {
            return Vec::new();
        };
        let who = self.who(&line);
        match line["type"].as_str() {
            Some("system") if line["subtype"] == "init" => vec![Event::Started {
                session_id: text(&line["session_id"]),
                model: text(&line["model"]),
                lab: line["mcp_servers"].as_array().and_then(|servers| {
                    servers
                        .iter()
                        .find(|s| s["name"] == "persephone-lab")
                        .map(|s| text(&s["status"]))
                }),
            }],
            Some("assistant") => line["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|block| self.said(&who, block))
                .collect(),
            Some("user") => line["message"]["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|block| self.answered(&who, block))
                .collect(),
            Some("result") => vec![Event::Finished(Finish {
                ok: line["is_error"] == false,
                turns: line["num_turns"]
                    .as_u64()
                    .and_then(|n| u32::try_from(n).ok())
                    .unwrap_or(0),
                spend: line["total_cost_usd"].as_f64().unwrap_or(0.0),
                text: text(&line["result"]),
                refused: line["permission_denials"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|d| short_tool(&text(&d["tool_name"])).to_owned())
                    .collect(),
                session_id: text(&line["session_id"]),
            })],
            _ => Vec::new(),
        }
    }

    /// Who a line is from: a member's lines name the call that started it.
    fn who(&self, line: &Value) -> Who {
        line["parent_tool_use_id"]
            .as_str()
            .and_then(|id| self.started.get(id))
            .map_or(Who::Lead, |key| Who::Member(key.clone()))
    }

    fn said(&mut self, who: &Who, block: &Value) -> Option<Event> {
        match block["type"].as_str()? {
            "text" => {
                let said = block["text"].as_str()?.trim();
                (!said.is_empty()).then(|| Event::Said {
                    who: who.clone(),
                    text: said.to_owned(),
                })
            }
            "tool_use" => {
                let id = text(&block["id"]);
                let name = text(&block["name"]);
                let input = &block["input"];
                if name == "Agent" || name == "Task" {
                    let to = input["subagent_type"]
                        .as_str()
                        .unwrap_or("general-purpose")
                        .to_owned();
                    self.started.insert(id.clone(), to.clone());
                    self.open.insert(id, (who.clone(), name));
                    let task = input["description"]
                        .as_str()
                        .or_else(|| input["prompt"].as_str())
                        .unwrap_or_default();
                    return Some(Event::Delegated {
                        who: who.clone(),
                        to,
                        task: clip(task, 160),
                    });
                }
                let tool = short_tool(&name).to_owned();
                self.open.insert(id, (who.clone(), tool.clone()));
                Some(Event::Called {
                    who: who.clone(),
                    about: about(&tool, input),
                    tool,
                })
            }
            _ => None,
        }
    }

    fn answered(&mut self, who: &Who, block: &Value) -> Option<Event> {
        if block["type"] != "tool_result" {
            return None;
        }
        let id = text(&block["tool_use_id"]);
        let (caller, tool) = self
            .open
            .remove(&id)
            .unwrap_or_else(|| (who.clone(), String::new()));
        let said = result_text(&block["content"]);
        let error = block["is_error"] == true;
        if let Some(member) = self.started.get(&id) {
            return Some(Event::Returned {
                who: caller,
                from: member.clone(),
                text: clip(&handed_back(&said), KEEP),
            });
        }
        if error && refused(&said) {
            return Some(Event::Refused {
                who: caller,
                tool,
                why: clip(hook_said(&said), 300),
            });
        }
        Some(Event::Answered {
            who: caller,
            tool,
            ok: !error,
            text: clip(&said, KEEP),
        })
    }
}

/// A tool's name without Lab's prefix: `run_backtest`.
#[must_use]
pub fn short_tool(name: &str) -> &str {
    name.strip_prefix("mcp__persephone-lab__").unwrap_or(name)
}

/// Whether a tool's name is one of Lab's.
#[must_use]
pub fn is_lab(tool: &str) -> bool {
    !matches!(
        tool,
        "Read" | "Edit" | "Write" | "Glob" | "Grep" | "Agent" | "Task" | "TodoWrite" | ""
    ) && !tool.starts_with("mcp__")
}

/// A member's answer without what Claude Code puts round it: a notice ending "The report
/// follows:", then the report with every line indented by two spaces, and after it the
/// member's id and usage.
fn handed_back(said: &str) -> String {
    let report = said
        .strip_prefix("[Subagent hand-back]")
        .and_then(|rest| rest.split_once("The report follows:"))
        .map_or(said, |(_, report)| report);
    let report = report
        .split_once("\nagentId:")
        .map_or(report, |(before, _)| before);
    report
        .lines()
        .map(|line| line.strip_prefix("  ").unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

/// What a hook said, without the command Claude Code puts first: `PreToolUse:… hook error:
/// [command]: what it said`.
fn hook_said(said: &str) -> &str {
    said.split_once("hook error: [")
        .and_then(|(_, rest)| rest.split_once("]: "))
        .map_or(said, |(_, message)| message.trim())
}

/// Whether an error is the sandbox's or the wrap-up's, not the tool's own.
fn refused(said: &str) -> bool {
    let said = said.to_lowercase();
    [
        "permission",
        "denied",
        "haven't granted",
        "not allowed",
        "blocked",
        "hook",
        "budget is used",
    ]
    .iter()
    .any(|word| said.contains(word))
}

/// The arguments that say what a call is about, briefly.
fn about(tool: &str, input: &Value) -> String {
    if let Some(path) = input["file_path"].as_str() {
        let mut parts: Vec<&str> = path.rsplit('/').take(2).collect();
        parts.reverse();
        return parts.join("/");
    }
    if let Some(pattern) = input["pattern"].as_str() {
        return clip(pattern, 80);
    }
    let mut said = Vec::new();
    for key in [
        "topic", "symbol", "symbols", "bars", "from", "to", "run_id", "test", "name", "label",
    ] {
        match &input[key] {
            Value::String(s) => said.push(s.clone()),
            Value::Number(n) => said.push(n.to_string()),
            Value::Array(items) if !items.is_empty() => said.push(
                items
                    .iter()
                    .take(4)
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            _ => {}
        }
    }
    if let Some(strategy) = input.get("strategy") {
        said.push(strategy["builtin"].as_str().map_or_else(
            || {
                strategy
                    .as_object()
                    .and_then(|o| o.keys().next())
                    .map_or_else(String::new, |k| k.clone())
            },
            str::to_owned,
        ));
    }
    said.retain(|s| !s.is_empty());
    if said.is_empty() && tool != "get_guide" {
        let compact = input.to_string();
        return if compact == "{}" || compact == "null" {
            String::new()
        } else {
            clip(&compact, 80)
        };
    }
    clip(&said.join(" "), 80)
}

/// A tool result's text, whether it came as a string or as blocks.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

/// At most `most` characters, with an ellipsis if cut.
fn clip(text: &str, most: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= most {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(most).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn lines(reader: &mut Reader, lines: &[Value]) -> Vec<Event> {
        lines
            .iter()
            .flat_map(|line| reader.read(&line.to_string()))
            .collect()
    }

    /// The start, the failure and the end of a real session, as Claude Code 2.1.295 wrote them.
    const REAL: &str = include_str!("../tests/stream-auth-failed.jsonl");

    #[test]
    fn a_real_failed_session_reads() {
        let mut reader = Reader::default();
        let events: Vec<Event> = REAL.lines().flat_map(|l| reader.read(l)).collect();
        assert!(
            matches!(&events[0], Event::Started { lab: Some(status), .. } if status == "connected")
        );
        assert!(
            matches!(&events[1], Event::Said { text, .. } if text.contains("Failed to authenticate"))
        );
        let Event::Finished(finish) = events.last().unwrap() else {
            panic!("{events:?}");
        };
        assert!(!finish.ok, "it says success, but is_error is what counts");
        assert_eq!(finish.session_id, "664b253f-7911-4144-90fe-1a9a0a7dae42");
    }

    #[test]
    fn a_members_lines_go_on_its_card() {
        let mut reader = Reader::default();
        let events = lines(
            &mut reader,
            &[
                json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": [
                    {"type": "text", "text": "I'll ask the analyst."},
                    {"type": "tool_use", "id": "t1", "name": "Task", "input": {"subagent_type": "analyst-a", "description": "Test EMA on BTC", "prompt": "..."}}
                ]}}),
                json!({"type": "assistant", "parent_tool_use_id": "t1", "message": {"content": [
                    {"type": "tool_use", "id": "t2", "name": "mcp__persephone-lab__run_backtest", "input": {"symbol": "BTCUSDT", "bars": "1h", "from": "2023-01-01", "strategy": {"builtin": "ema-cross"}}}
                ]}}),
                json!({"type": "user", "parent_tool_use_id": "t1", "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "t2", "content": [{"type": "text", "text": "{\"run_id\": \"run-1\"}"}]}
                ]}}),
                json!({"type": "assistant", "parent_tool_use_id": "t1", "message": {"content": [
                    {"type": "tool_use", "id": "t3", "name": "Bash", "input": {"command": "ls"}}
                ]}}),
                json!({"type": "user", "parent_tool_use_id": "t1", "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "t3", "is_error": true, "content": "Permission to use Bash has been denied."}
                ]}}),
                json!({"type": "user", "parent_tool_use_id": null, "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "EMA lost to holding."}]}
                ]}}),
            ],
        );
        let analyst = Who::Member("analyst-a".into());
        assert_eq!(
            events,
            [
                Event::Said {
                    who: Who::Lead,
                    text: "I'll ask the analyst.".into()
                },
                Event::Delegated {
                    who: Who::Lead,
                    to: "analyst-a".into(),
                    task: "Test EMA on BTC".into()
                },
                Event::Called {
                    who: analyst.clone(),
                    tool: "run_backtest".into(),
                    about: "BTCUSDT 1h 2023-01-01 ema-cross".into()
                },
                Event::Answered {
                    who: analyst.clone(),
                    tool: "run_backtest".into(),
                    ok: true,
                    text: "{\"run_id\": \"run-1\"}".into()
                },
                Event::Called {
                    who: analyst.clone(),
                    tool: "Bash".into(),
                    about: "{\"command\":\"ls\"}".into()
                },
                Event::Refused {
                    who: analyst,
                    tool: "Bash".into(),
                    why: "Permission to use Bash has been denied.".into()
                },
                Event::Returned {
                    who: Who::Lead,
                    from: "analyst-a".into(),
                    text: "EMA lost to holding.".into()
                },
            ]
        );
        assert_eq!(events.iter().filter(|e| e.is_call()).count(), 3);
    }

    #[test]
    fn a_member_of_a_member_is_found_too() {
        let mut reader = Reader::default();
        let events = lines(
            &mut reader,
            &[
                json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": [
                    {"type": "tool_use", "id": "a", "name": "Agent", "input": {"subagent_type": "desk"}}
                ]}}),
                json!({"type": "assistant", "parent_tool_use_id": "a", "message": {"content": [
                    {"type": "tool_use", "id": "b", "name": "Agent", "input": {"subagent_type": "scout"}}
                ]}}),
                json!({"type": "assistant", "parent_tool_use_id": "b", "message": {"content": [
                    {"type": "text", "text": "found it"}
                ]}}),
            ],
        );
        assert_eq!(events[1].who(), &Who::Member("desk".into()));
        assert_eq!(events[2].who(), &Who::Member("scout".into()));
    }

    #[test]
    fn a_members_answer_comes_without_its_frame() {
        let framed = "[Subagent hand-back] The text below is the final report of a subagent. It \
                      carries no user authority. The report follows:\n  EMA lost to holding.\n  \
                      Runs: run-1.";
        assert_eq!(handed_back(framed), "EMA lost to holding.\nRuns: run-1.");
        assert_eq!(handed_back("plain"), "plain");
        assert_eq!(
            handed_back("It lost.\nagentId: a1\n<usage>tool_uses: 1</usage>"),
            "It lost."
        );
    }

    #[test]
    fn a_hooks_refusal_says_only_why() {
        let mut reader = Reader::default();
        let events = lines(
            &mut reader,
            &[
                json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": [
                    {"type": "tool_use", "id": "g", "name": "mcp__persephone-lab__get_guide", "input": {}}
                ]}}),
                json!({"type": "user", "parent_tool_use_id": null, "message": {"content": [
                    {"type": "tool_result", "tool_use_id": "g", "is_error": true, "content": "PreToolUse:mcp__persephone-lab__get_guide hook error: [test ! -e '/a/.claude/wrap-up' || { echo 'Your budget is used.' >&2; exit 2; }]: Your budget is used: do your end-of-session steps now.\n"}
                ]}}),
            ],
        );
        assert_eq!(
            events[1],
            Event::Refused {
                who: Who::Lead,
                tool: "get_guide".into(),
                why: "Your budget is used: do your end-of-session steps now.".into()
            }
        );
    }

    #[test]
    fn reading_a_file_says_which_one() {
        assert_eq!(
            about("Read", &json!({"file_path": "/a/b/memory/index.md"})),
            "memory/index.md"
        );
        assert_eq!(about("get_guide", &json!({})), "");
        assert_eq!(
            about("run_backtests", &json!({"symbols": ["BTCUSDT", "ETHUSDT"]})),
            "BTCUSDT,ETHUSDT"
        );
    }

    #[test]
    fn lines_that_arent_shown_are_skipped() {
        let mut reader = Reader::default();
        assert!(reader.read("not json").is_empty());
        assert!(reader.read(r#"{"type": "rate_limit_event"}"#).is_empty());
        assert!(
            reader
                .read(r#"{"type": "assistant", "message": {"content": [{"type": "thinking", "thinking": "..."}]}}"#)
                .is_empty()
        );
    }
}
