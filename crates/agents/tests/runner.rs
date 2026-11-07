//! The runner against a stand-in for `claude`: a script that writes a report, prints a stream as
//! Claude Code does, and behaves as the `behave` file in its folder says.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, atomic::Ordering, mpsc},
    thread,
    time::{Duration, Instant},
};

use persephone_agents::{
    folder::{Setup, WRAP_UP, agent_dir},
    run::{Control, RunEvent, Status, run},
    team::{Link, LinkKind, Starter, Team},
};

const FAKE: &str = r#"#!/bin/sh
prompt=$(cat)
mode=$(cat behave 2>/dev/null)
mkdir -p outbox
printf '%s\n%s\n' "report from $(basename "$PWD")" "$prompt" > outbox/report.md
echo "$@" > last-args.txt
sid="00000000-0000-4000-8000-$(printf '%012d' $$)"
echo '{"type":"system","subtype":"init","session_id":"'$sid'","model":"claude-haiku-5-5","mcp_servers":[]}'
case "$mode" in
  slow)
    sleep 30 &
    echo $! > child.pid
    wait
    ;;
  nap)
    sleep 1
    ;;
  calls)
    i=0
    while [ $i -lt 25 ]; do
      echo '{"type":"assistant","parent_tool_use_id":null,"message":{"content":[{"type":"tool_use","id":"t'$i'","name":"Read","input":{"file_path":"x"}}]}}'
      i=$((i+1))
    done
    ;;
esac
if [ "$mode" = fail ]; then
  echo '{"type":"result","is_error":true,"num_turns":1,"total_cost_usd":0.01,"result":"it broke","session_id":"'$sid'"}'
  exit 1
fi
echo '{"type":"result","is_error":false,"num_turns":2,"total_cost_usd":0.25,"result":"done","session_id":"'$sid'"}'
"#;

struct Rig {
    root: PathBuf,
    setup: Setup,
}

impl Rig {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("persephone-runner-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let claude = root.join("fake-claude");
        fs::write(&claude, FAKE).unwrap();
        Command::new("chmod")
            .arg("+x")
            .arg(&claude)
            .status()
            .unwrap();
        let setup = Setup {
            root: root.join("agents"),
            claude,
            lab: None,
            keep_out: vec![],
        };
        Self { root, setup }
    }

    fn behave(&self, team: &Team, agent: usize, mode: &str) {
        let dir = agent_dir(&self.setup.root, team, &team.agents[agent].id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("behave"), mode).unwrap();
    }

    fn dir(&self, team: &Team, agent: usize) -> PathBuf {
        agent_dir(&self.setup.root, team, &team.agents[agent].id)
    }

    fn run(&self, team: &Team) -> Vec<RunEvent> {
        let mut events = Vec::new();
        run(team, &self.setup, &Control::default(), &mut |e| {
            events.push(e)
        })
        .unwrap();
        events
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn team(starter: Starter) -> Team {
    let mut team = Team::new("Desk", starter);
    team.brief = "Study BTC.".into();
    team
}

fn statuses(events: &[RunEvent], step: usize) -> Vec<Status> {
    events
        .iter()
        .filter_map(|e| match e {
            RunEvent::Status { step: s, status } if *s == step => Some(status.clone()),
            _ => None,
        })
        .collect()
}

fn ended(events: &[RunEvent]) -> &persephone_agents::run::Summary {
    events
        .iter()
        .find_map(|e| match e {
            RunEvent::Ended(summary) => Some(summary),
            _ => None,
        })
        .unwrap()
}

#[test]
fn a_hand_off_reaches_the_inbox_and_each_session_is_committed() {
    let rig = Rig::new("hand-off");
    let mut team = team(Starter::OneAnalyst);
    team.add_agent([200.0, 0.0]);
    team.agents[1].name = "Writer".into();
    team.agents[1].role = "Writes it up.".into();
    let (a, b) = (team.agents[0].id.clone(), team.agents[1].id.clone());
    team.links.push(Link {
        from: a.clone(),
        to: b,
        kind: LinkKind::HandsTo,
    });
    let events = rig.run(&team);
    let summary = ended(&events);
    assert!(summary.steps.iter().all(|s| s.ok), "{summary:?}");
    assert!((summary.spend - 0.5).abs() < 1e-9);
    let run = &summary.run;
    let handed = rig
        .dir(&team, 1)
        .join("inbox")
        .join(run)
        .join("analyst")
        .join("report.md");
    let text = fs::read_to_string(&handed).unwrap();
    assert!(text.starts_with(&format!("report from {a}")), "{text}");
    let prompt = fs::read_to_string(rig.dir(&team, 1).join("outbox/report.md")).unwrap();
    assert!(
        prompt.contains(&format!("`inbox/{run}/analyst/`")),
        "{prompt}"
    );
    let log = Command::new("git")
        .current_dir(rig.dir(&team, 0))
        .args(["log", "--format=%s"])
        .output()
        .unwrap();
    let log = String::from_utf8_lossy(&log.stdout);
    assert!(log.contains(&format!("Run {run}: Analyst: work")), "{log}");
    // The run's records: each step's stream and outbox.
    let runs = rig.setup.root.join(&team.id).join("runs").join(run);
    assert!(runs.join("summary.json").exists());
    assert!(runs.join("00-analyst.jsonl").exists());
    assert!(runs.join("steps/00-analyst/report.md").exists());
}

#[test]
fn no_more_run_at_once_than_allowed() {
    let rig = Rig::new("at-once");
    let mut team = team(Starter::OneAnalyst);
    for (n, x) in [(2, 200.0), (3, 400.0)] {
        team.add_agent([x, 0.0]);
        team.agents[n - 1].role = "Studies.".into();
    }
    team.at_once = 2;
    for agent in 0..3 {
        rig.behave(&team, agent, "nap");
    }
    let mut most = 0;
    let mut going = 0;
    let events = rig.run(&team);
    for event in &events {
        if let RunEvent::Status { status, .. } = event {
            match status {
                Status::Running => going += 1,
                s if s.finished() => going -= 1,
                _ => {}
            }
            most = most.max(going);
        }
    }
    assert_eq!(most, 2);
    assert!(ended(&events).steps.iter().all(|s| s.ok));
}

#[test]
fn a_session_past_its_budget_is_told_to_wrap_up() {
    let rig = Rig::new("budget");
    let mut team = team(Starter::OneAnalyst);
    team.agents[0].budget = 10;
    rig.behave(&team, 0, "calls");
    let events = rig.run(&team);
    assert_eq!(
        statuses(&events, 0),
        [Status::Running, Status::WrappingUp, Status::Done]
    );
    assert!(rig.dir(&team, 0).join(WRAP_UP).exists());
    assert_eq!(ended(&events).steps[0].calls, 25);
}

#[test]
fn a_failed_session_skips_what_waits_on_it() {
    let rig = Rig::new("failed");
    let mut team = team(Starter::ResearcherAndReviewer);
    rig.behave(&team, 0, "fail");
    team.at_once = 1;
    let events = rig.run(&team);
    assert_eq!(
        statuses(&events, 0),
        [Status::Running, Status::Failed("it broke".into())]
    );
    assert!(
        matches!(&statuses(&events, 1)[..], [Status::Skipped(why)] if why.contains("Researcher: work"))
    );
    assert!(matches!(&statuses(&events, 2)[..], [Status::Skipped(_)]));
}

#[test]
fn an_answer_carries_on_the_authors_conversation() {
    let rig = Rig::new("review");
    let team = team(Starter::ResearcherAndReviewer);
    let events = rig.run(&team);
    let summary = ended(&events);
    let labels: Vec<&str> = summary.steps.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Researcher: work",
            "Reviewer: review of Researcher, round 1",
            "Researcher: answer, round 1"
        ]
    );
    assert!(summary.steps.iter().all(|s| s.ok), "{summary:?}");
    let first = &summary.steps[0].session_id;
    let args = fs::read_to_string(rig.dir(&team, 0).join("last-args.txt")).unwrap();
    assert!(args.contains(&format!("--resume {first}")), "{args}");
    let review = rig
        .dir(&team, 0)
        .join("inbox")
        .join(&summary.run)
        .join("reviewer-round-1/report.md");
    assert!(
        fs::read_to_string(review)
            .unwrap()
            .contains("review Researcher's work")
    );
}

#[test]
fn stop_ends_the_session_and_what_it_started() {
    let rig = Rig::new("stop");
    let team = team(Starter::OneAnalyst);
    rig.behave(&team, 0, "slow");
    let control = Control::default();
    let (tx, rx) = mpsc::channel();
    let events = Arc::new(Mutex::new(Vec::new()));
    let started = Instant::now();
    let worker = {
        let (team, setup, control, events) = (
            team.clone(),
            rig.setup.clone(),
            control.clone(),
            Arc::clone(&events),
        );
        thread::spawn(move || {
            run(&team, &setup, &control, &mut |e| {
                if matches!(
                    e,
                    RunEvent::Status {
                        status: Status::Running,
                        ..
                    }
                ) {
                    let _ = tx.send(());
                }
                events.lock().unwrap().push(e);
            })
            .unwrap();
        })
    };
    rx.recv_timeout(Duration::from_secs(10)).unwrap();
    let child = rig.dir(&team, 0).join("child.pid");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !child.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    control.stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "it didn't wait out the sleep"
    );
    let events = events.lock().unwrap();
    assert!(matches!(
        statuses(&events, 0).last(),
        Some(Status::Stopped(why)) if why == "you stopped it"
    ));
    let pid = fs::read_to_string(child).unwrap();
    assert!(
        !alive(pid.trim()),
        "the session's child process was stopped too"
    );
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .status()
        .is_ok_and(|s| s.success())
}

#[test]
fn a_design_with_problems_doesnt_start() {
    let rig = Rig::new("problems");
    let mut team = team(Starter::OneAnalyst);
    team.brief.clear();
    let error = run(&team, &rig.setup, &Control::default(), &mut |_| {}).unwrap_err();
    assert!(error.contains("brief"), "{error}");
    assert!(!Path::new(&rig.setup.root).join(&team.id).exists());
}

#[test]
fn what_a_run_found_reads_as_one_document() {
    let rig = Rig::new("findings");
    let team = team(Starter::ResearcherAndReviewer);
    let dir = rig.dir(&team, 0);
    fs::create_dir_all(dir.join("memory")).unwrap();
    let events = rig.run(&team);
    let run = ended(&events).run.clone();
    // What the researcher wrote while it ran, and a belief it holds.
    fs::write(dir.join("runs/windows.json"), "{\"windows\": 7}").unwrap();
    fs::write(
        dir.join("memory/beliefs.md"),
        "# Beliefs\n\nOne line each: claim.\nA claim without run ids stays open.\n\n- News adds nothing | supported | run-1\n",
    )
    .unwrap();
    let document = persephone_agents::findings::findings(&rig.setup.root, &team, &run).unwrap();
    for part in [
        "# Desk: what run",
        "## The brief\n\nStudy BTC.",
        "| Researcher: work | done | 0 | $0.25 |",
        "### Researcher: work\n\nreport from",
        "### Reviewer: review of Researcher, round 1",
        "### Researcher: answer, round 1",
        "#### Beliefs\n\n- News adds nothing | supported | run-1",
        "### Researcher: windows.json\n\n```json\n{\"windows\": 7}\n```",
    ] {
        assert!(document.contains(part), "missing {part:?} in:\n{document}");
    }
    assert!(
        !document.contains("One line each"),
        "the template's own lines are left out"
    );
    assert_eq!(
        persephone_agents::findings::file_name(&team, &run),
        format!("desk-{run}-findings.md")
    );
}
