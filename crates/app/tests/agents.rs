//! The Agents view, driven through the messages the UI sends. Teams are kept in a folder of the
//! test's own; nothing here starts Claude Code: a run's events are handed in as the runner
//! would send them.

use std::{path::PathBuf, sync::Arc};

use iced::Size;
use persephone::{
    Message, Persephone,
    agents::{AgentsMessage, Selected},
    backtest::{PanelMessage, TestKind},
    keymap::Action,
};
use persephone_agents::{
    check::Found,
    plan::plan,
    run::{RunEvent, Status, Summary},
    stream::{Event, Finish, Who},
    team::{LinkKind, Model, Starter},
};
use persephone_chart::ChartData;
use persephone_core::{Candle, Timeframe};

fn app() -> Persephone {
    let candles = (0..500u64)
        .map(|i| Candle {
            ts: i * Timeframe::H1.nanos(),
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.0,
            volume: 1.0,
        })
        .collect();
    Persephone::preloaded(
        ChartData::new("BTCUSDT", "BINANCE", Timeframe::H1, 2, candles),
        Size::new(1600.0, 1000.0),
    )
}

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("persephone-agents-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn found() -> Found {
    Found {
        claude: Some("/usr/local/bin/claude".into()),
        logged_in: Some(true),
        lab: Some("/usr/local/bin/persephone-lab".into()),
    }
}

fn say(app: &mut Persephone, message: AgentsMessage) {
    let _ = app.update(Message::Agents(message));
}

fn shows(app: &Persephone, words: &str) -> bool {
    let mut ui = iced_test::Simulator::with_size(
        iced::Settings::default(),
        Size::new(1600.0, 1000.0),
        app.view(),
    );
    ui.find(words).is_ok()
}

#[test]
fn ctrl_g_opens_it_over_the_charts_and_a_test_tab_closes_it() {
    let mut app = app();
    assert!(shows(&app, "Agents"), "its button in the top bar");
    let _ = app.update(Message::Key(Action::Agents));
    assert!(app.agents().saved.open);
    assert!(shows(&app, "Teams"));
    let _ = app.update(Message::Top(PanelMessage::Test(TestKind::Backtest)));
    assert!(!app.agents().saved.open);
}

#[test]
fn it_is_saved_with_the_layout() {
    let dir = dir("layout");
    let path = dir.join("layout.ron");
    let (mut first, _) = Persephone::start(&[], Some(path.clone()));
    first.use_agents_root(dir.join("teams"), found());
    let _ = first.update(Message::Agents(AgentsMessage::Toggle));
    let _ = first.update(Message::Agents(AgentsMessage::New(Starter::OneAnalyst)));
    let (restored, _) = Persephone::start(&[], Some(path));
    let saved = &restored.agents().saved;
    assert!(saved.open);
    assert_eq!(saved, &first.agents().saved, "the team and the folder too");
    assert_eq!(restored.agents().teams.len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_team_drawn_is_kept_in_its_folder() {
    let dir = dir("drawn");
    let mut app = app();
    app.use_agents_root(dir.clone(), found());
    say(&mut app, AgentsMessage::New(Starter::Blank));
    say(&mut app, AgentsMessage::Add([0.0, 0.0]));
    say(&mut app, AgentsMessage::Add([300.0, 0.0]));
    let ids: Vec<String> = app
        .agents()
        .team()
        .unwrap()
        .agents
        .iter()
        .map(|a| a.id.clone())
        .collect();
    say(&mut app, AgentsMessage::LinkAs(LinkKind::HandsTo));
    say(
        &mut app,
        AgentsMessage::Connect(ids[0].clone(), ids[1].clone()),
    );
    say(
        &mut app,
        AgentsMessage::Select(Selected::Agent(ids[1].clone())),
    );
    say(&mut app, AgentsMessage::Model(Model::Haiku));
    say(&mut app, AgentsMessage::Name("Writer".into()));
    say(
        &mut app,
        AgentsMessage::Moved {
            id: ids[1].clone(),
            at: [320.0, 40.0],
            done: true,
        },
    );

    let mut again = Persephone::preloaded(
        ChartData::new("BTCUSDT", "BINANCE", Timeframe::H1, 2, Vec::new()),
        Size::new(1600.0, 1000.0),
    );
    again.use_agents_root(dir.clone(), found());
    let team = again.agents().team().unwrap();
    let writer = team.agent(&ids[1]).unwrap();
    assert_eq!(writer.name, "Writer");
    assert_eq!(writer.model, Model::Haiku);
    assert_eq!(writer.at, [320.0, 40.0]);
    assert_eq!(team.links.len(), 1);
    assert_eq!(team.links[0].kind, LinkKind::HandsTo);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_second_lead_replaces_the_first() {
    let dir = dir("lead");
    let mut app = app();
    app.use_agents_root(dir.clone(), found());
    say(&mut app, AgentsMessage::New(Starter::LeadAndAnalysts));
    let team = app.agents().team().unwrap().clone();
    let (lead, a, b) = (&team.agents[0].id, &team.agents[1].id, &team.agents[2].id);
    say(&mut app, AgentsMessage::LinkAs(LinkKind::ReportsTo));
    say(&mut app, AgentsMessage::Connect(a.clone(), b.clone()));
    let team = app.agents().team().unwrap();
    assert_eq!(team.lead_of(a), Some(b.as_str()));
    assert_eq!(team.reports_of(lead).len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn check_lists_what_to_fix_and_a_click_selects_its_card() {
    let dir = dir("check");
    let mut app = app();
    app.use_agents_root(dir.clone(), found());
    let _ = app.update(Message::Agents(AgentsMessage::Toggle));
    say(&mut app, AgentsMessage::New(Starter::OneAnalyst));
    say(&mut app, AgentsMessage::Add([300.0, 0.0]));
    say(&mut app, AgentsMessage::Check);
    let problems = &app.agents().problems;
    assert!(
        problems.iter().any(|p| p.what.contains("brief is empty")),
        "{problems:?}"
    );
    let role = problems
        .iter()
        .find(|p| p.what.contains("describe its role"))
        .unwrap();
    let id = role.agents[0].clone();
    assert!(shows(&app, &format!("• {}", role.what)));
    say(&mut app, AgentsMessage::Select(Selected::Agent(id.clone())));
    assert_eq!(app.agents().selected, Selected::Agent(id));
    // Run won't start while there's something to fix.
    say(&mut app, AgentsMessage::Run);
    assert!(app.agents().live.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_runs_events_light_the_cards() {
    let dir = dir("live");
    let mut app = app();
    app.use_agents_root(dir.clone(), found());
    let _ = app.update(Message::Agents(AgentsMessage::Toggle));
    say(&mut app, AgentsMessage::New(Starter::LeadAndAnalysts));
    say(
        &mut app,
        AgentsMessage::Brief(iced::widget::text_editor::Action::Edit(
            iced::widget::text_editor::Edit::Paste(Arc::new("Study BTC.".into())),
        )),
    );
    assert_eq!(app.agents().team().unwrap().brief, "Study BTC.");
    // Inside the app, where this test can feed it events; the task it returns is never run.
    app.run_agents_in_app();
    say(&mut app, AgentsMessage::Run);
    assert!(app.agents().running());

    let team = app.agents().team().unwrap().clone();
    let (lead, analyst) = (team.agents[0].id.clone(), team.agents[1].id.clone());
    let plan = plan(&team).unwrap();
    let mut send = |event| say(&mut app, AgentsMessage::Running(event));
    send(RunEvent::Planned {
        run: "r1".into(),
        plan,
    });
    send(RunEvent::Status {
        step: 0,
        status: Status::Running,
    });
    send(RunEvent::Event {
        step: 0,
        event: Event::Delegated {
            who: Who::Lead,
            to: "analyst-a".into(),
            task: "Test EMA".into(),
        },
    });
    send(RunEvent::Event {
        step: 0,
        event: Event::Called {
            who: Who::Member("analyst-a".into()),
            tool: "run_backtest".into(),
            about: "BTCUSDT 1h".into(),
        },
    });
    let live = app.agents().live().unwrap();
    assert_eq!(live.cards[&lead].status, Some(Status::Running));
    assert_eq!(live.cards[&analyst].status, Some(Status::Running));
    assert_eq!(live.cards[&analyst].calls, 1);
    assert_eq!(live.cards[&analyst].doing, "run_backtest BTCUSDT 1h");
    assert_eq!(live.lab_calls, 1);
    assert!(shows(&app, "Analyst A → run_backtest BTCUSDT 1h"));

    let mut send = |event| say(&mut app, AgentsMessage::Running(event));
    send(RunEvent::Event {
        step: 0,
        event: Event::Returned {
            who: Who::Lead,
            from: "analyst-a".into(),
            text: "It lost to holding.".into(),
        },
    });
    send(RunEvent::Event {
        step: 0,
        event: Event::Finished(Finish {
            ok: true,
            turns: 9,
            spend: 1.25,
            ..Finish::default()
        }),
    });
    send(RunEvent::Status {
        step: 0,
        status: Status::Done,
    });
    send(RunEvent::Ended(Summary {
        run: "r1".into(),
        team: team.name.clone(),
        steps: Vec::new(),
        spend: 1.25,
    }));
    let live = app.agents().live().unwrap();
    assert_eq!(live.cards[&analyst].status, Some(Status::Done));
    assert_eq!(live.cards[&lead].spend, 1.25);
    assert!(!app.agents().running());
    std::fs::remove_dir_all(dir).unwrap();
}

fn desk(root: &std::path::Path) -> persephone_agents::control::Desk {
    persephone_agents::control::Desk {
        root: root.to_owned(),
        claude: None,
        runner: None,
    }
}

#[test]
fn a_team_changed_by_another_program_shows_at_once() {
    use serde_json::json;
    let dir = dir("outside");
    let mut app = app();
    app.use_agents_root(dir.clone(), found());
    let _ = app.update(Message::Agents(AgentsMessage::Toggle));
    let desk = desk(&dir);
    desk.dispatch(
        "create_team",
        &json!({"name": "From outside", "brief": "Study.", "agents": [{"name": "Solo", "role": "Studies."}]}),
    )
    .unwrap();
    let changed = AgentsMessage::Changed {
        teams: true,
        runs: false,
        files: false,
    };
    say(&mut app, changed.clone());
    let team = app
        .agents()
        .teams
        .iter()
        .find(|t| t.name == "From outside")
        .unwrap()
        .clone();
    say(&mut app, AgentsMessage::Pick(team.id.clone()));
    let solo = team.agents[0].id.clone();
    say(
        &mut app,
        AgentsMessage::Select(Selected::Agent(solo.clone())),
    );
    say(&mut app, AgentsMessage::Model(Model::Haiku));
    // The app's own save isn't taken for another's.
    say(&mut app, changed.clone());
    assert_eq!(app.agents().team().unwrap().agents[0].model, Model::Haiku);
    // Another program's change shows, editors and all, on top of the app's.
    desk.dispatch(
        "update_agent",
        &json!({"team": "From outside", "agent": "Solo", "role": "Studies harder."}),
    )
    .unwrap();
    say(&mut app, changed);
    let agent = &app.agents().team().unwrap().agents[0];
    assert_eq!(agent.role, "Studies harder.");
    assert_eq!(agent.model, Model::Haiku);
    assert_eq!(app.agents().role.text().trim_end(), "Studies harder.");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_run_started_elsewhere_lights_the_cards_and_can_be_stopped() {
    use persephone_agents::{
        folder,
        run::{self, RunState, StepState},
    };
    use serde_json::json;
    let dir = dir("elsewhere");
    let mut app = app();
    app.use_agents_root(dir.clone(), found());
    let _ = app.update(Message::Agents(AgentsMessage::Toggle));
    say(&mut app, AgentsMessage::New(Starter::LeadAndAnalysts));
    let team = app.agents().team().unwrap().clone();
    let plan = plan(&team).unwrap();
    let (name, run_dir) = run::reserve_run(&folder::team_dir(&dir, &team)).unwrap();
    // Its runner, in another process that's still there.
    let mut runner = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .unwrap();
    std::fs::write(
        run_dir.join("plan.json"),
        serde_json::to_string(&plan).unwrap(),
    )
    .unwrap();
    let state = RunState {
        run: name,
        team: team.id.clone(),
        pid: runner.id(),
        started: 0,
        ended: None,
        steps: vec![StepState {
            label: plan.label(&team, 0),
            agent: team.agents[0].id.clone(),
            status: Status::Running,
            calls: 2,
            spend: 0.0,
            doing: String::new(),
        }],
        notes: Vec::new(),
    };
    std::fs::write(
        run_dir.join("state.json"),
        serde_json::to_string(&state).unwrap(),
    )
    .unwrap();
    let lines = [
        json!({"type": "assistant", "parent_tool_use_id": null, "message": {"content": [
            {"type": "tool_use", "id": "t1", "name": "Agent", "input": {"subagent_type": "analyst-a", "description": "Test EMA"}}
        ]}}),
        json!({"type": "assistant", "parent_tool_use_id": "t1", "message": {"content": [
            {"type": "tool_use", "id": "t2", "name": "mcp__persephone-lab__run_backtest", "input": {"symbol": "BTCUSDT"}}
        ]}}),
    ];
    let log: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(run_dir.join("00-lead.jsonl"), log).unwrap();
    say(
        &mut app,
        AgentsMessage::Changed {
            teams: false,
            runs: true,
            files: false,
        },
    );
    let live = app.agents().live().expect("the run started elsewhere");
    assert!(live.external.is_some());
    let analyst = &team.agents[1].id;
    assert_eq!(live.cards[&team.agents[0].id].status, Some(Status::Running));
    assert_eq!(live.cards[analyst].status, Some(Status::Running));
    assert_eq!(live.cards[analyst].calls, 1);
    assert!(app.agents().running());
    assert!(shows(&app, "Stop"));
    // Closing the window leaves a run in a process of its own going.
    let _ = app.update(Message::CloseRequested(iced::window::Id::unique()));
    assert!(app.agents().closing.is_none());
    assert!(!run_dir.join("stop").exists());
    say(&mut app, AgentsMessage::Stop);
    assert!(
        run_dir.join("stop").exists(),
        "asked to stop through its folder"
    );
    let _ = runner.kill();
    let _ = runner.wait();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_setup_file_imports_as_a_team_and_exports_again() {
    let dir = dir("setup");
    let mut app = app();
    app.use_agents_root(dir.join("teams"), found());
    let _ = app.update(Message::Agents(AgentsMessage::Toggle));
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/agents/examples/crypto-research-desk.json");
    say(&mut app, AgentsMessage::SetupPicked(Some(example.clone())));
    let team = app.agents().team().unwrap().clone();
    assert_eq!(team.name, "Crypto research desk");
    assert_eq!(team.agents.len(), 4);
    assert_eq!(
        app.agents().note.as_deref(),
        Some("Imported Crypto research desk.")
    );
    // Again: the name's taken, so the next free one.
    say(&mut app, AgentsMessage::SetupPicked(Some(example)));
    assert_eq!(app.agents().team().unwrap().name, "Crypto research desk 2");
    let out = dir.join("out.json");
    say(&mut app, AgentsMessage::SetupSaveTo(Some(out.clone())));
    let written: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(
        written,
        persephone_agents::control::export(app.agents().team().unwrap())
    );
    // A broken file says why, and changes nothing.
    std::fs::write(
        dir.join("bad.json"),
        r#"{"name": "X", "agents": [{"nmae": "A"}]}"#,
    )
    .unwrap();
    let before = app.agents().teams.len();
    say(
        &mut app,
        AgentsMessage::SetupPicked(Some(dir.join("bad.json"))),
    );
    assert_eq!(app.agents().teams.len(), before);
    assert!(app.agents().note.as_deref().unwrap().contains("`nmae`"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn what_a_run_found_exports_as_one_file() {
    use persephone_agents::{
        folder,
        run::{self, RunState, StepState, Summary},
    };
    let dir = dir("findings");
    let mut app = app();
    app.use_agents_root(dir.join("teams"), found());
    let _ = app.update(Message::Agents(AgentsMessage::Toggle));
    say(&mut app, AgentsMessage::New(Starter::OneAnalyst));
    let team = app.agents().team().unwrap().clone();
    assert_eq!(
        app.agents().findings_run(),
        None,
        "nothing to export before a run"
    );
    // A run that's ended, as the runner leaves it.
    let plan = plan(&team).unwrap();
    let (name, run_dir) = run::reserve_run(&folder::team_dir(&dir.join("teams"), &team)).unwrap();
    std::fs::write(
        run_dir.join("plan.json"),
        serde_json::to_string(&plan).unwrap(),
    )
    .unwrap();
    let state = RunState {
        run: name.clone(),
        team: team.id.clone(),
        pid: 0,
        started: 0,
        ended: Some(1),
        steps: vec![StepState {
            label: plan.label(&team, 0),
            agent: team.agents[0].id.clone(),
            status: Status::Done,
            calls: 12,
            spend: 0.5,
            doing: String::new(),
        }],
        notes: Vec::new(),
    };
    std::fs::write(
        run_dir.join("state.json"),
        serde_json::to_string(&state).unwrap(),
    )
    .unwrap();
    let report = run_dir.join("steps").join(run::step_name(&plan, 0));
    std::fs::create_dir_all(&report).unwrap();
    std::fs::write(
        report.join("report.md"),
        "# Report\n\nNothing worth trading.\n",
    )
    .unwrap();
    let summary = Summary {
        run: name.clone(),
        team: team.name.clone(),
        steps: Vec::new(),
        spend: 0.5,
    };
    std::fs::write(
        run_dir.join("summary.json"),
        serde_json::to_string(&summary).unwrap(),
    )
    .unwrap();
    say(
        &mut app,
        AgentsMessage::Changed {
            teams: false,
            runs: true,
            files: false,
        },
    );
    assert_eq!(app.agents().findings_run().as_deref(), Some(name.as_str()));
    assert!(shows(&app, "Export findings…"));
    let out = dir.join("found.md");
    say(
        &mut app,
        AgentsMessage::FindingsSaveTo(name.clone(), Some(out.clone())),
    );
    let written = std::fs::read_to_string(&out).unwrap();
    assert!(written.contains("Nothing worth trading."), "{written}");
    assert!(
        written.contains("| Analyst: work | done | 12 | $0.50 |"),
        "{written}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
