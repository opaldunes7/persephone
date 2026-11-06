//! `persephone-agents`: Persephone's agent teams, for another program or AI to build, run and
//! watch.
//!
//! ```text
//! persephone-agents tools [--markdown]              What there is
//! persephone-agents call NAME [ARGS-AS-JSON | @FILE] Call one tool
//! persephone-agents mcp                             Serve the tools over MCP on stdio
//! persephone-agents serve [--port N]                Serve them over local HTTP (default 8766)
//! persephone-agents run TEAM [--run NAME]           Run a team here, saying what it does
//! ```
//!
//! `--root DIR` before any of them keeps teams somewhere other than `~/persephone-agents`, and
//! `--claude PATH` says where Claude Code is.

mod http;
mod mcp;

use std::{path::PathBuf, process::ExitCode};

use persephone_agents::{
    control::{Desk, reference, tools},
    folder::{self, Setup},
    run::{self, Control, RunEvent},
    stream::Event,
};
use serde_json::{Value, json};

const USAGE: &str = "usage: persephone-agents [--root DIR] [--claude PATH] tools [--markdown] | call NAME [ARGS-AS-JSON | @FILE] | mcp | serve [--port N] | run TEAM [--run NAME]";

fn run_cli() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let (mut root, mut claude) = (None, None);
    while let Some(option) = args
        .first()
        .filter(|a| *a == "--root" || *a == "--claude")
        .cloned()
    {
        if args.len() < 2 {
            return Err(USAGE.into());
        }
        let value = PathBuf::from(args.remove(1));
        args.remove(0);
        if option == "--root" {
            root = Some(value);
        } else {
            claude = Some(value);
        }
    }
    let desk = Desk {
        root: root.unwrap_or_else(folder::default_root),
        claude,
        runner: std::env::current_exe().ok(),
    };
    match args.first().map(String::as_str) {
        Some("tools") => {
            if args.get(1).is_some_and(|a| a == "--markdown") {
                print!("{}", reference());
            } else {
                for tool in tools() {
                    println!("{:<16} {}", tool.name, tool.what);
                }
            }
            Ok(())
        }
        Some("call") => {
            let name = args.get(1).ok_or(USAGE)?;
            let call: Value = match args.get(2) {
                Some(text) => {
                    let text = match text.strip_prefix('@') {
                        Some(path) => std::fs::read_to_string(path)
                            .map_err(|e| format!("couldn't read {path}: {e}"))?,
                        None => text.clone(),
                    };
                    serde_json::from_str(&text)
                        .map_err(|e| format!("the arguments aren't JSON: {e}"))?
                }
                None => json!({}),
            };
            let answer = desk.dispatch(name, &call)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&answer).unwrap_or_default()
            );
            Ok(())
        }
        Some("mcp") => mcp::serve(desk),
        Some("serve") => {
            let port = match args.get(1).map(String::as_str) {
                Some("--port") => args
                    .get(2)
                    .and_then(|port| port.parse().ok())
                    .ok_or("--port needs a number")?,
                Some(_) => return Err(USAGE.into()),
                None => 8766,
            };
            http::serve(&desk, port, |port| {
                eprintln!(
                    "Listening on http://127.0.0.1:{port}. The token is in {}",
                    http::token_path(&desk).display()
                );
            })
        }
        Some("run") => {
            let team = args.get(1).ok_or(USAGE)?;
            let named = match args.get(2).map(String::as_str) {
                Some("--run") => Some(args.get(3).ok_or("--run needs a name")?.clone()),
                Some(_) => return Err(USAGE.into()),
                None => None,
            };
            run_team(&desk, team, named.as_deref())
        }
        _ => Err(USAGE.into()),
    }
}

/// Runs a team in this process, saying what it does, until it ends.
fn run_team(desk: &Desk, wanted: &str, named: Option<&str>) -> Result<(), String> {
    let teams = folder::load_teams(&desk.root);
    let team = teams
        .iter()
        .find(|t| t.id == wanted || t.name.eq_ignore_ascii_case(wanted))
        .ok_or_else(|| {
            format!(
                "there's no team called `{wanted}` in {}",
                desk.root.display()
            )
        })?;
    let found = run::look_around(desk.claude.as_deref(), team.lab.program.as_deref());
    let claude = found
        .claude
        .ok_or("Claude Code isn't where this can find it: pass --claude PATH")?;
    let setup = Setup {
        root: desk.root.clone(),
        claude,
        lab: team.lab.program.clone().or(found.lab),
        keep_out: run::code_folder().into_iter().collect(),
    };
    let summary = run::run_named(
        team,
        &setup,
        &Control::default(),
        named,
        &mut |event| match event {
            RunEvent::Planned { run, .. } => println!("Run {run} of {}", team.name),
            RunEvent::Status { step, status } => println!("step {}: {}", step + 1, status.label()),
            RunEvent::Event { step, event } => match event {
                Event::Called { tool, about, .. } => println!("  {} → {tool} {about}", step + 1),
                Event::Delegated { to, task, .. } => println!("  {} hands {to}: {task}", step + 1),
                Event::Refused { tool, why, .. } => {
                    println!("  {} refused {tool}: {why}", step + 1);
                }
                _ => {}
            },
            RunEvent::Note(note) => println!("note: {note}"),
            RunEvent::Ended(summary) => println!("Ended, ${:.2}", summary.spend),
        },
    )?;
    if summary.steps.iter().all(|s| s.ok) {
        Ok(())
    } else {
        Err("not every step finished".into())
    }
}

fn main() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("persephone-agents: {error}");
            ExitCode::FAILURE
        }
    }
}
