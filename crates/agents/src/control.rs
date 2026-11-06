//! The agents' controls, for another program or another AI: every team, its agents and links,
//! its runs and what they wrote, as tools called by name with JSON arguments. `persephone-agents`
//! serves them over MCP, local HTTP and its command line.
//!
//! The agents in a team never reach these: they have no shell and no web, and Lab is their only
//! tools server. Teams are the same files the app draws, so a change made here shows there.

use std::{
    collections::HashMap,
    fs,
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Map, Value, json};

use crate::{
    check::{check, check_setup},
    folder,
    plan::{Plan, StepKind, plan, sessions},
    run::{self, RunState},
    stream::{Event, Reader, Who},
    team::{Effort, LAB, LabData, Link, LinkKind, Model, Starter, Team, fresh_id, key},
};

/// One argument of a tool.
#[derive(Clone, Copy, Debug)]
pub struct Arg {
    pub name: &'static str,
    /// Its JSON type.
    pub kind: &'static str,
    pub what: &'static str,
    pub required: bool,
}

const fn arg(name: &'static str, kind: &'static str, what: &'static str) -> Arg {
    Arg {
        name,
        kind,
        what,
        required: true,
    }
}

const fn opt(name: &'static str, kind: &'static str, what: &'static str) -> Arg {
    Arg {
        name,
        kind,
        what,
        required: false,
    }
}

/// One tool.
#[derive(Clone, Debug)]
pub struct Tool {
    pub name: &'static str,
    pub what: &'static str,
    pub args: Vec<Arg>,
}

impl Tool {
    /// Its arguments as JSON Schema.
    #[must_use]
    pub fn input_schema(&self) -> Value {
        let mut properties = Map::new();
        for arg in &self.args {
            let kind = if arg.kind == "string_or_null" {
                json!(["string", "null"])
            } else {
                json!(arg.kind)
            };
            properties.insert(
                arg.name.to_owned(),
                json!({"type": kind, "description": arg.what}),
            );
        }
        let required: Vec<&str> = self
            .args
            .iter()
            .filter(|a| a.required)
            .map(|a| a.name)
            .collect();
        json!({"type": "object", "properties": properties, "required": required})
    }
}

const TEAM: Arg = arg("team", "string", "The team, by its id or name.");
const AGENT_FIELDS: [Arg; 10] = [
    opt(
        "role",
        "string",
        "What it does, in its own words: the heart of its prompt.",
    ),
    opt(
        "model",
        "string",
        "fable, opus, sonnet or haiku (Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5).",
    ),
    opt("effort", "string", "low, medium, high, xhigh or max."),
    opt(
        "budget",
        "integer",
        "Tool calls it's asked to stay within each session.",
    ),
    opt(
        "spend_cap",
        "number",
        "The most one of its sessions may spend, in dollars at API prices. Only for an agent that reports to no one.",
    ),
    opt(
        "memory",
        "boolean",
        "Whether it keeps a memory from one run to the next.",
    ),
    opt(
        "app_notes",
        "boolean",
        "Whether it writes bugs and ideas for the app.",
    ),
    opt("uses_lab", "boolean", "Whether it has Lab's tools."),
    opt(
        "reports_to",
        "string_or_null",
        "Its lead, by name or id: they run as one session, the lead handing it work. Null or empty for none.",
    ),
    opt(
        "at",
        "array",
        "Where its card's centre is on the canvas, [x, y].",
    ),
];

/// Every tool, in the order they're listed.
#[must_use]
pub fn tools() -> Vec<Tool> {
    let with = |head: &[Arg], tail: &[Arg]| head.iter().chain(tail).copied().collect::<Vec<_>>();
    vec![
        Tool {
            name: "get_guide",
            what: "How teams work and how to use these tools. Read it first.",
            args: vec![],
        },
        Tool {
            name: "list_teams",
            what: "Every team: id, name, its agents, and a run that's going, if any.",
            args: vec![],
        },
        Tool {
            name: "get_team",
            what: "A team's whole design: brief, principles, agents, links and Lab, how it would run, the most a run can spend, and what Check finds wrong with it.",
            args: vec![TEAM],
        },
        Tool {
            name: "create_team",
            what: "Makes a team, from a starter or built whole in one call: give `agents` and `links` and it's laid out on the canvas, ready to check and run. It shows in the app's Agents view at once.",
            args: vec![
                arg("name", "string", "The team's name."),
                opt(
                    "starter",
                    "string",
                    "blank (the default), one_analyst, lead_and_analysts or researcher_and_reviewer.",
                ),
                opt("brief", "string", "What the team should study."),
                opt(
                    "principles",
                    "string",
                    "How every agent works: method, not market views. Leave out for the agent kit's.",
                ),
                opt(
                    "at_once",
                    "integer",
                    "How many sessions may run at once, 1 to 4.",
                ),
                opt(
                    "agents",
                    "array",
                    "Agents to add, each an object: name (required), role, model, effort, budget, spend_cap, memory, app_notes, uses_lab (true unless said), reports_to (another's name).",
                ),
                opt(
                    "links",
                    "array",
                    "Links between them, each an object: from, to (names), kind (hands_to or reviews; reports_to too), and rounds for reviews.",
                ),
            ],
        },
        Tool {
            name: "update_team",
            what: "Changes a team's name, brief, principles or how many sessions run at once.",
            args: vec![
                TEAM,
                opt("name", "string", "A new name."),
                opt("brief", "string", "What the team should study."),
                opt("principles", "string", "How every agent works."),
                opt(
                    "at_once",
                    "integer",
                    "How many sessions may run at once, 1 to 4.",
                ),
            ],
        },
        Tool {
            name: "duplicate_team",
            what: "Copies a team's design, with new agents that start with no memory.",
            args: vec![TEAM, opt("name", "string", "The copy's name.")],
        },
        Tool {
            name: "archive_team",
            what: "Moves a team's folder to the archive. Nothing is deleted.",
            args: vec![TEAM],
        },
        Tool {
            name: "add_agent",
            what: "Adds an agent to a team. Its card goes under its lead, or beside the others.",
            args: with(&[TEAM, arg("name", "string", "Its name.")], &AGENT_FIELDS),
        },
        Tool {
            name: "update_agent",
            what: "Changes an agent: any of its fields, its name, or its lead.",
            args: with(
                &[
                    TEAM,
                    arg("agent", "string", "The agent, by its name or id."),
                    opt("name", "string", "A new name. Its folder and memory stay."),
                ],
                &AGENT_FIELDS,
            ),
        },
        Tool {
            name: "remove_agent",
            what: "Takes an agent off a team, with its links. Its folder, if it had one, goes to the archive.",
            args: vec![
                TEAM,
                arg("agent", "string", "The agent, by its name or id."),
            ],
        },
        Tool {
            name: "link",
            what: "Links two agents. reports_to: `from` joins `to`'s team, replacing any lead it had. hands_to: `to` works from what `from` found. reviews: `from` re-runs `to`'s claims and `to` answers, for `rounds`.",
            args: vec![
                TEAM,
                arg("from", "string", "An agent, by name or id."),
                arg("to", "string", "An agent, by name or id."),
                arg("kind", "string", "reports_to, hands_to or reviews."),
                opt("rounds", "integer", "For reviews: 1 to 5. Default 1."),
            ],
        },
        Tool {
            name: "unlink",
            what: "Takes off the links from one agent to another: all of them, or those of a kind.",
            args: vec![
                TEAM,
                arg("from", "string", "An agent, by name or id."),
                arg("to", "string", "An agent, by name or id, or Lab."),
                opt("kind", "string", "reports_to, hands_to, reviews or uses."),
            ],
        },
        Tool {
            name: "set_lab",
            what: "Says which Lab the team's agents use: its program, and whose data it works on.",
            args: vec![
                TEAM,
                opt(
                    "program",
                    "string",
                    "Lab's program; empty for the one beside the app.",
                ),
                opt(
                    "data",
                    "string",
                    "own (a folder of the agents' own) or app (your app's, so what they save shows there).",
                ),
                opt(
                    "folder",
                    "string",
                    "With own: the folder. Default ~/persephone-agent-data.",
                ),
            ],
        },
        Tool {
            name: "tidy_layout",
            what: "Lays a team's cards out as a tree: leads above their teams, sessions side by side, Lab below.",
            args: vec![TEAM],
        },
        Tool {
            name: "check_team",
            what: "What's wrong with a team before it runs, and whether this computer has what it needs (Claude Code logged in, Lab built).",
            args: vec![TEAM],
        },
        Tool {
            name: "run_team",
            what: "Starts a run of a team in the background, if Check finds nothing wrong, and answers with its name at once. Watch it with get_run.",
            args: vec![TEAM],
        },
        Tool {
            name: "get_run",
            what: "A run as it goes, or as it ended: each step's status, calls, spend and what it's doing, and its latest activity.",
            args: vec![
                TEAM,
                opt("run", "string", "The run; the latest if left out."),
                opt(
                    "lines",
                    "integer",
                    "How many of the latest activity lines. Default 30.",
                ),
            ],
        },
        Tool {
            name: "list_runs",
            what: "A team's runs, newest first, with how each ended.",
            args: vec![TEAM],
        },
        Tool {
            name: "get_activity",
            what: "A run's activity, a page at a time: what each agent said, its tool calls and their answers, work handed out and brought back, and refusals.",
            args: vec![
                TEAM,
                arg("run", "string", "The run."),
                opt("agent", "string", "Only this agent's, by name or id."),
                opt("from", "integer", "The first line to give. Default 0."),
                opt("limit", "integer", "The most lines to give. Default 100."),
            ],
        },
        Tool {
            name: "stop_run",
            what: "Stops a run now, with everything it started.",
            args: vec![
                TEAM,
                opt("run", "string", "The run; the one going if left out."),
            ],
        },
        Tool {
            name: "wrap_up_run",
            what: "Has a run's sessions do their end-of-session steps, and start no more.",
            args: vec![
                TEAM,
                opt("run", "string", "The run; the one going if left out."),
            ],
        },
        Tool {
            name: "get_outputs",
            what: "What an agent has written: its report, newest journal, beliefs, bugs and ideas, tasks and vetoed list. A member's are its notes.",
            args: vec![
                TEAM,
                arg("agent", "string", "The agent, by its name or id."),
            ],
        },
        Tool {
            name: "get_feedback",
            what: "Every agent's bugs and ideas for Persephone, in one place.",
            args: vec![TEAM],
        },
        Tool {
            name: "export_team",
            what: "A team's whole setup as a file: its brief, principles, every agent's settings, the links, Lab and the layout, by name. Import it with import_team to get the same setup again, here or on another computer. Agents' memory isn't in it.",
            args: vec![TEAM, opt("path", "string", "Also write it to this file.")],
        },
        Tool {
            name: "import_team",
            what: "Makes a team from a setup file, as export_team writes it or as written by hand or by an AI (docs/agents-setup.md says how). Every key is checked, so a typo is caught rather than ignored. Its agents start with no memory.",
            args: vec![
                opt("setup", "object", "The setup itself."),
                opt("path", "string", "Or a file holding it."),
                opt(
                    "name",
                    "string",
                    "A name for the team instead of the file's, such as when that one's taken.",
                ),
            ],
        },
        Tool {
            name: "set_notes",
            what: "Writes an agent's tasks.md (jobs for it to work through) or vetoed.md (what you reverted from its memory, and why). Only for an agent that reports to no one.",
            args: vec![
                TEAM,
                arg("agent", "string", "The agent, by its name or id."),
                arg("file", "string", "tasks or vetoed."),
                arg("text", "string", "The whole file."),
            ],
        },
    ]
}

/// The tools as Markdown, for the docs.
#[must_use]
pub fn reference() -> String {
    let mut out = String::from(
        "# Persephone agents' controls\n\n\
         Generated from the tools themselves: `persephone-agents tools --markdown`.\n",
    );
    for tool in tools() {
        out += &format!("\n## `{}`\n\n{}\n\n", tool.name, tool.what);
        for arg in &tool.args {
            let needed = if arg.required { ", required" } else { "" };
            out += &format!("- `{}` ({}{needed}): {}\n", arg.name, arg.kind, arg.what);
        }
        if tool.args.is_empty() {
            out += "No arguments.\n";
        }
    }
    out
}

const GUIDE: &str = "# Persephone agents: guide to the controls

A team is a design: agents, the links between them, and the Lab they use, kept in its folder as \
team.json. The app's Agents view draws the same files, so what you change here shows there at \
once, and what's changed there shows here.

## Agents

Each has a name and a role (what it does, in its own words), a model (fable, opus, sonnet, haiku: \
Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5) and an effort (low, medium, high, xhigh, max), a \
budget of tool calls per session, and a spend cap per session in dollars (only for an agent that \
reports to no one). It may keep a memory between runs, write bugs and ideas for the app, and use \
Lab.

## Links

- reports_to: the first is one of the second's team. They run as one session: the lead hands it \
work with Claude Code's Agent tool and checks what comes back. An agent has one lead. Teams nest.
- hands_to: the second works from what the first found. Between sessions, the second starts \
after the first, with its report in its inbox.
- reviews: the first re-runs the second's claims in Lab, and the second answers, for 1 to 5 \
rounds.
- uses: the agent has Lab's tools. Set it with uses_lab.

Inside a session, hands_to and reviews join agents with the same lead, and become what that lead \
is told. Between sessions they join agents that report to no one.

## Running

Each agent that reports to no one is a Claude Code session in its own folder, sandboxed: Lab, its \
own folder, and nothing else; no shell, no web. run_team starts a run in the background; get_run \
shows it as it goes; wrap_up_run has it finish cleanly, and stop_run ends it at once. Each \
session's spend cap holds; check_team says the most a run can spend.

## Building one

1. create_team with a name, a brief, and its agents and links, all in one call; or import_team
   with a setup file (export_team writes one from any team).
2. check_team, and fix what it says with update_team, update_agent, link and unlink.
3. run_team, then get_run until it's ended.
4. get_outputs and get_feedback for what the agents wrote.";

/// Where the controls work, and what they start runs with.
#[derive(Clone, Debug)]
pub struct Desk {
    /// Where the teams are.
    pub root: PathBuf,
    /// Claude Code's program; none to look for it.
    pub claude: Option<PathBuf>,
    /// The program that runs a team in the background (`persephone-agents run`); none where
    /// runs can't be started.
    pub runner: Option<PathBuf>,
}

impl Desk {
    /// Calls a tool by name.
    ///
    /// # Errors
    ///
    /// Says what's wrong with the call, or why it couldn't be done.
    pub fn dispatch(&self, tool: &str, args: &Value) -> Result<Value, String> {
        match tool {
            "get_guide" => Ok(json!({"guide": GUIDE})),
            "list_teams" => Ok(self.list_teams()),
            "get_team" => {
                let team = self.load(text(args, "team")?)?;
                Ok(self.describe(&team))
            }
            "create_team" => self.create_team(args),
            "update_team" => self.update_team(args),
            "duplicate_team" => self.duplicate_team(args),
            "archive_team" => self.archive_team(args),
            "add_agent" => self.add_agent(args),
            "update_agent" => self.update_agent(args),
            "remove_agent" => self.remove_agent(args),
            "link" => self.link(args),
            "unlink" => self.unlink(args),
            "set_lab" => self.set_lab(args),
            "tidy_layout" => {
                let mut team = self.load(text(args, "team")?)?;
                tidy(&mut team);
                self.save(&team)?;
                Ok(self.describe(&team))
            }
            "check_team" => self.check_team(args),
            "run_team" => self.run_team(args),
            "get_run" => self.get_run(args),
            "list_runs" => self.list_runs(args),
            "get_activity" => self.get_activity(args),
            "stop_run" => self.ask_run(args, true),
            "wrap_up_run" => self.ask_run(args, false),
            "get_outputs" => self.get_outputs(args),
            "get_feedback" => self.get_feedback(args),
            "set_notes" => self.set_notes(args),
            "export_team" => {
                let team = self.load(text(args, "team")?)?;
                let setup = export(&team);
                let written = match args.get("path").and_then(Value::as_str) {
                    Some(path) => {
                        let path = expand(path);
                        let text = serde_json::to_string_pretty(&setup).unwrap_or_default() + "\n";
                        folder::write_aside(&path, text.as_bytes())?;
                        Some(path)
                    }
                    None => None,
                };
                Ok(json!({"setup": setup, "written": written}))
            }
            "import_team" => self.import_team(args),
            _ => Err(format!(
                "there's no tool called `{tool}`; the tools are {}",
                tools()
                    .iter()
                    .map(|t| t.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    fn teams(&self) -> Vec<Team> {
        folder::load_teams(&self.root)
    }

    /// A team by its id or name.
    fn load(&self, wanted: &str) -> Result<Team, String> {
        let teams = self.teams();
        let wanted_low = wanted.trim().to_lowercase();
        teams
            .iter()
            .find(|t| t.id == wanted)
            .or_else(|| teams.iter().find(|t| t.name.to_lowercase() == wanted_low))
            .or_else(|| teams.iter().find(|t| key(&t.name) == key(wanted)))
            .cloned()
            .ok_or_else(|| {
                let names: Vec<&str> = teams.iter().map(|t| t.name.as_str()).collect();
                if names.is_empty() {
                    format!("there's no team called `{wanted}`, and no teams yet: make one with create_team")
                } else {
                    format!(
                        "there's no team called `{wanted}`; the teams are {}",
                        names.join(", ")
                    )
                }
            })
    }

    fn save(&self, team: &Team) -> Result<(), String> {
        folder::save_team(&self.root, team)
    }

    fn going(&self, team: &Team) -> Option<RunState> {
        run::going_run(&self.root, team)
    }

    fn list_teams(&self) -> Value {
        let teams: Vec<Value> = self
            .teams()
            .iter()
            .map(|team| {
                json!({
                    "id": team.id,
                    "name": team.name,
                    "agents": team.agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
                    "running": self.going(team).map(|state| state.run),
                })
            })
            .collect();
        json!({"teams": teams, "folder": self.root})
    }

    /// A team's design, as these tools speak of it, with what Check finds.
    fn describe(&self, team: &Team) -> Value {
        let agents: Vec<Value> = team
            .agents
            .iter()
            .map(|agent| {
                json!({
                    "id": agent.id,
                    "name": agent.name,
                    "role": agent.role,
                    "model": agent.model.cli(),
                    "effort": agent.effort.cli(),
                    "budget": agent.budget,
                    "spend_cap": agent.spend_cap,
                    "memory": agent.memory,
                    "app_notes": agent.app_notes,
                    "uses_lab": team.uses_lab(&agent.id),
                    "reports_to": team.lead_of(&agent.id).map(|id| team.name_of(id)),
                    "its_team": team.reports_of(&agent.id).iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
                    "at": agent.at,
                })
            })
            .collect();
        let links: Vec<Value> = team
            .links
            .iter()
            .filter(|l| l.kind != LinkKind::Uses)
            .map(|link| {
                let mut out = json!({
                    "from": team.name_of(&link.from),
                    "to": team.name_of(&link.to),
                    "kind": kind_name(link.kind),
                });
                if let LinkKind::Reviews { rounds } = link.kind {
                    out["rounds"] = json!(rounds);
                }
                out
            })
            .collect();
        let (data, data_folder) = match &team.lab.data {
            LabData::Own(dir) => ("own", Some(dir.display().to_string())),
            LabData::App => ("app", None),
        };
        let how = plan(team).ok().map(|plan| {
            (0..plan.steps.len())
                .map(|step| plan.label(team, step))
                .collect::<Vec<_>>()
        });
        json!({
            "id": team.id,
            "name": team.name,
            "brief": team.brief,
            "principles": team.principles,
            "at_once": team.at_once,
            "agents": agents,
            "links": links,
            "lab": {
                "program": team.lab.program,
                "data": data,
                "folder": data_folder,
            },
            "steps": how,
            "most_spend": most_spend(team),
            "problems": check(team).iter().map(|p| p.what.as_str()).collect::<Vec<_>>(),
            "folder": folder::team_dir(&self.root, team),
        })
    }

    fn create_team(&self, args: &Value) -> Result<Value, String> {
        let name = text(args, "name")?.trim();
        if name.is_empty() {
            return Err("a team needs a name".into());
        }
        if self
            .teams()
            .iter()
            .any(|t| t.name.eq_ignore_ascii_case(name))
        {
            return Err(format!(
                "there's a team called `{name}` already: give this one another name, or change that one with update_team"
            ));
        }
        let starter = match args.get("starter").and_then(Value::as_str) {
            None | Some("" | "blank") => Starter::Blank,
            Some("one_analyst") => Starter::OneAnalyst,
            Some("lead_and_analysts") => Starter::LeadAndAnalysts,
            Some("researcher_and_reviewer") => Starter::ResearcherAndReviewer,
            Some(other) => {
                return Err(format!(
                    "`{other}` isn't a starter: blank, one_analyst, lead_and_analysts or researcher_and_reviewer"
                ));
            }
        };
        let mut team = Team::new(name, starter);
        team_fields(&mut team, args)?;
        if let Some(agents) = args.get("agents") {
            let agents = agents
                .as_array()
                .ok_or("`agents` is a list of objects, each with a name and its fields")?;
            // Every card first, so leads and links can name any of them.
            let mut leads = Vec::new();
            for spec in agents {
                let name = spec
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.trim().is_empty())
                    .ok_or("every agent in `agents` needs a name")?;
                let id = team.add_agent([0.0, 0.0]);
                if let Some(agent) = team.agent_mut(&id) {
                    agent.name = name.trim().to_owned();
                }
                let mut fields = spec.clone();
                if let Some(lead) = fields.as_object_mut().and_then(|f| f.remove("reports_to")) {
                    leads.push((id.clone(), lead));
                }
                if fields.get("uses_lab").is_none() {
                    fields["uses_lab"] = json!(true);
                }
                agent_fields(&mut team, &id, &fields)?;
            }
            for (id, lead) in leads {
                agent_fields(&mut team, &id, &json!({"reports_to": lead}))?;
            }
        }
        if let Some(links) = args.get("links") {
            let links = links
                .as_array()
                .ok_or("`links` is a list of objects, each with from, to and kind")?;
            for spec in links {
                add_link(&mut team, spec)?;
            }
        }
        if args.get("agents").is_some() {
            tidy(&mut team);
        }
        self.save(&team)?;
        Ok(self.describe(&team))
    }

    fn import_team(&self, args: &Value) -> Result<Value, String> {
        let setup = match (args.get("setup"), args.get("path").and_then(Value::as_str)) {
            (Some(setup), _) => setup.clone(),
            (None, Some(path)) => read_setup(&expand(path))?,
            (None, None) => return Err("give `setup`, or the `path` of a file holding one".into()),
        };
        let mut team = import(&setup)?;
        if let Some(name) = args.get("name").and_then(Value::as_str) {
            let name = name.trim();
            if name.is_empty() {
                return Err("a team needs a name".into());
            }
            team.name = name.to_owned();
        }
        if self
            .teams()
            .iter()
            .any(|t| t.name.eq_ignore_ascii_case(&team.name))
        {
            return Err(format!(
                "there's a team called `{}` already: pass `name` to call this one something else",
                team.name
            ));
        }
        self.save(&team)?;
        Ok(self.describe(&team))
    }

    fn update_team(&self, args: &Value) -> Result<Value, String> {
        let mut team = self.load(text(args, "team")?)?;
        if let Some(name) = args.get("name").and_then(Value::as_str) {
            let name = name.trim();
            if name.is_empty() {
                return Err("a team needs a name".into());
            }
            team.name = name.to_owned();
        }
        team_fields(&mut team, args)?;
        self.save(&team)?;
        Ok(self.describe(&team))
    }

    fn duplicate_team(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        let mut copy = team.clone();
        copy.name = args
            .get("name")
            .and_then(Value::as_str)
            .map_or_else(|| format!("{} copy", team.name), str::to_owned);
        copy.id = fresh_id(&copy.name);
        // New agents, with folders of their own: none of the original's memory.
        for agent in &team.agents {
            let new = fresh_id(&agent.name);
            for a in &mut copy.agents {
                if a.id == agent.id {
                    a.id.clone_from(&new);
                }
            }
            for link in &mut copy.links {
                if link.from == agent.id {
                    link.from.clone_from(&new);
                }
                if link.to == agent.id {
                    link.to.clone_from(&new);
                }
            }
        }
        self.save(&copy)?;
        Ok(self.describe(&copy))
    }

    fn archive_team(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        if let Some(state) = self.going(&team) {
            return Err(format!("run {} is going: stop it first", state.run));
        }
        let to = folder::archive(&self.root, &folder::team_dir(&self.root, &team), &team.id)?;
        Ok(json!({"archived": team.name, "to": to}))
    }

    fn add_agent(&self, args: &Value) -> Result<Value, String> {
        let mut team = self.load(text(args, "team")?)?;
        let name = text(args, "name")?.trim();
        if name.is_empty() {
            return Err("an agent needs a name".into());
        }
        let id = team.add_agent([0.0, 0.0]);
        if let Some(agent) = team.agent_mut(&id) {
            agent.name = name.to_owned();
        }
        let mut fields = args.clone();
        if fields.get("uses_lab").is_none() {
            fields["uses_lab"] = json!(true);
        }
        agent_fields(&mut team, &id, &fields)?;
        if args.get("at").is_none() {
            place(&mut team, &id);
        }
        self.save(&team)?;
        let mut answer = self.describe(&team);
        answer["added"] = json!(id);
        Ok(answer)
    }

    fn update_agent(&self, args: &Value) -> Result<Value, String> {
        let mut team = self.load(text(args, "team")?)?;
        let id = agent_id(&team, text(args, "agent")?)?;
        if let Some(name) = args.get("name").and_then(Value::as_str) {
            let name = name.trim();
            if name.is_empty() {
                return Err("an agent needs a name".into());
            }
            if let Some(agent) = team.agent_mut(&id) {
                agent.name = name.to_owned();
            }
        }
        agent_fields(&mut team, &id, args)?;
        self.save(&team)?;
        Ok(self.describe(&team))
    }

    fn remove_agent(&self, args: &Value) -> Result<Value, String> {
        let mut team = self.load(text(args, "team")?)?;
        if let Some(state) = self.going(&team) {
            return Err(format!("run {} is going: stop it first", state.run));
        }
        let id = agent_id(&team, text(args, "agent")?)?;
        let was_top = team.is_root(&id);
        team.remove_agent(&id);
        let dir = folder::agent_dir(&self.root, &team, &id);
        let mut answer = Value::Null;
        if was_top && dir.exists() {
            let to = folder::archive(&self.root, &dir, &format!("{}-{id}", team.id))?;
            answer = json!(to);
        }
        self.save(&team)?;
        let mut out = self.describe(&team);
        out["archived_folder"] = answer;
        Ok(out)
    }

    fn link(&self, args: &Value) -> Result<Value, String> {
        let mut team = self.load(text(args, "team")?)?;
        add_link(&mut team, args)?;
        self.save(&team)?;
        Ok(self.describe(&team))
    }

    fn unlink(&self, args: &Value) -> Result<Value, String> {
        let mut team = self.load(text(args, "team")?)?;
        let from = agent_id(&team, text(args, "from")?)?;
        let to_name = text(args, "to")?;
        let to = if to_name.eq_ignore_ascii_case("lab") {
            LAB.to_owned()
        } else {
            agent_id(&team, to_name)?
        };
        let kind = args
            .get("kind")
            .and_then(Value::as_str)
            .map(|k| parse_kind(k, 1))
            .transpose()?;
        let before = team.links.len();
        team.links.retain(|l| {
            !(l.from == from && l.to == to && kind.is_none_or(|k| k.same_kind(l.kind)))
        });
        if team.links.len() == before {
            return Err(format!(
                "there's no such link from {} to {}",
                team.name_of(&from),
                team.name_of(&to)
            ));
        }
        self.save(&team)?;
        Ok(self.describe(&team))
    }

    fn set_lab(&self, args: &Value) -> Result<Value, String> {
        let mut team = self.load(text(args, "team")?)?;
        if let Some(program) = args.get("program").and_then(Value::as_str) {
            let program = program.trim();
            team.lab.program = (!program.is_empty()).then(|| PathBuf::from(program));
        }
        let folder = args.get("folder").and_then(Value::as_str).map(str::trim);
        match args.get("data").and_then(Value::as_str) {
            Some("app") => team.lab.data = LabData::App,
            Some("own") | None if folder.is_some_and(|f| !f.is_empty()) => {
                team.lab.data = LabData::Own(PathBuf::from(folder.unwrap_or_default()));
            }
            Some("own") => {
                if team.lab.data == LabData::App {
                    team.lab.data = LabData::own_default();
                }
            }
            None => {}
            Some(other) => return Err(format!("`{other}` isn't own or app")),
        }
        self.save(&team)?;
        Ok(self.describe(&team))
    }

    fn check_team(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        let found = run::look_around(self.claude.as_deref(), team.lab.program.as_deref());
        let mut problems: Vec<String> = check(&team).into_iter().map(|p| p.what).collect();
        problems.extend(check_setup(&team, &found).into_iter().map(|p| p.what));
        let steps = plan(&team).ok().map(|plan| {
            (0..plan.steps.len())
                .map(|step| plan.label(&team, step))
                .collect::<Vec<_>>()
        });
        Ok(json!({
            "ready": problems.is_empty(),
            "problems": problems,
            "steps": steps,
            "most_spend": most_spend(&team),
            "claude": found.claude,
            "logged_in": found.logged_in,
            "lab": found.lab,
        }))
    }

    fn run_team(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        if let Some(state) = self.going(&team) {
            return Err(format!(
                "run {} of this team is going: watch it with get_run, or stop it with stop_run",
                state.run
            ));
        }
        let found = run::look_around(self.claude.as_deref(), team.lab.program.as_deref());
        let mut problems: Vec<String> = check(&team).into_iter().map(|p| p.what).collect();
        problems.extend(check_setup(&team, &found).into_iter().map(|p| p.what));
        if !problems.is_empty() {
            return Err(format!("it can't run yet: {}", problems.join(" ")));
        }
        let runner = self
            .runner
            .as_ref()
            .ok_or("this can't start runs: run it from persephone-agents")?;
        let (run, dir) = run::reserve_run(&folder::team_dir(&self.root, &team))?;
        // Going from now, until its runner says otherwise, so no second run starts meanwhile.
        if let Ok(plan) = plan(&team) {
            let starting = RunState {
                run: run.clone(),
                team: team.id.clone(),
                pid: 0,
                started: unix_now(),
                ended: None,
                steps: (0..plan.steps.len())
                    .map(|step| run::StepState {
                        label: plan.label(&team, step),
                        agent: plan.agent_of(step).to_owned(),
                        status: run::Status::Waiting,
                        calls: 0,
                        spend: 0.0,
                        doing: String::new(),
                    })
                    .collect(),
                notes: Vec::new(),
            };
            let text = serde_json::to_string_pretty(&starting).unwrap_or_default();
            folder::write_aside(&dir.join("state.json"), text.as_bytes())?;
        }
        let said = fs::File::create(dir.join("runner.log")).map_err(|e| e.to_string())?;
        let mut command = Command::new(runner);
        command.arg("--root").arg(&self.root);
        if let Some(claude) = &self.claude {
            command.arg("--claude").arg(claude);
        }
        command
            .args(["run", &team.id, "--run", &run])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(said)
            // Its own group, so it lives on after whatever asked for it.
            .process_group(0);
        command
            .spawn()
            .map_err(|e| format!("couldn't start {}: {e}", runner.display()))?;
        Ok(json!({
            "run": run,
            "started": true,
            "most_spend": most_spend(&team),
            "watch": "get_run, with this run's name",
        }))
    }

    /// A run, by name or the latest, and its folder.
    fn find_run(&self, team: &Team, args: &Value) -> Result<(String, PathBuf), String> {
        let named = args
            .get("run")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty());
        let run = match named {
            Some(run) => run.to_owned(),
            None => latest_run(&self.root, team).ok_or("this team hasn't run yet")?,
        };
        let dir = run::run_dir(&self.root, team, &run);
        if !dir.exists() {
            return Err(format!("there's no run called `{run}`"));
        }
        Ok((run, dir))
    }

    fn get_run(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        let (run, dir) = self.find_run(&team, args)?;
        let lines = number(args, "lines")?.unwrap_or(30) as usize;
        let state = run::read_state(&dir);
        let error = fs::read_to_string(dir.join("runner.log"))
            .ok()
            .map(|said| said.trim().to_owned())
            .filter(|said| !said.is_empty());
        let all = activity(&team, &dir, None);
        let recent: Vec<String> = all[all.len().saturating_sub(lines)..].to_vec();
        let Some(state) = state else {
            return Ok(json!({
                "run": run,
                "status": if error.is_some() { "failed to start" } else { "starting" },
                "error": error,
            }));
        };
        // Still as run_team left it, with its runner gone and an error behind it.
        let status = if state.pid == 0 && error.is_some() {
            "failed to start"
        } else if state.going() {
            "running"
        } else if state.ended.is_some() {
            "ended"
        } else {
            "stopped: its runner is gone"
        };
        let steps: Vec<Value> = state
            .steps
            .iter()
            .map(|step| {
                json!({
                    "step": step.label,
                    "status": step.status.label(),
                    "calls": step.calls,
                    "spend": step.spend,
                    "doing": step.doing,
                })
            })
            .collect();
        Ok(json!({
            "run": run,
            "status": status,
            "spend": state.spend(),
            "seconds": state.ended.unwrap_or_else(unix_now).saturating_sub(state.started),
            "steps": steps,
            "notes": state.notes,
            "recent": recent,
            "lines": all.len(),
            "error": error,
        }))
    }

    fn list_runs(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        let runs_dir = folder::team_dir(&self.root, &team).join("runs");
        let mut names: Vec<String> = fs::read_dir(&runs_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort_by(|a, b| b.cmp(a));
        let runs: Vec<Value> = names
            .iter()
            .map(|run| {
                let dir = runs_dir.join(run);
                let state = run::read_state(&dir);
                let status = match &state {
                    Some(s) if s.going() => "running".to_owned(),
                    Some(s) if s.ended.is_some() => format!(
                        "ended: {} of {} steps done",
                        s.steps
                            .iter()
                            .filter(|st| st.status == run::Status::Done)
                            .count(),
                        s.steps.len()
                    ),
                    Some(_) => "stopped: its runner is gone".to_owned(),
                    None => "never started".to_owned(),
                };
                json!({
                    "run": run,
                    "status": status,
                    "spend": state.as_ref().map(RunState::spend),
                })
            })
            .collect();
        Ok(json!({"runs": runs}))
    }

    fn get_activity(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        let (run, dir) = self.find_run(&team, args)?;
        let only = args
            .get("agent")
            .and_then(Value::as_str)
            .map(|a| agent_id(&team, a))
            .transpose()?;
        let all = activity(&team, &dir, only.as_deref());
        let from = number(args, "from")?.unwrap_or(0) as usize;
        let limit = number(args, "limit")?.unwrap_or(100).clamp(1, 1000) as usize;
        let page: Vec<&String> = all.iter().skip(from).take(limit).collect();
        let next = (from + page.len() < all.len()).then_some(from + page.len());
        Ok(json!({"run": run, "lines": page, "total": all.len(), "next": next}))
    }

    fn ask_run(&self, args: &Value, stop: bool) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        let run = match args
            .get("run")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
        {
            Some(run) => run.to_owned(),
            None => self.going(&team).ok_or("no run of this team is going")?.run,
        };
        let dir = run::run_dir(&self.root, &team, &run);
        if !dir.exists() {
            return Err(format!("there's no run called `{run}`"));
        }
        if stop {
            run::ask_stop(&dir)?;
        } else {
            run::ask_wrap_up(&dir)?;
        }
        Ok(json!({"run": run, "asked": if stop { "stop" } else { "wrap up" }}))
    }

    fn get_outputs(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        let id = agent_id(&team, text(args, "agent")?)?;
        let top = team.root_of(&id).to_owned();
        let dir = folder::agent_dir(&self.root, &team, &top);
        let read = |path: &str| fs::read_to_string(dir.join(path)).unwrap_or_default();
        if top != id {
            let name = team.name_of(&id);
            return Ok(json!({
                "agent": name,
                "member_of": team.name_of(&top),
                "notes": read(&format!("memory/team/{}.md", key(name))),
                "folder": dir,
            }));
        }
        let journal = newest(&dir.join("memory/journal"));
        Ok(json!({
            "agent": team.name_of(&id),
            "folder": dir,
            "report": read("outbox/report.md"),
            "journal": journal.as_ref().map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            "journal_text": journal.and_then(|p| fs::read_to_string(p).ok()),
            "beliefs": read("memory/beliefs.md"),
            "bugs": read("app/bugs.md"),
            "ideas": read("app/ideas.md"),
            "tasks": read("tasks.md"),
            "vetoed": read("vetoed.md"),
        }))
    }

    fn get_feedback(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        let agents: Vec<Value> = sessions(&team)
            .iter()
            .map(|session| {
                let dir = folder::agent_dir(&self.root, &team, &session.root);
                let read = |p: &str| fs::read_to_string(dir.join(p)).unwrap_or_default();
                json!({
                    "agent": team.name_of(&session.root),
                    "bugs": read("app/bugs.md"),
                    "ideas": read("app/ideas.md"),
                })
            })
            .collect();
        Ok(json!({"agents": agents}))
    }

    fn set_notes(&self, args: &Value) -> Result<Value, String> {
        let team = self.load(text(args, "team")?)?;
        let id = agent_id(&team, text(args, "agent")?)?;
        if !team.is_root(&id) {
            return Err(format!(
                "{} works in {}'s session and has no files of its own",
                team.name_of(&id),
                team.name_of(team.root_of(&id))
            ));
        }
        let file = match text(args, "file")? {
            "tasks" => "tasks.md",
            "vetoed" => "vetoed.md",
            other => return Err(format!("`{other}` isn't tasks or vetoed")),
        };
        let dir = folder::agent_dir(&self.root, &team, &id);
        if !dir.exists() {
            return Err(format!(
                "{} has no folder yet: it's made the first time the team runs",
                team.name_of(&id)
            ));
        }
        folder::write_aside(&dir.join(file), text(args, "text")?.as_bytes())?;
        Ok(json!({"written": dir.join(file)}))
    }
}

/// The most a run can spend: every step's session cap.
fn most_spend(team: &Team) -> Option<f64> {
    let plan = plan(team).ok()?;
    Some(
        plan.steps
            .iter()
            .filter_map(|step| team.agent(&plan.sessions[step.session].root))
            .fold(0.0, |most, agent| most + agent.spend_cap),
    )
}

/// The newest run's name.
fn latest_run(root: &Path, team: &Team) -> Option<String> {
    let mut names: Vec<String> = fs::read_dir(folder::team_dir(root, team).join("runs"))
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.pop()
}

fn newest(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    files.pop()
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A run's activity, as lines of text: every step's, in order, or only one agent's.
fn activity(team: &Team, dir: &Path, only: Option<&str>) -> Vec<String> {
    let Some(plan) = run::read_plan(dir) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for step in 0..plan.steps.len() {
        let name = format!(
            "{step:02}-{}.jsonl",
            plan.sessions[plan.steps[step].session].key
        );
        let Ok(text) = fs::read_to_string(dir.join(name)) else {
            continue;
        };
        let mut reader = Reader::default();
        for event in text.lines().flat_map(|line| reader.read(line)) {
            let who = who_of(team, &plan, step, event.who());
            let about_them = match &event {
                Event::Delegated { to, .. } => Some(member_id(team, &plan, step, to)),
                Event::Returned { from, .. } => Some(member_id(team, &plan, step, from)),
                _ => None,
            };
            if let Some(only) = only
                && who.0 != only
                && about_them.flatten().as_deref() != Some(only)
            {
                continue;
            }
            if let Some(line) = line(&plan, step, &who.1, &event) {
                lines.push(line);
            }
        }
    }
    lines
}

/// Who did something: their id and name.
fn who_of(team: &Team, plan: &Plan, step: usize, who: &Who) -> (String, String) {
    let root = plan.agent_of(step);
    match who {
        Who::Lead => (root.to_owned(), team.name_of(root).to_owned()),
        Who::Member(member) => match member_id(team, plan, step, member) {
            Some(id) => (id.clone(), team.name_of(&id).to_owned()),
            None => (String::new(), member.clone()),
        },
    }
}

fn member_id(_team: &Team, plan: &Plan, step: usize, member: &str) -> Option<String> {
    plan.sessions[plan.steps[step].session]
        .members
        .iter()
        .find(|m| m.key == member)
        .map(|m| m.id.clone())
}

/// One activity line.
fn line(plan: &Plan, step: usize, who: &str, event: &Event) -> Option<String> {
    let step_name = match plan.steps[step].kind {
        StepKind::Work => String::new(),
        StepKind::Review { round, .. } => format!(" (review, round {round})"),
        StepKind::Answer { round } => format!(" (answer, round {round})"),
    };
    Some(match event {
        Event::Started { model, .. } => format!("{who}{step_name} started on {model}"),
        Event::Said { text, .. } => format!("{who}: {text}"),
        Event::Called { tool, about, .. } => format!("{who} → {tool} {about}").trim().to_owned(),
        Event::Answered { tool, ok, text, .. } => {
            let first: String = text
                .lines()
                .next()
                .unwrap_or_default()
                .chars()
                .take(200)
                .collect();
            format!("   ← {tool}{}: {first}", if *ok { "" } else { " failed" })
        }
        Event::Delegated { to, task, .. } => format!("{who} hands {to}: {task}"),
        Event::Returned { from, text, .. } => format!("{from} brings back: {text}"),
        Event::Refused { tool, why, .. } => format!("{who} was refused {tool}: {why}"),
        Event::Finished(finish) => format!(
            "{who}{step_name} finished: {}, {} turns, ${:.2}",
            if finish.ok { "done" } else { "failed" },
            finish.turns,
            finish.spend
        ),
    })
}

fn text<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{name}` is needed, as text"))
}

fn number(args: &Value, name: &str) -> Result<Option<u64>, String> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("`{name}` is a whole number")),
    }
}

/// An agent by its id, name or key.
fn agent_id(team: &Team, wanted: &str) -> Result<String, String> {
    let low = wanted.trim().to_lowercase();
    team.agents
        .iter()
        .find(|a| a.id == wanted)
        .or_else(|| team.agents.iter().find(|a| a.name.to_lowercase() == low))
        .or_else(|| team.agents.iter().find(|a| key(&a.name) == key(wanted)))
        .map(|a| a.id.clone())
        .ok_or_else(|| {
            let names: Vec<&str> = team.agents.iter().map(|a| a.name.as_str()).collect();
            format!(
                "there's no agent called `{wanted}` in {}; its agents are {}",
                team.name,
                if names.is_empty() {
                    "none yet".to_owned()
                } else {
                    names.join(", ")
                }
            )
        })
}

fn parse_model(said: &str) -> Result<Model, String> {
    let low = said.to_lowercase();
    Model::ALL
        .into_iter()
        .find(|m| low.contains(m.cli()))
        .ok_or_else(|| format!("`{said}` isn't a model: fable, opus, sonnet or haiku"))
}

fn parse_effort(said: &str) -> Result<Effort, String> {
    let low = said.to_lowercase().replace([' ', '_', '-'], "");
    let low = if low == "extrahigh" {
        "xhigh".to_owned()
    } else {
        low
    };
    Effort::ALL
        .into_iter()
        .find(|e| e.cli() == low)
        .ok_or_else(|| format!("`{said}` isn't an effort: low, medium, high, xhigh or max"))
}

fn parse_kind(said: &str, rounds: u32) -> Result<LinkKind, String> {
    match said.to_lowercase().replace([' ', '-'], "_").as_str() {
        "reports_to" => Ok(LinkKind::ReportsTo),
        "hands_to" => Ok(LinkKind::HandsTo),
        "reviews" => Ok(LinkKind::Reviews { rounds }),
        "uses" => Ok(LinkKind::Uses),
        _ => Err(format!(
            "`{said}` isn't a kind of link: reports_to, hands_to or reviews"
        )),
    }
}

fn kind_name(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::ReportsTo => "reports_to",
        LinkKind::HandsTo => "hands_to",
        LinkKind::Reviews { .. } => "reviews",
        LinkKind::Uses => "uses",
    }
}

/// The team's own fields that are in `args`.
fn team_fields(team: &mut Team, args: &Value) -> Result<(), String> {
    if let Some(brief) = args.get("brief").and_then(Value::as_str) {
        team.brief = brief.trim().to_owned();
    }
    if let Some(principles) = args.get("principles").and_then(Value::as_str) {
        team.principles = principles.trim().to_owned();
    }
    if let Some(at_once) = number(args, "at_once")? {
        team.at_once = u32::try_from(at_once.clamp(1, 4)).unwrap_or(1);
    }
    Ok(())
}

/// An agent's fields that are in `args`.
fn agent_fields(team: &mut Team, id: &str, args: &Value) -> Result<(), String> {
    let model = args
        .get("model")
        .and_then(Value::as_str)
        .map(parse_model)
        .transpose()?;
    let effort = args
        .get("effort")
        .and_then(Value::as_str)
        .map(parse_effort)
        .transpose()?;
    let budget = number(args, "budget")?;
    let spend_cap = match args.get("spend_cap") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_f64()
                .filter(|cap| *cap > 0.0 && cap.is_finite())
                .ok_or("`spend_cap` is a number of dollars above 0")?,
        ),
    };
    let at = match args.get("at") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let point: Vec<f64> = value
                .as_array()
                .map(|xy| xy.iter().filter_map(Value::as_f64).collect())
                .unwrap_or_default();
            let [x, y] = point[..] else {
                return Err("`at` is [x, y]".into());
            };
            #[allow(clippy::cast_possible_truncation)]
            Some([x as f32, y as f32])
        }
    };
    let agent = team.agent_mut(id).ok_or("that agent is gone")?;
    if let Some(role) = args.get("role").and_then(Value::as_str) {
        agent.role = role.trim().to_owned();
    }
    if let Some(model) = model {
        agent.model = model;
    }
    if let Some(effort) = effort {
        agent.effort = effort;
    }
    if let Some(budget) = budget {
        agent.budget = u32::try_from(budget.max(1)).unwrap_or(u32::MAX);
    }
    if let Some(cap) = spend_cap {
        agent.spend_cap = cap;
    }
    if let Some(memory) = args.get("memory").and_then(Value::as_bool) {
        agent.memory = memory;
    }
    if let Some(notes) = args.get("app_notes").and_then(Value::as_bool) {
        agent.app_notes = notes;
    }
    if let Some(at) = at {
        agent.at = at;
    }
    if let Some(uses) = args.get("uses_lab").and_then(Value::as_bool) {
        team.links
            .retain(|l| !(l.from == id && l.kind == LinkKind::Uses));
        if uses {
            team.links.push(Link {
                from: id.to_owned(),
                to: LAB.into(),
                kind: LinkKind::Uses,
            });
        }
    }
    match args.get("reports_to") {
        None => {}
        Some(Value::Null) => team
            .links
            .retain(|l| !(l.from == id && l.kind == LinkKind::ReportsTo)),
        Some(Value::String(lead)) if lead.trim().is_empty() => team
            .links
            .retain(|l| !(l.from == id && l.kind == LinkKind::ReportsTo)),
        Some(Value::String(lead)) => {
            let lead = agent_id(team, lead)?;
            if lead == id {
                return Err("an agent can't report to itself".into());
            }
            team.links
                .retain(|l| !(l.from == id && l.kind == LinkKind::ReportsTo));
            team.links.push(Link {
                from: id.to_owned(),
                to: lead,
                kind: LinkKind::ReportsTo,
            });
        }
        Some(_) => return Err("`reports_to` is an agent's name, or null for none".into()),
    }
    Ok(())
}

/// A link from its description: from, to, kind and rounds.
fn add_link(team: &mut Team, spec: &Value) -> Result<(), String> {
    let from = agent_id(team, text(spec, "from")?)?;
    let to_name = text(spec, "to")?;
    let rounds = number(spec, "rounds")?.unwrap_or(1);
    let rounds = u32::try_from(rounds).unwrap_or(u32::MAX);
    let kind = parse_kind(text(spec, "kind")?, rounds)?;
    let to = if to_name.eq_ignore_ascii_case("lab") {
        LAB.to_owned()
    } else {
        agent_id(team, to_name)?
    };
    let kind = if to == LAB { LinkKind::Uses } else { kind };
    if kind == LinkKind::Uses && to != LAB {
        return Err("a uses link goes to Lab: set uses_lab on the agent instead".into());
    }
    if from == to {
        return Err("an agent can't be linked to itself".into());
    }
    if kind == LinkKind::ReportsTo {
        team.links
            .retain(|l| !(l.from == from && l.kind == LinkKind::ReportsTo));
    }
    team.links
        .retain(|l| !(l.from == from && l.to == to && l.kind.same_kind(kind)));
    team.links.push(Link { from, to, kind });
    Ok(())
}

// --- Setup files -----------------------------------------------------------------------------

/// What a setup file says it is.
pub const FORMAT: &str = "persephone-agents/1";

const TEAM_KEYS: [&str; 9] = [
    "format",
    "name",
    "brief",
    "principles",
    "at_once",
    "lab",
    "agents",
    "links",
    "view",
];
const AGENT_KEYS: [&str; 11] = [
    "name",
    "role",
    "model",
    "effort",
    "budget",
    "spend_cap",
    "memory",
    "app_notes",
    "uses_lab",
    "reports_to",
    "at",
];
const LINK_KEYS: [&str; 4] = ["from", "to", "kind", "rounds"];
const LAB_KEYS: [&str; 3] = ["data", "folder", "at"];
const VIEW_KEYS: [&str; 2] = ["at", "zoom"];

/// A team's whole setup, as a file: everything but its id and its agents' memory, by name, so
/// it can be kept, shared, or written by hand or by another AI. Lab's program isn't in it, as
/// that's where Lab is on this computer.
#[must_use]
pub fn export(team: &Team) -> Value {
    let agents: Vec<Value> = team
        .agents
        .iter()
        .map(|agent| {
            json!({
                "name": agent.name,
                "role": agent.role,
                "model": agent.model.cli(),
                "effort": agent.effort.cli(),
                "budget": agent.budget,
                "spend_cap": agent.spend_cap,
                "memory": agent.memory,
                "app_notes": agent.app_notes,
                "uses_lab": team.uses_lab(&agent.id),
                "reports_to": team.lead_of(&agent.id).map(|id| team.name_of(id)),
                "at": agent.at,
            })
        })
        .collect();
    // Leads are said by each agent's reports_to; the rest are links.
    let links: Vec<Value> = team
        .links
        .iter()
        .filter(|l| matches!(l.kind, LinkKind::HandsTo | LinkKind::Reviews { .. }))
        .map(|link| {
            let mut out = json!({
                "from": team.name_of(&link.from),
                "to": team.name_of(&link.to),
                "kind": kind_name(link.kind),
            });
            if let LinkKind::Reviews { rounds } = link.kind {
                out["rounds"] = json!(rounds);
            }
            out
        })
        .collect();
    let mut lab = json!({"at": team.lab.at});
    match &team.lab.data {
        LabData::App => lab["data"] = json!("app"),
        LabData::Own(dir) => {
            lab["data"] = json!("own");
            if LabData::Own(dir.clone()) != LabData::own_default() {
                lab["folder"] = json!(tilde(dir));
            }
        }
    }
    json!({
        "format": FORMAT,
        "name": team.name,
        "brief": team.brief,
        "principles": team.principles,
        "at_once": team.at_once,
        "lab": lab,
        "agents": agents,
        "links": links,
        "view": {"at": team.view.at, "zoom": team.view.zoom},
    })
}

/// A team from a setup file, with a new id; its agents start with no memory. Every key is
/// checked, so a misspelt one is said rather than ignored. The cards stay where the file puts
/// them if it places every one; otherwise they're laid out as a tree.
///
/// # Errors
///
/// Says what in the file is wrong, and where.
pub fn import(setup: &Value) -> Result<Team, String> {
    only(setup, &TEAM_KEYS, "The setup")?;
    if let Some(format) = setup.get("format") {
        let format = format.as_str().unwrap_or_default();
        if format != FORMAT {
            return Err(format!(
                "the setup's format is `{format}`, and this reads `{FORMAT}`"
            ));
        }
    }
    let name = setup
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .ok_or("the setup needs a `name` for the team")?;
    kinds(
        setup,
        &[
            ("brief", "text"),
            ("principles", "text"),
            ("at_once", "whole number"),
        ],
        "The setup",
    )?;
    let mut team = Team::new(name, Starter::Blank);
    team_fields(&mut team, setup)?;

    let specs = match setup.get("agents") {
        None => Vec::new(),
        Some(agents) => agents
            .as_array()
            .ok_or("`agents` is a list of objects, one for each agent")?
            .clone(),
    };
    let mut leads = Vec::new();
    let mut placed = !specs.is_empty();
    for (n, spec) in specs.iter().enumerate() {
        let what = format!("Agent {}", n + 1);
        only(spec, &AGENT_KEYS, &what)?;
        kinds(
            spec,
            &[
                ("name", "text"),
                ("role", "text"),
                ("model", "text"),
                ("effort", "text"),
                ("budget", "whole number"),
                ("spend_cap", "number"),
                ("memory", "true or false"),
                ("app_notes", "true or false"),
                ("uses_lab", "true or false"),
                ("reports_to", "text or null"),
                ("at", "[x, y]"),
            ],
            &what,
        )?;
        let agent_name = spec
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| format!("{what} needs a `name`"))?;
        if team.agents.iter().any(|a| key(&a.name) == key(agent_name)) {
            return Err(format!(
                "two agents are called `{agent_name}`: names must differ"
            ));
        }
        let id = team.add_agent([0.0, 0.0]);
        if let Some(agent) = team.agent_mut(&id) {
            agent.name = agent_name.to_owned();
        }
        let mut fields = spec.clone();
        if let Some(lead) = fields.as_object_mut().and_then(|f| f.remove("reports_to")) {
            leads.push((id.clone(), lead));
        }
        if fields.get("uses_lab").is_none() {
            fields["uses_lab"] = json!(true);
        }
        placed &= fields.get("at").is_some();
        agent_fields(&mut team, &id, &fields).map_err(|e| format!("{agent_name}: {e}"))?;
    }
    for (id, lead) in leads {
        agent_fields(&mut team, &id, &json!({"reports_to": lead}))
            .map_err(|e| format!("{}: {e}", team.name_of(&id)))?;
    }
    if let Some(links) = setup.get("links") {
        let links = links
            .as_array()
            .ok_or("`links` is a list of objects, each with from, to and kind")?;
        for (n, spec) in links.iter().enumerate() {
            let what = format!("Link {}", n + 1);
            only(spec, &LINK_KEYS, &what)?;
            kinds(
                spec,
                &[
                    ("from", "text"),
                    ("to", "text"),
                    ("kind", "text"),
                    ("rounds", "whole number"),
                ],
                &what,
            )?;
            add_link(&mut team, spec).map_err(|e| format!("{what}: {e}"))?;
        }
    }
    if placed {
        if let Some(view) = setup.get("view") {
            only(view, &VIEW_KEYS, "`view`")?;
            if let Some(at) = point(view.get("at")) {
                team.view.at = at;
            }
            if let Some(zoom) = view.get("zoom").and_then(Value::as_f64) {
                #[allow(clippy::cast_possible_truncation)]
                let zoom = zoom as f32;
                team.view.zoom = zoom.clamp(0.3, 2.5);
            }
        }
    } else {
        tidy(&mut team);
    }
    if let Some(lab) = setup.get("lab") {
        only(lab, &LAB_KEYS, "`lab`")?;
        let folder = lab.get("folder").and_then(Value::as_str);
        team.lab.data = match (lab.get("data").and_then(Value::as_str), folder) {
            (Some("app"), _) => LabData::App,
            (Some("own") | None, Some(folder)) => LabData::Own(expand(folder)),
            (Some("own") | None, None) => LabData::own_default(),
            (Some(other), _) => {
                return Err(format!("`lab.data` is own or app, not `{other}`"));
            }
        };
        if placed && let Some(at) = point(lab.get("at")) {
            team.lab.at = at;
        }
    }
    Ok(team)
}

/// A setup from a file.
///
/// # Errors
///
/// Says why it couldn't be read.
pub fn read_setup(path: &Path) -> Result<Value, String> {
    let text =
        fs::read_to_string(path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} isn't JSON: {e}", path.display()))
}

/// Refuses keys a part of a setup doesn't take.
fn only(value: &Value, keys: &[&str], what: &str) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{what} is a JSON object"))?;
    match object.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(stray) => Err(format!(
            "{what} has `{stray}`, which a setup doesn't take; it takes {}",
            keys.join(", ")
        )),
        None => Ok(()),
    }
}

/// Refuses values of the wrong kind.
fn kinds(value: &Value, wanted: &[(&str, &str)], what: &str) -> Result<(), String> {
    for (name, kind) in wanted {
        let Some(found) = value.get(*name) else {
            continue;
        };
        let fits = match *kind {
            "text" => found.is_string(),
            "text or null" => found.is_string() || found.is_null(),
            "whole number" => found.is_u64(),
            "number" => found.is_number(),
            "true or false" => found.is_boolean(),
            "[x, y]" => point(Some(found)).is_some(),
            _ => true,
        };
        if !fits {
            return Err(format!("{what}'s `{name}` should be {kind}, not {found}"));
        }
    }
    Ok(())
}

fn point(value: Option<&Value>) -> Option<[f32; 2]> {
    let xy = value?.as_array()?;
    #[allow(clippy::cast_possible_truncation)]
    match xy[..] {
        [Value::Number(ref x), Value::Number(ref y)] => {
            Some([x.as_f64()? as f32, y.as_f64()? as f32])
        }
        _ => None,
    }
}

/// A path, with a leading `~` as the home folder.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => crate::team::home().join(rest),
        None => PathBuf::from(path),
    }
}

/// A path, with the home folder as `~`, so a file works on another computer.
fn tilde(path: &Path) -> String {
    match path.strip_prefix(crate::team::home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// How far apart cards are laid out.
const WIDE: f32 = 260.0;
const TALL: f32 = 170.0;

/// Puts a new card under its lead, beside its fellows, or else beside the other sessions.
fn place(team: &mut Team, id: &str) {
    let at = match team.lead_of(id).and_then(|lead| team.agent(lead)) {
        Some(lead) => {
            let right = team
                .reports_of(&lead.id)
                .iter()
                .filter(|a| a.id != id)
                .map(|a| a.at[0])
                .fold(f32::NEG_INFINITY, f32::max);
            let x = if right.is_finite() {
                right + WIDE
            } else {
                lead.at[0]
            };
            [x, lead.at[1] + TALL]
        }
        None => {
            let right = team
                .agents
                .iter()
                .filter(|a| a.id != id)
                .map(|a| a.at[0])
                .fold(f32::NEG_INFINITY, f32::max);
            [if right.is_finite() { right + WIDE } else { 0.0 }, 0.0]
        }
    };
    if let Some(agent) = team.agent_mut(id) {
        agent.at = at;
    }
}

/// Lays the cards out as a tree: each lead centred over its team, sessions side by side, and
/// Lab below them all.
pub fn tidy(team: &mut Team) {
    fn lay(
        team: &Team,
        id: &str,
        depth: usize,
        next: &mut f32,
        places: &mut HashMap<String, [f32; 2]>,
    ) -> f32 {
        let reports: Vec<String> = if depth > team.agents.len() {
            Vec::new()
        } else {
            team.reports_of(id).iter().map(|a| a.id.clone()).collect()
        };
        let x = if reports.is_empty() {
            let x = *next;
            *next += WIDE;
            x
        } else {
            let xs: Vec<f32> = reports
                .iter()
                .map(|r| lay(team, r, depth + 1, next, places))
                .collect();
            f32::midpoint(xs[0], xs[xs.len() - 1])
        };
        #[allow(clippy::cast_precision_loss)]
        places.insert(id.to_owned(), [x, depth as f32 * TALL]);
        x
    }
    let roots: Vec<String> = team
        .agents
        .iter()
        .filter(|a| team.is_root(&a.id))
        .map(|a| a.id.clone())
        .collect();
    let mut next = 0.0;
    let mut places = HashMap::new();
    for root in &roots {
        lay(team, root, 0, &mut next, &mut places);
    }
    let shift = (next - WIDE).max(0.0) / 2.0;
    let mut bottom: f32 = 0.0;
    for agent in &mut team.agents {
        if let Some([x, y]) = places.get(&agent.id) {
            agent.at = [x - shift, *y];
            bottom = bottom.max(*y);
        }
    }
    team.lab.at = [0.0, bottom + TALL + 30.0];
    team.view.at = [0.0, (bottom + TALL) / 2.0];
    team.view.zoom = 1.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desk(name: &str) -> Desk {
        let root =
            std::env::temp_dir().join(format!("persephone-desk-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        Desk {
            root,
            claude: None,
            runner: None,
        }
    }

    fn call(desk: &Desk, tool: &str, args: Value) -> Value {
        desk.dispatch(tool, &args)
            .unwrap_or_else(|e| panic!("{tool}: {e}"))
    }

    #[test]
    fn a_team_is_built_whole_in_one_call() {
        let desk = desk("whole");
        let team = call(
            &desk,
            "create_team",
            json!({
                "name": "BTC desk",
                "brief": "Is there anything in BTC 1h after costs?",
                "agents": [
                    {"name": "Head", "role": "Leads.", "model": "opus", "effort": "high", "spend_cap": 12},
                    {"name": "Trend", "role": "Tests trend rules.", "model": "Sonnet 5.5", "reports_to": "Head"},
                    {"name": "Carry", "role": "Tests funding carry.", "model": "sonnet", "reports_to": "Head"},
                    {"name": "Skeptic", "role": "Re-runs the head's claims.", "model": "haiku", "effort": "extra high"}
                ],
                "links": [{"from": "Skeptic", "to": "Head", "kind": "reviews", "rounds": 2}]
            }),
        );
        assert_eq!(team["problems"], json!([]), "{team}");
        assert_eq!(team["agents"][1]["reports_to"], "Head");
        assert_eq!(team["agents"][3]["effort"], "xhigh");
        assert_eq!(team["agents"][0]["its_team"], json!(["Trend", "Carry"]));
        assert_eq!(
            team["steps"],
            json!([
                "Head: work",
                "Skeptic: review of Head, round 1",
                "Head: answer, round 1",
                "Skeptic: review of Head, round 2",
                "Head: answer, round 2"
            ])
        );
        assert_eq!(
            team["links"],
            json!([
                {"from": "Trend", "to": "Head", "kind": "reports_to"},
                {"from": "Carry", "to": "Head", "kind": "reports_to"},
                {"from": "Skeptic", "to": "Head", "kind": "reviews", "rounds": 2}
            ])
        );
        // Laid out: the head over its two, the skeptic beside them.
        let at = |n: usize| team["agents"][n]["at"].clone();
        assert_eq!(at(0)[1], 0.0);
        assert_eq!(at(1)[1], 170.0);
        assert!(at(3)[0].as_f64() > at(2)[0].as_f64());
        // And it's there for the app, by name too.
        let listed = call(&desk, "list_teams", json!({}));
        assert_eq!(listed["teams"][0]["name"], "BTC desk");
        let again = call(&desk, "get_team", json!({"team": "btc desk"}));
        assert_eq!(again["id"], team["id"]);
        let _ = fs::remove_dir_all(&desk.root);
    }

    #[test]
    fn agents_and_links_change_by_name() {
        let desk = desk("edit");
        call(
            &desk,
            "create_team",
            json!({"name": "Pair", "starter": "researcher_and_reviewer", "brief": "Study ETH."}),
        );
        let team = call(
            &desk,
            "update_agent",
            json!({"team": "Pair", "agent": "Reviewer", "model": "haiku", "budget": 40, "name": "Critic"}),
        );
        assert_eq!(team["agents"][1]["name"], "Critic");
        assert_eq!(team["agents"][1]["model"], "haiku");
        let team = call(
            &desk,
            "add_agent",
            json!({"team": "Pair", "name": "Writer", "role": "Writes it up.", "reports_to": "Researcher"}),
        );
        assert_eq!(team["agents"][0]["its_team"], json!(["Writer"]));
        assert_eq!(team["agents"][2]["at"][1], 170.0, "under its lead");
        let team = call(
            &desk,
            "unlink",
            json!({"team": "Pair", "from": "Critic", "to": "Researcher"}),
        );
        assert_eq!(
            team["links"],
            json!([{"from": "Writer", "to": "Researcher", "kind": "reports_to"}])
        );
        let team = call(
            &desk,
            "update_agent",
            json!({"team": "Pair", "agent": "Writer", "reports_to": null}),
        );
        assert_eq!(team["links"], json!([]));
        let gone = call(
            &desk,
            "remove_agent",
            json!({"team": "Pair", "agent": "writer"}),
        );
        assert_eq!(gone["agents"].as_array().unwrap().len(), 2);
        let _ = fs::remove_dir_all(&desk.root);
    }

    #[test]
    fn mistakes_are_said_plainly() {
        let desk = desk("mistakes");
        let none = desk
            .dispatch("get_team", &json!({"team": "Nope"}))
            .unwrap_err();
        assert!(none.contains("no teams yet"), "{none}");
        call(
            &desk,
            "create_team",
            json!({"name": "Solo", "starter": "one_analyst"}),
        );
        let twice = desk
            .dispatch("create_team", &json!({"name": "solo"}))
            .unwrap_err();
        assert!(twice.contains("already"), "{twice}");
        let model = desk
            .dispatch(
                "update_agent",
                &json!({"team": "Solo", "agent": "Analyst", "model": "gpt-5"}),
            )
            .unwrap_err();
        assert!(model.contains("isn't a model"), "{model}");
        let who = desk
            .dispatch("update_agent", &json!({"team": "Solo", "agent": "Bob"}))
            .unwrap_err();
        assert!(who.contains("its agents are Analyst"), "{who}");
        let kind = desk
            .dispatch(
                "link",
                &json!({"team": "Solo", "from": "Analyst", "to": "Analyst", "kind": "likes"}),
            )
            .unwrap_err();
        assert!(kind.contains("isn't a kind of link"), "{kind}");
        let tool = desk.dispatch("make_coffee", &json!({})).unwrap_err();
        assert!(tool.contains("create_team"), "{tool}");
        // A team that can't run says why, before anything starts.
        let empty = desk
            .dispatch("run_team", &json!({"team": "Solo"}))
            .unwrap_err();
        assert!(empty.contains("brief is empty"), "{empty}");
        let _ = fs::remove_dir_all(&desk.root);
    }

    #[test]
    fn a_setup_exported_imports_as_the_same_team() {
        let desk = desk("roundtrip");
        call(
            &desk,
            "create_team",
            json!({
                "name": "Desk",
                "brief": "Study BTC.",
                "at_once": 3,
                "agents": [
                    {"name": "Head", "role": "Leads.", "model": "fable", "effort": "max", "spend_cap": 7.5, "memory": false},
                    {"name": "Quant", "role": "Tests.", "model": "haiku", "budget": 33, "reports_to": "Head", "uses_lab": true},
                    {"name": "Critic", "role": "Re-runs.", "app_notes": false, "uses_lab": false}
                ],
                "links": [{"from": "Critic", "to": "Head", "kind": "reviews", "rounds": 3}]
            }),
        );
        call(&desk, "set_lab", json!({"team": "Desk", "data": "app"}));
        let path = desk.root.join("desk.json");
        let exported = call(
            &desk,
            "export_team",
            json!({"team": "Desk", "path": path.display().to_string()}),
        );
        assert_eq!(exported["setup"]["format"], FORMAT);
        let imported = call(
            &desk,
            "import_team",
            json!({"path": path.display().to_string(), "name": "Desk 2"}),
        );
        let original = call(&desk, "get_team", json!({"team": "Desk"}));
        for field in [
            "brief",
            "principles",
            "at_once",
            "links",
            "lab",
            "steps",
            "most_spend",
            "problems",
        ] {
            let (mut a, mut b) = (original[field].clone(), imported[field].clone());
            if field == "lab" {
                a["folder"] = Value::Null;
                b["folder"] = Value::Null;
            }
            assert_eq!(a, b, "{field}");
        }
        for (a, b) in original["agents"]
            .as_array()
            .unwrap()
            .iter()
            .zip(imported["agents"].as_array().unwrap())
        {
            for field in [
                "name",
                "role",
                "model",
                "effort",
                "budget",
                "spend_cap",
                "memory",
                "app_notes",
                "uses_lab",
                "reports_to",
                "at",
            ] {
                assert_eq!(a[field], b[field], "{field}");
            }
            assert_ne!(a["id"], b["id"], "new agents, with no memory");
        }
        let _ = fs::remove_dir_all(&desk.root);
    }

    #[test]
    fn a_setup_with_a_mistake_says_where() {
        let wrong = |setup: Value| import(&setup).unwrap_err();
        assert!(
            wrong(json!({"name": "T", "agents": [{"name": "A", "modle": "opus"}]}))
                .contains("Agent 1 has `modle`")
        );
        assert!(
            wrong(json!({"name": "T", "agents": [{"name": "A", "budget": "lots"}]}))
                .contains("`budget` should be whole number")
        );
        assert!(
            wrong(json!({"name": "T", "agents": [{"name": "A", "model": "gpt"}]}))
                .contains("A: `gpt` isn't a model")
        );
        assert!(
            wrong(json!({"name": "T", "agents": [{"name": "A"}, {"name": "a"}]}))
                .contains("two agents")
        );
        assert!(wrong(json!({"name": "T", "agents": [{"name": "A"}], "links": [{"from": "A", "to": "B", "kind": "hands_to"}]})).contains("Link 1: there's no agent called `B`"));
        assert!(wrong(json!({"name": "T", "team": "x"})).contains("The setup has `team`"));
        assert!(wrong(json!({"format": "persephone-agents/9", "name": "T"})).contains("format"));
        assert!(wrong(json!({"brief": "x"})).contains("needs a `name`"));
        // Without places for every card, they're laid out.
        let team = import(
            &json!({"name": "T", "agents": [{"name": "A"}, {"name": "B", "reports_to": "A"}]}),
        )
        .unwrap();
        assert_eq!(team.agents[1].at[1], 170.0);
        assert!(team.uses_lab(&team.agents[0].id), "Lab unless it says not");
    }

    #[test]
    fn the_example_setup_imports_ready_to_run() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/examples/agents-setup.json");
        let team = import(&read_setup(&path).unwrap()).unwrap();
        assert_eq!(check(&team), vec![], "{}", team.name);
    }

    #[test]
    fn check_says_how_it_would_run_and_what_it_could_spend() {
        let desk = desk("check");
        call(
            &desk,
            "create_team",
            json!({"name": "Desk", "starter": "lead_and_analysts", "brief": "Study BTC."}),
        );
        let checked = call(&desk, "check_team", json!({"team": "Desk"}));
        assert_eq!(checked["steps"], json!(["Lead: work"]));
        assert_eq!(checked["most_spend"], 20.0);
        let _ = fs::remove_dir_all(&desk.root);
    }

    #[test]
    fn every_tool_is_described_and_answers() {
        let names: Vec<&str> = tools().iter().map(|t| t.name).collect();
        assert!(names.len() >= 20);
        let desk = desk("all");
        for tool in tools() {
            assert!(tool.input_schema()["type"] == "object", "{}", tool.name);
            // With no arguments, each either answers or says what it needs: none falls through.
            if let Err(said) = desk.dispatch(tool.name, &json!({})) {
                assert!(!said.contains("there's no tool"), "{}: {said}", tool.name);
            }
        }
        assert!(reference().contains("## `create_team`"));
        let _ = fs::remove_dir_all(&desk.root);
    }
}
