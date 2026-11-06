//! What each agent is told: the `CLAUDE.md` of an agent at the top of a session, the prompt of
//! each member, and what starts each step. Built from the agent kit's, with the role, the team
//! and the links written in.

use std::fmt::Write as _;

use serde_json::{Map, Value, json};

use crate::{
    plan::{Plan, Session, StepKind},
    team::{Agent, LinkKind, Team, key},
};

/// The first line of a role, for where there's room for only that.
#[must_use]
pub fn first_line(role: &str) -> &str {
    let line = role.trim().lines().next().unwrap_or_default();
    // A sentence, if the line goes on.
    match line.find(". ") {
        Some(end) if end >= 10 => &line[..=end],
        _ => line,
    }
}

/// The `CLAUDE.md` of the agent at the top of `session`.
#[must_use]
pub fn claude_md(team: &Team, session: &Session) -> String {
    let Some(agent) = team.agent(&session.root) else {
        return String::new();
    };
    let memory = agent.memory;
    let mut out = String::new();
    let _ = write!(
        out,
        "# You\n\nYou are {}, an agent in the team \"{}\". You work through Persephone, a backtesting app, as a professional and as one of its beta users.\n\n## Your role\n\n{}\n\n## How you reach the data\n\n",
        agent.name,
        team.name,
        agent.role.trim()
    );
    let own_lab = team.uses_lab(&agent.id);
    if own_lab {
        out.push_str("You work through Persephone Lab, its tools for agents (`mcp__persephone-lab__*`). Call `get_guide` when you need to know how a tool works");
        out.push_str(if memory {
            ", and keep what you learn about Lab in `memory/lab.md`.\n\n"
        } else {
            ".\n\n"
        });
    } else if session.lab {
        out.push_str("Your team uses Persephone Lab, so its tools (`mcp__persephone-lab__*`) reach you too. Leave the Lab work to your team, unless you're checking what they found.\n\n");
    } else {
        out.push_str(
            "You don't have Lab's tools. You work from your inbox, your files, and what your team brings back.\n\n",
        );
    }
    out.push_str("You can't see or change Persephone's code, use a shell, or reach the web. That's on purpose: Lab is the only way to the data, and you're a user, not a developer. Write only inside this folder.\n\n");
    if memory {
        out.push_str("You start knowing nothing about these markets beyond what's in `memory/`. What's there you found yourself, or were told by your user or your team. Don't bring in market folklore as fact. If you believe something, it's in `memory/beliefs.md` with the runs that show it, or it's a question in `memory/questions.md`.\n\n");
    } else {
        out.push_str("You start knowing nothing about these markets beyond what you're given. Don't bring in market folklore as fact.\n\n");
    }
    if agent.app_notes {
        out.push_str("## The app\n\nAs a user with a quant's eye, write down what's broken in Persephone in `app/bugs.md`, and what would make it better in `app/ideas.md`.\n\n");
    }
    out.push_str(&team_section(team, &agent.id, true));
    out.push_str("@principles.md\n@vetoed.md\n");
    if memory {
        out.push_str("@memory/index.md\n");
    }

    out.push_str("\n## Start of every session\n\n");
    let mut steps: Vec<&str> = Vec::new();
    steps.push(if memory {
        "Read `subject.md`, `tasks.md`, and the newest file in `memory/journal/`."
    } else {
        "Read `subject.md`."
    });
    steps.push("Read everything the start prompt says is in your inbox for this run.");
    if memory {
        steps.push("Read whatever in `memory/` the work needs. `memory/index.md` is above.");
        steps.push("Do what the start prompt asks, working from the top open task in `tasks.md`, or carry on where the last session stopped.");
    } else {
        steps.push("Do what the start prompt asks.");
    }
    numbered(&mut out, &steps);

    out.push_str("\n## While you work\n\n");
    if memory {
        out.push_str("- **Count every variant you try** in `memory/tried.md` as you go (setting, rule, span, market), and pass the running total as `variants_tried` wherever a tool takes it.\n");
        out.push_str("- **Record beliefs as you form them** in `memory/beliefs.md`:\n  - one line each: claim, status (`open`, `supported`, `refuted`, `stale`), confidence, date, and the run ids behind it\n  - a claim with no run ids is `open`\n  - revise and refute your own beliefs when new runs say so, keeping the old line struck through with the reason\n");
        out.push_str("- **Re-test** any `supported` belief older than 60 days when it bears on the work. Mark it `stale` until you have.\n");
    } else {
        out.push_str("- **Count every variant you try** (setting, rule, span, market), and pass the running total as `variants_tried` wherever a tool takes it.\n");
    }
    if agent.app_notes {
        out.push_str("- **When a Lab answer looks wrong,** contradicts another tool, or isn't what the guide says, add it to `app/bugs.md` straight away with the exact call. Then carry on.\n");
    }
    out.push_str("- **Keep each run whose figures you rely on:** note its `run_id`, and put weight tables or position lists in `runs/`.\n");

    out.push_str("\n## End of every session, before you stop\n\n");
    let mut steps = vec![
        "Write your report for this run in `outbox/report.md`: what you set out to do, what you found, the run ids it rests on, and how sure you are. Whoever you hand your work to, or who reviews it, reads only that, and whatever else you put in `outbox/`.",
    ];
    if memory {
        steps.push("Write `memory/journal/YYYY-MM-DD-HHMM.md`: what you set out to do, what you did, what you found, what you'd do next, and the count of variants tried.");
        steps.push("Update `tasks.md`: tick what's done, and add notes to what's carried over.");
        steps.push("Rewrite `memory/index.md` so it's a true one-page map of everything in `memory/` now. Keep it under about 60 lines: it's loaded every session.");
    }
    steps.push("Make sure everything you wrote is saved. Your folder is committed to git when you stop, and your user reads that history.");
    numbered(&mut out, &steps);

    out.push_str("\n## Your user\n\nThey own `principles.md`, `subject.md` and `vetoed.md`. Don't edit those. `vetoed.md` lists things they reverted and why: don't reassert them unless new evidence says otherwise, and then say so plainly.\n");
    out
}

/// A member's prompt, its whole instructions: it doesn't read the lead's `CLAUDE.md`.
#[must_use]
pub fn member_prompt(team: &Team, agent: &Agent) -> String {
    let lead = team
        .lead_of(&agent.id)
        .map_or("your lead", |id| team.name_of(id));
    let member_key = key(&agent.name);
    let mut out = String::new();
    let _ = write!(
        out,
        "You are {}, an agent in the team \"{}\". You work through Persephone, a backtesting app. You report to {lead}, who hands you work and checks what you bring back.\n\n## Your role\n\n{}\n\n## How you reach the data\n\n",
        agent.name,
        team.name,
        agent.role.trim()
    );
    out.push_str(if team.uses_lab(&agent.id) {
        "You have Persephone Lab's tools (`mcp__persephone-lab__*`). Call `get_guide` when you need to know how one works.\n\n"
    } else {
        "You don't have Lab's tools. Work from what you're given and the files here.\n\n"
    });
    out.push_str("You can't see or change Persephone's code, use a shell, or reach the web. Write only inside this folder.\n\nYou start knowing nothing about these markets beyond what you're told and what you find. Don't bring in market folklore as fact.\n\n");
    out.push_str(&team_section(team, &agent.id, false));
    let _ = write!(
        out,
        "## How to work\n\nYour user's principles, which hold for the whole team:\n\n{}\n\n",
        team.principles.trim()
    );
    if agent.memory {
        let _ = write!(
            out,
            "## Your notes\n\nBefore you start, read `memory/team/{member_key}.md` if it's there: notes you left yourself in earlier runs. Before you finish, add what a future you should know to it, such as what worked in Lab, what didn't, and what you found. Keep it under about 40 lines.\n\n"
        );
    }
    if agent.app_notes {
        out.push_str("## The app\n\nIf a Lab answer looks wrong, contradicts another tool, or isn't what the guide says, add it to `app/bugs.md` with the exact call: the tool, its arguments, what came back, and what you expected and why. Ideas that would make Persephone better go in `app/ideas.md`. Add to what's there, and don't rewrite it.\n\n");
    }
    let _ = write!(
        out,
        "## When you're done\n\nStay within {} tool calls. Reply with what you did, what you found, every run id it rests on, the count of variants you tried, and how sure you are.\n",
        agent.budget
    );
    out
}

/// What a lead is told about its team and how they work together; empty with no team.
fn team_section(team: &Team, lead: &str, at_top: bool) -> String {
    let reports = team.reports_of(lead);
    if reports.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## Your team\n\nYou lead these agents. Hand each one its work with the Agent tool, naming it as `subagent_type`, and say exactly what you want back. Each starts with only what you tell it, so give it everything it needs: the question, the markets and spans, the settings, and the run ids to build on. Check what each brings back before you rely on it, by re-running in Lab what matters, or having another of them re-run it. You can hand work to several at once.\n\n",
    );
    for report in &reports {
        let _ = writeln!(
            out,
            "- `{}`: {}, {}. {}",
            key(&report.name),
            report.name,
            report.model.label(),
            first_line(&report.role)
        );
    }
    let ids: Vec<&str> = reports.iter().map(|r| r.id.as_str()).collect();
    let mut together = Vec::new();
    for link in &team.links {
        if !ids.contains(&link.from.as_str()) || !ids.contains(&link.to.as_str()) {
            continue;
        }
        let (a, b) = (key(team.name_of(&link.from)), key(team.name_of(&link.to)));
        match link.kind {
            LinkKind::HandsTo => together.push(format!(
                "- `{b}` works from what `{a}` found: hand `{b}` its work after `{a}` has come back, with `{a}`'s findings and run ids."
            )),
            LinkKind::Reviews { rounds } => together.push(format!(
                "- `{a}` checks `{b}`: before you accept `{b}`'s results, give `{a}` its findings and run ids to re-run in Lab, then give `{b}` what `{a}` found, to answer. Up to {rounds} round{}.",
                if rounds == 1 { "" } else { "s" }
            )),
            _ => {}
        }
    }
    if !together.is_empty() {
        out.push_str("\nHow they work together:\n\n");
        for line in together {
            out.push_str(&line);
            out.push('\n');
        }
    }
    if reports.iter().any(|r| r.memory) {
        out.push_str(if at_top {
            "\nEach keeps notes in `memory/team/<name>.md`. Those are theirs: don't edit them.\n"
        } else {
            "\nEach keeps notes in `memory/team/<name>.md`. Those are theirs.\n"
        });
    }
    out.push('\n');
    out
}

fn numbered(out: &mut String, steps: &[&str]) {
    for (n, step) in steps.iter().enumerate() {
        let _ = writeln!(out, "{}. {step}", n + 1);
    }
}

/// The tools a member may use, for its `tools` in `--agents`.
#[must_use]
pub fn member_tools(team: &Team, agent: &Agent) -> Vec<String> {
    let mut tools: Vec<String> = ["Read", "Edit", "Write", "Glob", "Grep"]
        .map(String::from)
        .into();
    if team.uses_lab(&agent.id) {
        tools.push("mcp__persephone-lab".into());
    }
    let reports: Vec<String> = team
        .reports_of(&agent.id)
        .iter()
        .map(|r| key(&r.name))
        .collect();
    if !reports.is_empty() {
        tools.push(format!("Agent({})", reports.join(", ")));
    }
    tools
}

/// `--agents`: every member of `session`.
#[must_use]
pub fn members_json(team: &Team, session: &Session) -> Value {
    let mut agents = Map::new();
    for member in &session.members {
        let Some(agent) = team.agent(&member.id) else {
            continue;
        };
        agents.insert(
            member.key.clone(),
            json!({
                "description": format!("{}: {}", agent.name, first_line(&agent.role)),
                "prompt": member_prompt(team, agent),
                "model": agent.model.cli(),
                "effort": agent.effort.cli(),
                "tools": member_tools(team, agent),
                // Room past its budget for its answer, and a hard stop after that.
                "maxTurns": agent.budget + 10,
                "omitClaudeMd": true,
                "background": false,
            }),
        );
    }
    Value::Object(agents)
}

/// The tool calls a session is asked to stay within: its top agent's and every member's.
#[must_use]
pub fn session_budget(team: &Team, session: &Session) -> u32 {
    std::iter::once(&session.root)
        .chain(session.members.iter().map(|m| &m.id))
        .filter_map(|id| team.agent(id))
        .map(|a| a.budget)
        .sum()
}

/// What starts `step` of `run`.
#[must_use]
pub fn start_prompt(team: &Team, plan: &Plan, step: usize, run: &str) -> String {
    let this = &plan.steps[step];
    let session = &plan.sessions[this.session];
    let Some(agent) = team.agent(&session.root) else {
        return String::new();
    };
    let budget = if session.members.is_empty() {
        format!("Use at most {} tool calls this session", agent.budget)
    } else {
        format!(
            "Use at most {} tool calls this session yourself, and your team at most {} between them",
            agent.budget,
            session_budget(team, session) - agent.budget
        )
    };
    let inbox = |folder: &str| format!("`inbox/{run}/{folder}/`");
    // What's handed to it, but not the report under review, nor the reviews to answer: those
    // are said on their own.
    let handed: Vec<String> = this
        .inputs
        .iter()
        .filter(|input| match this.kind {
            StepKind::Work => true,
            StepKind::Review { author, .. } => input.from != plan.sessions[author].root,
            StepKind::Answer { .. } => false,
        })
        .map(|input| {
            format!(
                "{}'s report and files are in {}.",
                team.name_of(&input.from),
                inbox(&input.folder)
            )
        })
        .collect();
    let handed = if handed.is_empty() {
        String::new()
    } else {
        format!(" {}", handed.join(" "))
    };
    match this.kind {
        StepKind::Work => format!(
            "Work as CLAUDE.md says. This is run {run}.{handed} {budget}, then do the end-of-session steps."
        ),
        StepKind::Review { author, round } => {
            let author = team.name_of(&plan.sessions[author].root);
            let folder = this
                .inputs
                .iter()
                .find(|input| input.folder.ends_with(&format!("-round-{round}")))
                .map_or_else(String::new, |input| inbox(&input.folder));
            let again = if round > 1 {
                format!(
                    " This is round {round}: {author} has answered your last review, so check what it changed and whether it holds now."
                )
            } else {
                String::new()
            };
            format!(
                "Work as CLAUDE.md says. This is run {run}, and your job in it is to review {author}'s work: its report and files are in {folder}.{again}{handed} Re-run in Lab the claims that matter, with the same settings, and with tests that could break them. Write your review in `outbox/report.md`: what holds, what doesn't, and the run ids that show it. {budget}, then do the end-of-session steps."
            )
        }
        StepKind::Answer { round } => {
            let reviews: Vec<String> = this
                .inputs
                .iter()
                .map(|input| {
                    format!(
                        "{}'s is in {}",
                        team.name_of(&input.from),
                        format_args!("`inbox/{run}/{}/report.md`", input.folder)
                    )
                })
                .collect();
            format!(
                "Your report has been reviewed (round {round}): {}. Answer it: re-run what it questions, change your findings where it's right, say plainly where you disagree and why, and rewrite `outbox/report.md`. {budget}, then do the end-of-session steps.",
                reviews.join("; ")
            )
        }
    }
}

/// `subject.md`: the brief.
#[must_use]
pub fn subject_md(team: &Team) -> String {
    format!(
        "# Subject\n\nWhat your user wants studied now. You read this at the start of every session.\n\n{}\n",
        team.brief.trim()
    )
}

/// A new agent's `tasks.md`.
#[must_use]
pub fn tasks_md(lab: bool) -> String {
    let mut out = String::from(
        "# Tasks\n\nYour user adds jobs; you work the top open one, tick it when done, and leave notes on any you carry over to the next session.\n",
    );
    if lab {
        out.push_str("\n- [ ] Learn Lab: read `get_guide` and the topics it points to, and write what matters in `memory/lab.md`.\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        plan::plan,
        team::{Link, Starter},
    };

    fn desk() -> Team {
        let mut team = Team::new("Desk", Starter::LeadAndAnalysts);
        team.brief = "Is there anything in BTC 1h?".into();
        team
    }

    #[test]
    fn a_lead_is_told_its_team() {
        let team = desk();
        let plan = plan(&team).unwrap();
        let md = claude_md(&team, &plan.sessions[0]);
        assert!(
            md.contains("You are Lead, an agent in the team \"Desk\""),
            "{md}"
        );
        assert!(md.contains("- `analyst-a`: Analyst A, Sonnet 5.5."), "{md}");
        assert!(md.contains("@memory/index.md"));
        assert!(md.contains("`outbox/report.md`"));
        assert!(md.contains("`app/bugs.md`"));
    }

    #[test]
    fn what_an_agent_is_told_follows_its_switches() {
        let mut team = Team::new("Solo", Starter::OneAnalyst);
        team.agents[0].memory = false;
        team.agents[0].app_notes = false;
        team.links.clear();
        let plan = plan(&team).unwrap();
        let md = claude_md(&team, &plan.sessions[0]);
        assert!(!md.contains("memory/"), "{md}");
        assert!(!md.contains("app/"), "{md}");
        assert!(md.contains("You don't have Lab's tools"));
    }

    #[test]
    fn members_are_defined_for_claude_code() {
        let mut team = desk();
        let (a, b) = (team.agents[1].id.clone(), team.agents[2].id.clone());
        team.links.push(Link {
            from: b,
            to: a,
            kind: LinkKind::Reviews { rounds: 2 },
        });
        let plan = plan(&team).unwrap();
        let json = members_json(&team, &plan.sessions[0]);
        let analyst = &json["analyst-a"];
        assert_eq!(analyst["model"], "sonnet");
        assert_eq!(analyst["effort"], "medium");
        assert_eq!(analyst["omitClaudeMd"], true);
        assert_eq!(analyst["maxTurns"], 90);
        let tools: Vec<&str> = analyst["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert!(tools.contains(&"mcp__persephone-lab"));
        assert!(!tools.iter().any(|t| t.starts_with("Agent")), "{tools:?}");
        let prompt = analyst["prompt"].as_str().unwrap();
        assert!(prompt.contains("You report to Lead"));
        assert!(
            prompt.contains("Count every variant tried"),
            "the principles"
        );
        assert!(prompt.contains("memory/team/analyst-a.md"));

        let md = claude_md(&team, &plan.sessions[0]);
        assert!(
            md.contains("- `analyst-b` checks `analyst-a`"),
            "the review between them is the lead's to run: {md}"
        );
        assert!(md.contains("Up to 2 rounds."));
    }

    #[test]
    fn a_member_with_a_team_may_call_only_it() {
        let mut team = desk();
        let id = team.add_agent([0.0, 300.0]);
        let analyst = team.agents[1].id.clone();
        team.links.push(Link {
            from: id,
            to: analyst,
            kind: LinkKind::ReportsTo,
        });
        let tools = member_tools(&team, &team.agents[1]);
        assert_eq!(tools.last().unwrap(), "Agent(agent-4)");
    }

    #[test]
    fn steps_start_with_where_their_inputs_are() {
        let mut team = Team::new("Pair", Starter::ResearcherAndReviewer);
        team.brief = "Study ETH.".into();
        let plan = plan(&team).unwrap();
        let work = start_prompt(&team, &plan, 0, "r1");
        assert!(
            work.starts_with("Work as CLAUDE.md says. This is run r1."),
            "{work}"
        );
        assert!(work.contains("at most 150 tool calls"));
        let review = start_prompt(&team, &plan, 1, "r1");
        assert!(
            review.contains("review Researcher's work: its report and files are in `inbox/r1/researcher-round-1/`"),
            "{review}"
        );
        let answer = start_prompt(&team, &plan, 2, "r1");
        assert!(
            answer.contains("Reviewer's is in `inbox/r1/reviewer-round-1/report.md`"),
            "{answer}"
        );
    }

    #[test]
    fn a_lead_shares_out_its_teams_budget() {
        let team = desk();
        let plan = plan(&team).unwrap();
        assert_eq!(session_budget(&team, &plan.sessions[0]), 60 + 80 + 80);
        let work = start_prompt(&team, &plan, 0, "r1");
        assert!(
            work.contains("at most 60 tool calls this session yourself, and your team at most 160"),
            "{work}"
        );
    }

    #[test]
    fn first_lines_are_short() {
        assert_eq!(
            first_line("Leads the research. Turns the brief into questions."),
            "Leads the research."
        );
        assert_eq!(first_line("Short. Then more."), "Short. Then more.");
        assert_eq!(first_line("\n  One line\nTwo"), "One line");
    }
}
