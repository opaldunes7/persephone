//! Running a team: each step a `claude -p` in its agent's folder, in its own process group, as
//! many at once as the team allows and the plan's order lets, each read as it goes.

use std::{
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    os::unix::process::CommandExt as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    check::{Found, check},
    folder::{self, Setup, WRAP_UP, copy_into, refresh, stamp, team_dir},
    plan::{Plan, plan},
    prompts::{session_budget, start_prompt},
    stream::{Event, Finish, Reader},
    team::{Team, home, key, session_uuid},
};

/// What the app can ask of a run while it goes.
#[derive(Clone, Debug, Default)]
pub struct Control {
    /// Stop every session now.
    pub stop: Arc<AtomicBool>,
    /// Have every session do its end-of-session steps, and start no more.
    pub wrap_up: Arc<AtomicBool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Waiting,
    Running,
    /// Told its budget is used; doing its end-of-session steps.
    WrappingUp,
    Done,
    Failed(String),
    Stopped(String),
    Skipped(String),
}

impl Status {
    #[must_use]
    pub fn finished(&self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed(_) | Self::Stopped(_) | Self::Skipped(_)
        )
    }

    #[must_use]
    pub fn going(&self) -> bool {
        matches!(self, Self::Running | Self::WrappingUp)
    }

    /// In a word or two, with why for an end that isn't done.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Waiting => "waiting".into(),
            Self::Running => "running".into(),
            Self::WrappingUp => "wrapping up".into(),
            Self::Done => "done".into(),
            Self::Failed(why) => format!("failed: {why}"),
            Self::Stopped(why) => format!("stopped: {why}"),
            Self::Skipped(why) => format!("skipped: {why}"),
        }
    }
}

/// What a run says as it goes.
#[derive(Clone, Debug, PartialEq)]
pub enum RunEvent {
    /// The run is ready to start: its name, and what it will do.
    Planned {
        run: String,
        plan: Plan,
    },
    Status {
        step: usize,
        status: Status,
    },
    Event {
        step: usize,
        event: Event,
    },
    /// Something worth saying that isn't any one agent's.
    Note(String),
    Ended(Summary),
}

/// A run's record, kept as `runs/<run>/summary.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub run: String,
    pub team: String,
    pub steps: Vec<StepSummary>,
    pub spend: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepSummary {
    pub label: String,
    /// The agent at the top of its session, by id.
    pub agent: String,
    pub status: String,
    pub ok: bool,
    pub calls: u32,
    pub turns: u32,
    pub spend: f64,
    pub seconds: u64,
    /// Its whole stream, beside the summary.
    pub log: String,
    #[serde(default)]
    pub session_id: String,
}

/// A run as it goes, kept as `runs/<run>/state.json` for whoever's watching: the app, or another
/// program through the control tools.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    pub run: String,
    /// The team, by id.
    pub team: String,
    /// The process running it, to tell a run that's going from one whose runner died; 0 while
    /// it's being started.
    pub pid: u32,
    /// When it started and ended, in Unix seconds.
    pub started: u64,
    #[serde(default)]
    pub ended: Option<u64>,
    pub steps: Vec<StepState>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StepState {
    pub label: String,
    /// The agent at the top of its session, by id.
    pub agent: String,
    pub status: Status,
    pub calls: u32,
    pub spend: f64,
    /// Its latest tool call or hand-out: who, and what.
    pub doing: String,
}

impl RunState {
    /// Whether it's still going: not ended, and its runner still there, or being started.
    #[must_use]
    pub fn going(&self) -> bool {
        let starting = self.pid == 0 && unix_now() < self.started.saturating_add(STARTING_SECS);
        self.ended.is_none() && (starting || alive(self.pid))
    }

    #[must_use]
    pub fn spend(&self) -> f64 {
        self.steps.iter().fold(0.0, |spent, s| spent + s.spend)
    }
}

/// Whether a process is there.
#[must_use]
pub fn alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only asks whether the process exists.
    pid > 0 && unsafe { libc::kill(pid, 0) } == 0
}

/// How long a run being started counts as going, before its runner has said anything.
const STARTING_SECS: u64 = 60;

/// Files in a run's folder that ask it to stop, or to wrap up, from outside the process running
/// it.
pub const STOP_FILE: &str = "stop";
pub const WRAP_UP_FILE: &str = "wrap-up";

/// A run's folder.
#[must_use]
pub fn run_dir(root: &Path, team: &Team, run: &str) -> PathBuf {
    team_dir(root, team).join("runs").join(run)
}

/// What a run says of itself as it goes, if it's said anything.
#[must_use]
pub fn read_state(run_dir: &Path) -> Option<RunState> {
    serde_json::from_str(&fs::read_to_string(run_dir.join("state.json")).ok()?).ok()
}

/// What a run set out to do.
#[must_use]
pub fn read_plan(run_dir: &Path) -> Option<Plan> {
    serde_json::from_str(&fs::read_to_string(run_dir.join("plan.json")).ok()?).ok()
}

/// The team's run that's going, if one is.
#[must_use]
pub fn going_run(root: &Path, team: &Team) -> Option<RunState> {
    let mut runs: Vec<RunState> = fs::read_dir(team_dir(root, team).join("runs"))
        .ok()?
        .flatten()
        .filter_map(|entry| read_state(&entry.path()))
        .filter(RunState::going)
        .collect();
    runs.sort_by(|a, b| b.run.cmp(&a.run));
    runs.into_iter().next()
}

/// Asks a run to stop, from any process.
///
/// # Errors
///
/// Says why the request couldn't be left.
pub fn ask_stop(run_dir: &Path) -> Result<(), String> {
    fs::write(run_dir.join(STOP_FILE), "").map_err(|e| e.to_string())
}

/// Asks a run to wrap up, from any process.
///
/// # Errors
///
/// Says why the request couldn't be left.
pub fn ask_wrap_up(run_dir: &Path) -> Result<(), String> {
    fs::write(run_dir.join(WRAP_UP_FILE), "").map_err(|e| e.to_string())
}

/// Keeps `state.json` up to date: at once when a step's status changes, and at most every second
/// for the counts in between.
struct Records {
    dir: PathBuf,
    state: RunState,
    written: Option<Instant>,
    statuses: Vec<Status>,
}

impl Records {
    fn write(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.state).unwrap_or_default();
        folder::write_aside(&self.dir.join("state.json"), text.as_bytes())
    }

    fn save(&mut self, states: &[State], doing: &[String], force: bool) {
        let statuses: Vec<Status> = states.iter().map(|s| s.status.clone()).collect();
        let due = self
            .written
            .is_none_or(|at| at.elapsed() >= Duration::from_secs(1));
        if !force && statuses == self.statuses && !due {
            return;
        }
        for (step, (state, saved)) in states.iter().zip(&mut self.state.steps).enumerate() {
            saved.status = state.status.clone();
            saved.calls = state.calls;
            saved.spend = state.finish.as_ref().map_or(0.0, |f| f.spend);
            saved.doing.clone_from(&doing[step]);
        }
        let _ = self.write();
        self.written = Some(Instant::now());
        self.statuses = statuses;
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A step's tool calls past its budget before it's stopped, should it not wrap up.
fn margin(budget: u32) -> u32 {
    (budget / 4).max(20)
}

/// How long a session has to stop once asked, before it's killed.
const GRACE: Duration = Duration::from_secs(5);

struct State {
    status: Status,
    calls: u32,
    finish: Option<Finish>,
    session_id: Option<String>,
    started: Option<Instant>,
    seconds: u64,
    /// Its process group, while it runs.
    group: Option<i32>,
    /// When it was asked to stop, and why.
    stopping: Option<(Instant, String)>,
}

enum Message {
    Event(usize, Event),
    Exit(usize, Option<i32>, String),
}

/// Runs `team`, saying everything to `send` as it goes, and returns its record.
///
/// # Errors
///
/// Says why it couldn't start: a problem in the design, or a folder that couldn't be made.
pub fn run(
    team: &Team,
    setup: &Setup,
    control: &Control,
    send: &mut dyn FnMut(RunEvent),
) -> Result<Summary, String> {
    run_named(team, setup, control, None, send)
}

/// [`run`], under the name [`reserve_run`] gave it, if it's given one.
///
/// # Errors
///
/// Says why it couldn't start: a problem in the design, or a folder that couldn't be made.
pub fn run_named(
    team: &Team,
    setup: &Setup,
    control: &Control,
    named: Option<&str>,
    send: &mut dyn FnMut(RunEvent),
) -> Result<Summary, String> {
    if let Some(problem) = check(team).into_iter().next() {
        return Err(problem.what);
    }
    let plan = plan(team).map_err(|_| "Its steps wait on each other in a circle.".to_owned())?;
    let team_dir = team_dir(&setup.root, team);
    let (run, run_dir) = match named {
        Some(run) => {
            let dir = team_dir.join("runs").join(run);
            fs::create_dir_all(dir.join("steps"))
                .map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
            (run.to_owned(), dir)
        }
        None => reserve_run(&team_dir)?,
    };
    let text = serde_json::to_string_pretty(&plan).unwrap_or_default();
    folder::write_aside(&run_dir.join("plan.json"), text.as_bytes())?;
    let mut doing: Vec<String> = vec![String::new(); plan.steps.len()];
    let mut records = Records {
        dir: run_dir.clone(),
        state: RunState {
            run: run.clone(),
            team: team.id.clone(),
            pid: std::process::id(),
            started: unix_now(),
            ended: None,
            steps: (0..plan.steps.len())
                .map(|step| StepState {
                    label: plan.label(team, step),
                    agent: plan.agent_of(step).to_owned(),
                    status: Status::Waiting,
                    calls: 0,
                    spend: 0.0,
                    doing: String::new(),
                })
                .collect(),
            notes: Vec::new(),
        },
        written: None,
        statuses: Vec::new(),
    };
    // Going from now: no other run of this team starts while its folders are made.
    records.write()?;
    // Every folder now, so each one's sandbox knows today's fellow agents.
    let dirs = plan
        .sessions
        .iter()
        .map(|session| refresh(setup, team, session))
        .collect::<Result<Vec<PathBuf>, String>>()?;
    send(RunEvent::Planned {
        run: run.clone(),
        plan: plan.clone(),
    });

    let mut states: Vec<State> = plan
        .steps
        .iter()
        .map(|_| State {
            status: Status::Waiting,
            calls: 0,
            finish: None,
            session_id: None,
            started: None,
            seconds: 0,
            group: None,
            stopping: None,
        })
        .collect();
    let budgets: Vec<u32> = plan
        .steps
        .iter()
        .map(|step| session_budget(team, &plan.sessions[step.session]))
        .collect();
    let at_once = team.at_once.max(1) as usize;
    let (tx, rx) = mpsc::channel();
    let mut stopped = false;
    let mut wrapped = false;
    // Claude Code's usage limit, once a session hit it: the rest would only fail the same way.
    let mut limited: Option<String> = None;
    let set =
        |states: &mut Vec<State>, step: usize, status: Status, send: &mut dyn FnMut(RunEvent)| {
            states[step].status = status.clone();
            send(RunEvent::Status { step, status });
        };

    loop {
        records.save(&states, &doing, false);
        // Asked from outside this process: by the app watching it, or the control tools.
        if run_dir.join(STOP_FILE).exists() {
            control.stop.store(true, Ordering::Relaxed);
        }
        if run_dir.join(WRAP_UP_FILE).exists() {
            control.wrap_up.store(true, Ordering::Relaxed);
        }
        if !stopped && control.stop.load(Ordering::Relaxed) {
            stopped = true;
            for step in 0..states.len() {
                if states[step].status.going() {
                    ask_to_stop(&mut states[step], "you stopped it");
                } else if states[step].status == Status::Waiting {
                    set(
                        &mut states,
                        step,
                        Status::Stopped("before it started".into()),
                        send,
                    );
                }
            }
        }
        if !wrapped && !stopped && control.wrap_up.load(Ordering::Relaxed) {
            wrapped = true;
            for step in 0..states.len() {
                if states[step].status == Status::Running {
                    flag_wrap_up(&dirs[plan.steps[step].session]);
                    set(&mut states, step, Status::WrappingUp, send);
                } else if states[step].status == Status::Waiting {
                    set(
                        &mut states,
                        step,
                        Status::Skipped("the run was wrapped up before it started".into()),
                        send,
                    );
                }
            }
        }
        // What can't run now, as what it waits on didn't finish.
        for step in 0..states.len() {
            if states[step].status != Status::Waiting {
                continue;
            }
            if let Some(why) = &limited {
                set(&mut states, step, Status::Skipped(why.clone()), send);
                continue;
            }
            if let Some(&failed) = plan.steps[step].after.iter().find(|&&before| {
                states[before].status.finished() && states[before].status != Status::Done
            }) {
                let why = format!("{} didn't finish", plan.label(team, failed));
                set(&mut states, step, Status::Skipped(why), send);
            }
        }
        // What can start.
        let mut going = states.iter().filter(|s| s.status.going()).count();
        for step in 0..states.len() {
            if going >= at_once {
                break;
            }
            let ready = states[step].status == Status::Waiting
                && plan.steps[step]
                    .after
                    .iter()
                    .all(|&before| states[before].status == Status::Done);
            if !ready {
                continue;
            }
            let dir = &dirs[plan.steps[step].session];
            match start(team, setup, &plan, step, &run, &run_dir, dir, &states, &tx) {
                Ok(group) => {
                    states[step].group = Some(group);
                    states[step].started = Some(Instant::now());
                    set(&mut states, step, Status::Running, send);
                    going += 1;
                }
                Err(why) => set(&mut states, step, Status::Failed(why), send),
            }
        }
        // Those that were asked to stop and haven't.
        for state in &mut states {
            if let (Some(group), Some((asked, _))) = (state.group, &state.stopping)
                && asked.elapsed() > GRACE
            {
                signal(group, libc::SIGKILL);
            }
        }
        if !states.iter().any(|s| s.status.going()) {
            if states.iter().all(|s| s.status.finished()) {
                break;
            }
            // Nothing runs and nothing can start: only a design that check passed can't be
            // stuck, but don't wait for ever on one that is.
            let stuck = states.iter().any(|s| s.status == Status::Waiting)
                && !(0..states.len()).any(|step| {
                    states[step].status == Status::Waiting
                        && plan.steps[step]
                            .after
                            .iter()
                            .all(|&b| states[b].status == Status::Done)
                });
            if stuck {
                for step in 0..states.len() {
                    if states[step].status == Status::Waiting {
                        set(
                            &mut states,
                            step,
                            Status::Skipped("it could never start".into()),
                            send,
                        );
                    }
                }
                continue;
            }
        }
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Message::Event(step, event)) => {
                let state = &mut states[step];
                match &event {
                    Event::Started { session_id, .. } => {
                        state.session_id = Some(session_id.clone())
                    }
                    Event::Finished(finish) => state.finish = Some(finish.clone()),
                    _ => {}
                }
                match &event {
                    Event::Called {
                        who, tool, about, ..
                    } => {
                        doing[step] =
                            format!("{}: {tool} {about}", who_name(team, &plan, step, who))
                                .trim()
                                .to_owned();
                    }
                    Event::Delegated { who, to, task } => {
                        doing[step] =
                            format!("{} hands {to}: {task}", who_name(team, &plan, step, who));
                    }
                    _ => {}
                }
                if event.is_call() {
                    state.calls += 1;
                    let budget = budgets[step];
                    if state.calls >= budget && state.status == Status::Running {
                        flag_wrap_up(&dirs[plan.steps[step].session]);
                        set(&mut states, step, Status::WrappingUp, send);
                    } else if state.calls >= budget + margin(budget) && state.stopping.is_none() {
                        ask_to_stop(state, "it kept going past its budget");
                    }
                }
                send(RunEvent::Event { step, event });
            }
            Ok(Message::Exit(step, code, said)) => {
                let state = &mut states[step];
                state.group = None;
                state.seconds = state.started.map_or(0, |t| t.elapsed().as_secs());
                let status = match (&state.stopping, &state.finish) {
                    (Some((_, why)), _) => Status::Stopped(why.clone()),
                    (None, Some(finish)) if finish.ok => Status::Done,
                    (None, Some(finish)) => Status::Failed(first_line(&finish.text, &said, code)),
                    (None, None) => Status::Failed(first_line("", &said, code)),
                };
                if let Status::Failed(why) = &status
                    && limit_hit(why)
                {
                    limited = Some(format!("Claude Code's usage limit: {why}"));
                }
                let session = &plan.sessions[plan.steps[step].session];
                let dir = &dirs[plan.steps[step].session];
                if let Err(why) = copy_into(
                    &dir.join("outbox"),
                    &run_dir.join("steps").join(step_name(&plan, step)),
                ) {
                    send(RunEvent::Note(why));
                }
                let label = plan.label(team, step);
                if let Err(why) = folder::commit(
                    dir,
                    team.name_of(&session.root),
                    &format!("Run {run}: {label}"),
                ) {
                    send(RunEvent::Note(format!("{label}: {why}")));
                }
                set(&mut states, step, status, send);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    let summary = Summary {
        run: run.clone(),
        team: team.name.clone(),
        spend: states
            .iter()
            .filter_map(|s| s.finish.as_ref())
            .fold(0.0, |spent, f| spent + f.spend),
        steps: states
            .iter()
            .enumerate()
            .map(|(step, state)| StepSummary {
                label: plan.label(team, step),
                agent: plan.agent_of(step).to_owned(),
                status: state.status.label(),
                ok: state.status == Status::Done,
                calls: state.calls,
                turns: state.finish.as_ref().map_or(0, |f| f.turns),
                spend: state.finish.as_ref().map_or(0.0, |f| f.spend),
                seconds: state.seconds,
                log: format!("{}.jsonl", step_name(&plan, step)),
                session_id: state.session_id.clone().unwrap_or_default(),
            })
            .collect(),
    };
    let text = serde_json::to_string_pretty(&summary).unwrap_or_default();
    if let Err(why) = folder::write_aside(&run_dir.join("summary.json"), text.as_bytes()) {
        send(RunEvent::Note(why));
    }
    records.state.ended = Some(unix_now());
    records.save(&states, &doing, true);
    send(RunEvent::Ended(summary.clone()));
    Ok(summary)
}

/// Who did something, by name: the agent at the top of its session, or a member's key.
fn who_name(team: &Team, plan: &Plan, step: usize, who: &crate::stream::Who) -> String {
    match who {
        crate::stream::Who::Lead => team.name_of(plan.agent_of(step)).to_owned(),
        crate::stream::Who::Member(key) => key.clone(),
    }
}

/// A name for a new run, from the time, and its folder, made now so no other run can have it.
///
/// # Errors
///
/// Says why its folder couldn't be made.
pub fn reserve_run(team_dir: &Path) -> Result<(String, PathBuf), String> {
    let runs = team_dir.join("runs");
    fs::create_dir_all(&runs).map_err(|e| format!("couldn't make {}: {e}", runs.display()))?;
    let base = stamp();
    let mut run = base.clone();
    let mut n = 2;
    loop {
        let dir = runs.join(&run);
        match fs::create_dir(&dir) {
            Ok(()) => {
                fs::create_dir_all(dir.join("steps"))
                    .map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
                return Ok((run, dir));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                run = format!("{base}-{n}");
                n += 1;
            }
            Err(e) => return Err(format!("couldn't make {}: {e}", dir.display())),
        }
    }
}

/// What a step's records are called: `02-reviewer`.
#[must_use]
pub fn step_name(plan: &Plan, step: usize) -> String {
    format!("{step:02}-{}", plan.sessions[plan.steps[step].session].key)
}

#[allow(clippy::too_many_arguments)]
fn start(
    team: &Team,
    setup: &Setup,
    plan: &Plan,
    step: usize,
    run: &str,
    run_dir: &Path,
    dir: &Path,
    states: &[State],
    tx: &mpsc::Sender<Message>,
) -> Result<i32, String> {
    let _ = fs::remove_file(dir.join(WRAP_UP));
    for input in &plan.steps[step].inputs {
        copy_into(
            &run_dir.join("steps").join(step_name(plan, input.step)),
            &dir.join("inbox").join(run).join(&input.folder),
        )?;
    }
    let resume = plan.steps[step]
        .resumes
        .and_then(|earlier| states[earlier].session_id.clone());
    let mut command = command(setup, team, plan, step, run, resume.as_deref());
    command.current_dir(dir);
    let mut child = command
        .spawn()
        .map_err(|e| format!("couldn't start {}: {e}", setup.claude.display()))?;
    let group = i32::try_from(child.id()).map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let prompt = start_prompt(team, plan, step, run);
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let log = run_dir.join(format!("{}.jsonl", step_name(plan, step)));
    let tx = tx.clone();
    thread::spawn(move || {
        let stderr = child.stderr.take().map(|mut err| {
            thread::spawn(move || {
                let mut text = String::new();
                let _ = err.read_to_string(&mut text);
                text
            })
        });
        let mut log = fs::File::create(log).ok();
        let mut reader = Reader::default();
        if let Some(out) = child.stdout.take() {
            for line in BufReader::new(out).lines() {
                let Ok(line) = line else { break };
                if let Some(log) = &mut log {
                    let _ = writeln!(log, "{line}");
                }
                for event in reader.read(&line) {
                    let _ = tx.send(Message::Event(step, event));
                }
            }
        }
        let code = child.wait().ok().and_then(|status| status.code());
        let said = stderr.and_then(|t| t.join().ok()).unwrap_or_default();
        let _ = tx.send(Message::Exit(step, code, said));
    });
    Ok(group)
}

/// The `claude` that runs `step`, carrying on `resume` if it's given; its folder isn't set.
#[must_use]
pub fn command(
    setup: &Setup,
    team: &Team,
    plan: &Plan,
    step: usize,
    run: &str,
    resume: Option<&str>,
) -> Command {
    let session = &plan.sessions[plan.steps[step].session];
    let mut command = Command::new(&setup.claude);
    command
        .arg("-p")
        .args(["--verbose", "--output-format", "stream-json"]);
    if let Some(agent) = team.agent(&session.root) {
        command
            .args(["--model", agent.model.cli()])
            .args(["--effort", agent.effort.cli()])
            .args(["--max-budget-usd", &format!("{:.2}", agent.spend_cap)]);
    }
    command.args(["--settings", ".claude/settings.json"]).args([
        "--mcp-config",
        ".mcp.json",
        "--strict-mcp-config",
    ]);
    let mut tools = String::from("Read,Edit,Write,Glob,Grep");
    let mut allowed: Vec<String> = ["Read", "Edit(./**)", "Glob", "Grep"]
        .map(String::from)
        .into();
    if session.lab {
        allowed.push("mcp__persephone-lab".into());
    }
    if !session.members.is_empty() {
        command.args(["--agents", ".claude/members.json"]);
        tools.push_str(",Agent");
        let reports: Vec<String> = team
            .reports_of(&session.root)
            .iter()
            .map(|r| key(&r.name))
            .collect();
        allowed.extend(crate::folder::call_rules(&reports));
    }
    command
        .args(["--tools", &tools])
        .arg("--allowedTools")
        .args(&allowed)
        .arg("--disallowedTools")
        .args(["Bash", "WebFetch", "WebSearch"]);
    match resume {
        Some(id) => command.args(["--resume", id]),
        None => command.args(["--session-id", &session_uuid()]),
    };
    command.args([
        "--name",
        &format!("{}: {}", team.name, plan.label(team, step)),
    ]);
    clean_env(&mut command);
    command
        .env(
            "CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH",
            session.depth.to_string(),
        )
        // A member started in the background answers its lead with a launch notice, not its
        // report: in the foreground the lead waits for what it found, and its card ends when it
        // does. Claude Code's own Explore and Plan agents aren't any team's.
        .env("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1")
        .env("CLAUDE_CODE_DISABLE_EXPLORE_PLAN_AGENTS", "1")
        .env(
            "PERSEPHONE_SESSION",
            format!("Run {run}: {}", plan.label(team, step)),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
}

/// When the app was itself started from inside a Claude Code session, leaves out what that
/// session set, so each agent's `claude` starts as it would from a terminal, with your own
/// login. What you set yourself to choose a provider or a token is kept.
fn clean_env(command: &mut Command) {
    if std::env::var_os("CLAUDECODE").is_none() {
        return;
    }
    for (name, _) in std::env::vars_os() {
        if let Some(name) = name.to_str()
            && parent_session_var(name)
        {
            command.env_remove(name);
        }
    }
}

fn parent_session_var(name: &str) -> bool {
    const KEEP: [&str; 6] = [
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_SKIP_BEDROCK_AUTH",
        "CLAUDE_CODE_SKIP_VERTEX_AUTH",
    ];
    if KEEP.contains(&name) {
        return false;
    }
    matches!(
        name,
        "CLAUDECODE"
            | "CLAUDE_PID"
            | "CLAUDE_EFFORT"
            | "CLAUDE_PREVIEW_CLASSIFIER_FLOOR"
            | "MCP_CONNECTION_NONBLOCKING"
            | "MCP_SERVER_CONNECTION_BATCH_SIZE"
            | "USE_LOCAL_OAUTH"
            | "USE_STAGING_OAUTH"
            | "ANTHROPIC_BASE_URL"
    ) || name.starts_with("CLAUDE_CODE_")
        || name.starts_with("CLAUDE_AGENT_SDK_")
}

fn flag_wrap_up(dir: &Path) {
    let _ = fs::write(dir.join(WRAP_UP), "");
}

fn ask_to_stop(state: &mut State, why: &str) {
    if let Some(group) = state.group {
        signal(group, libc::SIGTERM);
        state.stopping = Some((Instant::now(), why.to_owned()));
    }
}

fn signal(group: i32, signal: i32) {
    // SAFETY: only sends a signal; the group is one this run started.
    unsafe {
        libc::killpg(group, signal);
    }
}

/// Whether a session ended on Claude Code's usage limit, as in "You've hit your session limit".
fn limit_hit(said: &str) -> bool {
    let said = said.to_lowercase();
    (said.contains("hit your") && said.contains("limit")) || said.contains("usage limit")
}

/// Why a session failed, in a line.
fn first_line(said: &str, stderr: &str, code: Option<i32>) -> String {
    [said, stderr]
        .iter()
        .flat_map(|text| text.lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(
            || match code {
                Some(code) => format!("it ended with code {code}"),
                None => "it was killed".to_owned(),
            },
            |line| line.chars().take(200).collect(),
        )
}

/// A team's past runs, newest first.
#[must_use]
pub fn past_runs(root: &Path, team: &Team) -> Vec<Summary> {
    let Ok(entries) = fs::read_dir(team_dir(root, team).join("runs")) else {
        return Vec::new();
    };
    let mut runs: Vec<Summary> = entries
        .flatten()
        .filter_map(|entry| fs::read_to_string(entry.path().join("summary.json")).ok())
        .filter_map(|text| serde_json::from_str(&text).ok())
        .collect();
    runs.sort_by(|a: &Summary, b| b.run.cmp(&a.run));
    runs
}

/// What a past step's log says, read again.
#[must_use]
pub fn read_log(root: &Path, team: &Team, run: &str, log: &str) -> Vec<Event> {
    let path = team_dir(root, team).join("runs").join(run).join(log);
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut reader = Reader::default();
    text.lines().flat_map(|line| reader.read(line)).collect()
}

/// Claude Code's program: `configured` if it's there, else the first `claude` on the path or
/// where its installer puts it.
#[must_use]
pub fn find_claude(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return path.is_file().then(|| path.to_owned());
    }
    let on_path = std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|p| p.join("claude"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    on_path
        .into_iter()
        .chain([
            home().join(".local/bin/claude"),
            home().join(".claude/local/claude"),
            PathBuf::from("/opt/homebrew/bin/claude"),
            PathBuf::from("/usr/local/bin/claude"),
        ])
        .find(|path| path.is_file())
}

/// Lab's program: beside this one, as cargo builds them, or the workspace's release build.
#[must_use]
pub fn find_lab() -> Option<PathBuf> {
    beside("persephone-lab")
}

/// `persephone-agents`, which runs a team in a process of its own: found as Lab is.
#[must_use]
pub fn find_runner() -> Option<PathBuf> {
    beside("persephone-agents")
}

/// One of Persephone's programs: beside this one, as cargo builds them, or the workspace's
/// release build.
fn beside(name: &str) -> Option<PathBuf> {
    let here = std::env::current_exe().ok()?;
    let dir = here.parent()?;
    [
        dir.join(name),
        dir.join("../release").join(name),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/release")
            .join(name),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .and_then(|path| fs::canonicalize(path).ok())
}

/// Persephone's own code, which no agent may read: the workspace this was built from.
#[must_use]
pub fn code_folder() -> Option<PathBuf> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    workspace
        .join("Cargo.toml")
        .is_file()
        .then(|| fs::canonicalize(&workspace).ok())
        .flatten()
}

/// Whether Claude Code is logged in, from `claude auth status`; none if it couldn't say.
#[must_use]
pub fn logged_in(claude: &Path) -> Option<bool> {
    let mut command = Command::new(claude);
    command.args(["auth", "status"]).stdin(Stdio::null());
    clean_env(&mut command);
    let out = command.output().ok()?;
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    status["loggedIn"].as_bool()
}

/// What a run needs from this computer, looked for.
#[must_use]
pub fn look_around(claude: Option<&Path>, lab: Option<&Path>) -> Found {
    let claude = find_claude(claude);
    let logged_in = claude.as_deref().and_then(logged_in);
    let lab = match lab {
        Some(path) => path.is_file().then(|| path.to_owned()),
        None => find_lab(),
    };
    Found {
        claude,
        logged_in,
        lab,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::Starter;

    #[test]
    fn only_a_parent_sessions_variables_are_left_out() {
        assert!(parent_session_var("CLAUDECODE"));
        assert!(parent_session_var("CLAUDE_CODE_ENTRYPOINT"));
        assert!(parent_session_var("CLAUDE_AGENT_SDK_VERSION"));
        assert!(parent_session_var("ANTHROPIC_BASE_URL"));
        assert!(!parent_session_var("CLAUDE_CODE_USE_BEDROCK"));
        assert!(!parent_session_var("ANTHROPIC_API_KEY"));
        assert!(!parent_session_var("PATH"));
    }

    #[test]
    fn a_lead_may_call_only_its_team() {
        let mut team = Team::new("Desk", Starter::LeadAndAnalysts);
        team.brief = "Study.".into();
        let plan = plan(&team).unwrap();
        let setup = Setup {
            root: "/tmp/agents".into(),
            claude: "claude".into(),
            lab: None,
            keep_out: vec![],
        };
        let command = command(&setup, &team, &plan, 0, "r1", None);
        let args: Vec<String> = command
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let after = |flag: &str| {
            let at = args.iter().position(|a| a == flag).unwrap();
            args[at + 1].clone()
        };
        assert_eq!(after("--model"), "opus");
        assert_eq!(after("--effort"), "high");
        assert_eq!(after("--tools"), "Read,Edit,Write,Glob,Grep,Agent");
        assert_eq!(after("--agents"), ".claude/members.json");
        assert!(
            args.contains(&"Agent(analyst-a)".to_owned())
                && args.contains(&"Agent(analyst-b)".to_owned()),
            "{args:?}"
        );
        assert!(args.contains(&"--session-id".to_owned()));
        let depth = command
            .get_envs()
            .find(|(k, _)| *k == "CLAUDE_CODE_MAX_SUBAGENT_SPAWN_DEPTH")
            .and_then(|(_, v)| v)
            .unwrap();
        assert_eq!(depth, "1");
        assert!(
            command
                .get_envs()
                .any(|(k, v)| k == "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS" && v.is_some()),
            "members answer in the foreground"
        );
        let resumed = super::command(&setup, &team, &plan, 0, "r1", Some("abc"));
        assert!(resumed.get_args().any(|a| a == "abc"));
    }

    #[test]
    fn why_it_failed_is_one_line() {
        assert_eq!(first_line("", "\n  oops\nmore", Some(1)), "oops");
        assert_eq!(
            first_line("Failed to authenticate", "", None),
            "Failed to authenticate"
        );
        assert_eq!(first_line("", "", Some(2)), "it ended with code 2");
    }
}
