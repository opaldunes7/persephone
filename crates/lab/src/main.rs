//! `persephone-lab`: everything in Persephone, for agents and scripts.
//!
//! ```text
//! persephone-lab tools [--markdown]        What there is
//! persephone-lab call NAME [ARGS-AS-JSON]  Call one tool
//! persephone-lab mcp                       Serve agents over MCP on stdio
//! persephone-lab serve [--port N]          Serve over local HTTP (default port 8765)
//! ```
//!
//! `--data DIR` before any of them uses another folder than the app's.

use std::process::ExitCode;

use persephone_lab::{Lab, Paths, http, mcp, tools};
use serde_json::{Value, json};

const USAGE: &str = "usage: persephone-lab [--data DIR] tools [--markdown] | call NAME [ARGS-AS-JSON] | mcp | serve [--port N]";
/// The command line waits as long as a job takes.
const WAIT_FOR_EVER: f64 = 86_400.0;

fn run() -> Result<(), String> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut data = None;
    if args.first().is_some_and(|arg| arg == "--data") {
        if args.len() < 2 {
            return Err(USAGE.into());
        }
        data = Some(args.remove(1));
        args.remove(0);
    }
    let command = args.first().map(String::as_str);
    if command == Some("tools") {
        if args.get(1).is_some_and(|arg| arg == "--markdown") {
            print!("{}", tools::reference());
        } else {
            for tool in tools::tools() {
                println!("{:<22} {}", tool.name, tool.what);
            }
        }
        return Ok(());
    }
    let paths = match data {
        Some(data) => Paths::under(data),
        None => Paths::default_paths()?,
    };
    let lab = Lab::new(paths)?;
    match command {
        Some("call") => {
            let name = args.get(1).ok_or(USAGE)?;
            let mut call: Value = match args.get(2) {
                Some(text) => serde_json::from_str(text)
                    .map_err(|e| format!("the arguments aren't JSON: {e}"))?,
                None => json!({}),
            };
            let job = tools::tools()
                .iter()
                .any(|tool| tool.name == name && tool.job);
            if job && let Some(object) = call.as_object_mut() {
                object
                    .entry("wait_seconds")
                    .or_insert_with(|| json!(WAIT_FOR_EVER));
            }
            let answer = lab.dispatch(name, &call)?;
            println!("{answer}");
            Ok(())
        }
        Some("mcp") => mcp::serve(lab),
        Some("serve") => {
            let port = match args.get(1).map(String::as_str) {
                Some("--port") => args
                    .get(2)
                    .and_then(|port| port.parse().ok())
                    .ok_or("--port needs a number")?,
                Some(_) => return Err(USAGE.into()),
                None => 8765,
            };
            http::serve(&lab, port, |port| {
                eprintln!(
                    "Listening on http://127.0.0.1:{port}. The token is in {}",
                    lab.paths.lab().join("http-token").display()
                );
            })
        }
        _ => Err(USAGE.into()),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("persephone-lab: {error}");
            ExitCode::FAILURE
        }
    }
}
