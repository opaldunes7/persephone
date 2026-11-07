//! `persephone-agents` as another program or AI uses it: through its command line, with a
//! stand-in for `claude` that writes a report and prints a stream as Claude Code does.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const FAKE: &str = r#"#!/bin/sh
# Asked whether it's logged in, it can't tell: and writes nothing where it's asked from.
[ "$1" = "auth" ] && exit 0
prompt=$(cat)
mkdir -p outbox
printf '%s\n%s\n' "report from $(basename "$PWD")" "$prompt" > outbox/report.md
sid="00000000-0000-4000-8000-$(printf '%012d' $$)"
echo '{"type":"system","subtype":"init","session_id":"'$sid'","model":"claude-haiku-5-5","mcp_servers":[]}'
echo '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"subject.md"}}]}}'
sleep 1
echo '{"type":"result","is_error":false,"num_turns":2,"total_cost_usd":0.25,"result":"done","session_id":"'$sid'"}'
"#;

struct Rig {
    root: PathBuf,
    claude: PathBuf,
}

impl Rig {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "persephone-agents-server-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let claude = base.join("fake-claude");
        fs::write(&claude, FAKE).unwrap();
        Command::new("chmod")
            .arg("+x")
            .arg(&claude)
            .status()
            .unwrap();
        Self {
            root: base.join("teams"),
            claude,
        }
    }

    fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        let out = Command::new(env!("CARGO_BIN_EXE_persephone-agents"))
            .arg("--root")
            .arg(&self.root)
            .arg("--claude")
            .arg(&self.claude)
            .args(["call", tool, &args.to_string()])
            .output()
            .unwrap();
        if out.status.success() {
            Ok(serde_json::from_slice(&out.stdout).unwrap())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).into_owned())
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(base) = self.root.parent() {
            let _ = fs::remove_dir_all(base);
        }
    }
}

fn wait_for_end(rig: &Rig, team: &str, run: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = rig
            .call("get_run", &json!({"team": team, "run": run}))
            .unwrap();
        if state["status"] == "ended" {
            return state;
        }
        assert!(Instant::now() < deadline, "it didn't end: {state}");
        thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn another_ai_builds_runs_and_reads_a_team() {
    let rig = Rig::new("whole");
    let team = rig
        .call(
            "create_team",
            &json!({
                "name": "Remote desk",
                "brief": "Study BTC.",
                "agents": [{"name": "Solo", "role": "Studies.", "model": "haiku", "uses_lab": false}]
            }),
        )
        .unwrap();
    assert_eq!(team["problems"], json!([]), "{team}");
    let started = rig
        .call("run_team", &json!({"team": "Remote desk"}))
        .unwrap();
    let run = started["run"].as_str().unwrap().to_owned();
    // A second run can't start while this one goes.
    let again = rig
        .call("run_team", &json!({"team": "Remote desk"}))
        .unwrap_err();
    assert!(again.contains("is going"), "{again}");
    let state = wait_for_end(&rig, "Remote desk", &run);
    assert_eq!(state["steps"][0]["status"], "done", "{state}");
    assert_eq!(state["spend"], 0.25);
    let recent: Vec<&str> = state["recent"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        recent.iter().any(|l| l.starts_with("Solo → Read")),
        "{recent:?}"
    );
    let outputs = rig
        .call(
            "get_outputs",
            &json!({"team": "Remote desk", "agent": "Solo"}),
        )
        .unwrap();
    assert!(
        outputs["report"]
            .as_str()
            .unwrap()
            .starts_with("report from"),
        "{outputs}"
    );
    let runs = rig
        .call("list_runs", &json!({"team": "Remote desk"}))
        .unwrap();
    assert_eq!(runs["runs"][0]["run"], run.as_str());
}

#[test]
fn a_run_is_stopped_from_outside() {
    let rig = Rig::new("stop");
    rig.call(
        "create_team",
        &json!({
            "name": "Slow",
            "brief": "Study.",
            "agents": [
                {"name": "First", "role": "Studies.", "uses_lab": false},
                {"name": "Second", "role": "Writes.", "uses_lab": false}
            ],
            "links": [{"from": "First", "to": "Second", "kind": "hands_to"}]
        }),
    )
    .unwrap();
    let run = rig.call("run_team", &json!({"team": "Slow"})).unwrap()["run"]
        .as_str()
        .unwrap()
        .to_owned();
    // Once it's going, stopped through its folder, as the app's Stop button does too.
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig
        .call("get_run", &json!({"team": "Slow", "run": run}))
        .unwrap()["status"]
        != "running"
    {
        assert!(Instant::now() < deadline, "it never started");
        thread::sleep(Duration::from_millis(100));
    }
    rig.call("stop_run", &json!({"team": "Slow"})).unwrap();
    let state = wait_for_end(&rig, "Slow", &run);
    let second = state["steps"][1]["status"].as_str().unwrap();
    assert!(
        second.starts_with("stopped") || second.starts_with("skipped"),
        "{state}"
    );
    assert!(Path::new(&rig.root).exists());
}

#[test]
fn the_written_reference_matches_the_tools() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/agents/tools.md");
    let written = fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        written,
        persephone_agents::control::reference(),
        "run: persephone-agents tools --markdown > docs/agents/tools.md"
    );
}

#[test]
fn a_setup_file_goes_out_and_comes_back_in() {
    let rig = Rig::new("setup");
    let program = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_persephone-agents"))
            .arg("--root")
            .arg(&rig.root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/agents/examples/crypto-research-desk.json");
    let made = program(&["import", example.to_str().unwrap()]);
    assert!(
        made.starts_with("Made Crypto research desk (4 agents)"),
        "{made}"
    );
    assert!(!made.contains("Before it can run"), "{made}");
    let file = rig.root.join("desk.json");
    program(&["export", "Crypto research desk", file.to_str().unwrap()]);
    let made = program(&["import", file.to_str().unwrap(), "--name", "Second desk"]);
    assert!(made.starts_with("Made Second desk (4 agents)"), "{made}");
    let first = rig
        .call("export_team", &json!({"team": "Crypto research desk"}))
        .unwrap();
    let mut second = rig
        .call("export_team", &json!({"team": "Second desk"}))
        .unwrap();
    second["setup"]["name"] = first["setup"]["name"].clone();
    assert_eq!(first["setup"], second["setup"], "the same setup, exactly");
}
