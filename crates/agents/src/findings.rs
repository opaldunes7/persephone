//! What a run found, as one Markdown document: the brief, each step's report in order, what
//! each agent now believes and remembers, the data they saved while it ran, and their bugs and
//! ideas for the app. For keeping, reading away from the app, or handing to another AI.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use crate::{
    folder,
    plan::Plan,
    run::{self, Status},
    team::{Team, key},
};

/// The most of a saved file put in the document; the rest is left in its folder.
const MOST_FILE: usize = 200_000;

/// `run` of `team`, as one Markdown document.
///
/// # Errors
///
/// Says why: there's no such run, or it left no record.
pub fn findings(root: &Path, team: &Team, run: &str) -> Result<String, String> {
    let dir = run::run_dir(root, team, run);
    let state = run::read_state(&dir).ok_or_else(|| format!("run {run} left no record"))?;
    let plan = run::read_plan(&dir);
    let name = |id: &str| team.name_of(id).to_owned();
    // What was written while it ran, give or take, is its.
    let from = state.started.saturating_sub(60);
    let until = state.ended.unwrap_or(u64::MAX).saturating_add(600);
    let during = |path: &Path| modified(path).is_some_and(|at| (from..=until).contains(&at));

    let mut out = String::new();
    let _ = writeln!(out, "# {}: what run {run} found\n", team.name);
    let _ = writeln!(out, "## The brief\n\n{}\n", team.brief.trim());

    let _ = writeln!(
        out,
        "## The run\n\n${:.2} at API prices (on a Claude subscription, a share of its usage limits, not a charge). {}\n",
        state.spend(),
        if state.going() {
            "It's still going: this is as far as it's got."
        } else if state.ended.is_some() {
            "It's ended."
        } else {
            "Its runner stopped before it ended."
        }
    );
    out.push_str("| Step | Status | Tool calls | Spend |\n|---|---|---|---|\n");
    for step in &state.steps {
        let _ = writeln!(
            out,
            "| {} | {} | {} | ${:.2} |",
            step.label,
            step.status.label(),
            step.calls,
            step.spend
        );
    }
    out.push('\n');

    out.push_str("## The reports, in order\n\n");
    let mut reported = false;
    if let Some(plan) = &plan {
        for (step, saved) in state.steps.iter().enumerate().take(plan.steps.len()) {
            let snapshot = dir.join("steps").join(run::step_name(plan, step));
            let Ok(report) = fs::read_to_string(snapshot.join("report.md")) else {
                continue;
            };
            reported = true;
            let _ = writeln!(out, "### {}\n\n{}\n", saved.label, down(report.trim(), 3));
            let others: Vec<String> = files(&snapshot)
                .into_iter()
                .filter(|f| f.file_name().is_some_and(|n| n != "report.md"))
                .map(|f| format!("`{}`", f.display()))
                .collect();
            if !others.is_empty() {
                let _ = writeln!(out, "Also handed on with it: {}.\n", others.join(", "));
            }
            if matches!(saved.status, Status::Failed(_) | Status::Stopped(_)) {
                let _ = writeln!(out, "_This step {}._\n", saved.status.label());
            }
        }
    }
    if !reported {
        out.push_str("No step wrote a report.\n\n");
    }

    let tops = tops(team, plan.as_ref());
    out.push_str("## What each agent believes and remembers now\n\n");
    for top in &tops {
        let agent_dir = folder::agent_dir(root, team, top);
        let _ = writeln!(out, "### {}\n", name(top));
        for (title, file) in [
            ("Beliefs", "memory/beliefs.md"),
            ("Variants tried", "memory/tried.md"),
            ("Open questions", "memory/questions.md"),
        ] {
            if let Some(body) = body_of(&agent_dir.join(file)) {
                let _ = writeln!(out, "#### {title}\n\n{}\n", down(&body, 4));
            }
        }
        for journal in files(&agent_dir.join("memory/journal")) {
            let path = agent_dir.join("memory/journal").join(&journal);
            if during(&path)
                && let Ok(text) = fs::read_to_string(&path)
            {
                let stem = journal.file_stem().unwrap_or_default().to_string_lossy();
                let _ = writeln!(out, "#### Journal, {stem}\n\n{}\n", down(text.trim(), 4));
            }
        }
        if let Some(plan) = &plan
            && let Some(session) = plan.sessions.iter().find(|s| &s.root == top)
        {
            for member in &session.members {
                let notes = agent_dir.join(format!("memory/team/{}.md", member.key));
                if let Some(body) = body_of(&notes) {
                    let _ = writeln!(
                        out,
                        "#### {}'s notes\n\n{}\n",
                        name(&member.id),
                        down(&body, 4)
                    );
                }
            }
        }
    }

    out.push_str("## The data they saved during the run\n\n");
    let mut saved_any = false;
    for top in &tops {
        let runs = folder::agent_dir(root, team, top).join("runs");
        let mut earlier = Vec::new();
        for file in files(&runs) {
            let path = runs.join(&file);
            if !during(&path) {
                earlier.push(format!("`{}`", file.display()));
                continue;
            }
            saved_any = true;
            let _ = writeln!(out, "### {}: {}\n", name(top), file.display());
            match fs::read_to_string(&path) {
                Ok(text) if text.len() <= MOST_FILE => {
                    let text = text.trim();
                    if path.extension().is_some_and(|e| e == "md") {
                        let _ = writeln!(out, "{}\n", down(text, 3));
                    } else {
                        let lang = path
                            .extension()
                            .map_or(String::new(), |e| e.to_string_lossy().into_owned());
                        let _ = writeln!(out, "```{lang}\n{text}\n```\n");
                    }
                }
                Ok(text) => {
                    let _ = writeln!(
                        out,
                        "{} KB, too big to put here: it's in `{}`.\n",
                        text.len() / 1024,
                        path.display()
                    );
                }
                Err(_) => {
                    let _ = writeln!(out, "Not text: it's in `{}`.\n", path.display());
                }
            }
        }
        if !earlier.is_empty() {
            let _ = writeln!(
                out,
                "From earlier runs, in {}'s `runs/`: {}.\n",
                name(top),
                earlier.join(", ")
            );
        }
    }
    if !saved_any {
        out.push_str("Nothing was saved during this run.\n\n");
    }

    out.push_str("## Their bugs and ideas for Persephone\n\n");
    let mut noted = false;
    for top in &tops {
        let agent_dir = folder::agent_dir(root, team, top);
        for (title, file) in [("Bugs", "app/bugs.md"), ("Ideas", "app/ideas.md")] {
            if let Some(body) = body_of(&agent_dir.join(file)) {
                noted = true;
                let _ = writeln!(out, "### {}: {title}\n\n{}\n", name(top), down(&body, 2));
            }
        }
    }
    if !noted {
        out.push_str("None.\n\n");
    }

    let _ = writeln!(
        out,
        "---\n\nEverything here is also in `{}`: the run's record and each session's whole stream, and each agent's own folder beside it.",
        folder::team_dir(root, team).display()
    );
    Ok(out)
}

/// The agents at the top of the run's sessions, as its plan has them, else as the team does.
fn tops(team: &Team, plan: Option<&Plan>) -> Vec<String> {
    match plan {
        Some(plan) => plan.sessions.iter().map(|s| s.root.clone()).collect(),
        None => team
            .agents
            .iter()
            .filter(|a| team.is_root(&a.id))
            .map(|a| a.id.clone())
            .collect(),
    }
}

/// A file's text without its title and the kit's line about what goes in it; none when nothing's
/// left.
fn body_of(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.first().is_some_and(|l| l.starts_with("# ")) {
        lines.remove(0);
    }
    let body: Vec<&str> = lines
        .into_iter()
        .skip_while(|l| l.trim().is_empty())
        .collect();
    // The templates' explanation of the file, before anything the agent wrote.
    let start = body
        .iter()
        .position(|l| l.trim().is_empty())
        .filter(|&blank| {
            let lead = body[..blank].join(" ");
            lead.starts_with("Each")
                || lead.starts_with("One line each")
                || lead.starts_with("Every setting")
                || lead.starts_with("What the user")
                || lead.starts_with("Things the user")
        })
        .map_or(0, |blank| blank + 1);
    let body = body[start..].join("\n").trim().to_owned();
    (!body.is_empty() && body != "(Nothing yet.)").then_some(body)
}

/// Markdown with its headings pushed `levels` further down, to sit under this document's.
fn down(text: &str, levels: usize) -> String {
    let mut fenced = false;
    text.lines()
        .map(|line| {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
            }
            if !fenced && line.starts_with('#') {
                format!("{}{line}", "#".repeat(levels))
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The files in a folder, by name, in order.
fn files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| PathBuf::from(e.file_name()))
        .filter(|name| !name.to_string_lossy().starts_with('.'))
        .collect();
    found.sort();
    found
}

/// When a file last changed, in Unix seconds.
fn modified(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs())
}

/// What an exported findings file is called: `crypto-desk-2026-11-07-1020-findings.md`.
#[must_use]
pub fn file_name(team: &Team, run: &str) -> String {
    format!("{}-{run}-findings.md", key(&team.name))
}
