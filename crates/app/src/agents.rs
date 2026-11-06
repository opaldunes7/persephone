//! The Agents view: teams of Claude agents drawn as cards and links, run as Claude Code sessions
//! and watched as they work. It opens from the top bar, Ctrl+G or the menu, over the charts,
//! which keep running underneath. What's drawn, checked and run is `persephone_agents`'.

pub mod board;
pub mod view;

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use iced::{
    Task,
    futures::{SinkExt, StreamExt, channel::mpsc},
    widget::text_editor,
    window,
};
use persephone_agents::{
    check::{Found, Problem, check, check_setup},
    folder::{self, Setup},
    plan::{Plan, plan, sessions},
    run::{self, Control, RunEvent, Status, Summary},
    stream::{Event, Who, is_lab},
    team::{Effort, LAB, LabData, Link, LinkKind, Model, Starter, Team, View, fresh_id, key},
};
use serde::{Deserialize, Serialize};

use crate::Message;

/// The view as it's saved with the layout.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentsSaved {
    pub open: bool,
    /// The team last on the canvas, by id.
    pub team: Option<String>,
    /// Where teams live; none for `~/persephone-agents`.
    pub root: Option<PathBuf>,
    /// Claude Code's program; none to look for it.
    pub claude: Option<PathBuf>,
}

/// What's selected on the canvas: the inspector shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Selected {
    /// Nothing: the team itself.
    #[default]
    Team,
    Agent(String),
    Link(usize),
    Lab,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Setup,
    Activity,
    Outputs,
    Feedback,
}

/// A line of a run's activity.
#[derive(Clone, Debug, PartialEq)]
pub enum Line {
    Event(Event),
    Status(Status),
}

/// One step of the run on screen.
#[derive(Clone, Debug)]
pub struct StepLive {
    pub status: Status,
    pub started: Option<Instant>,
    pub spend: Option<f64>,
}

/// What a card shows of the run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardLive {
    pub status: Option<Status>,
    pub calls: u32,
    /// What it's doing now, or last did.
    pub doing: String,
    pub spend: f64,
}

/// A run: going, or the last one this session.
pub struct Live {
    pub team: String,
    pub run: Option<String>,
    pub plan: Option<Plan>,
    pub steps: Vec<StepLive>,
    pub log: Vec<(usize, Line)>,
    pub cards: HashMap<String, CardLive>,
    pub notes: Vec<String>,
    pub control: Control,
    pub started: Instant,
    pub ended: Option<Summary>,
    pub failed: Option<String>,
    pub lab_calls: u32,
    /// Each session's members' ids by their keys.
    keys: Vec<HashMap<String, String>>,
}

impl Live {
    fn new(team: &str, control: Control) -> Self {
        Self {
            team: team.to_owned(),
            run: None,
            plan: None,
            steps: Vec::new(),
            log: Vec::new(),
            cards: HashMap::new(),
            notes: Vec::new(),
            control,
            started: Instant::now(),
            ended: None,
            failed: None,
            lab_calls: 0,
            keys: Vec::new(),
        }
    }

    #[must_use]
    pub fn going(&self) -> bool {
        self.ended.is_none() && self.failed.is_none()
    }

    /// What it's spent so far, as the sessions that ended said.
    #[must_use]
    pub fn spend(&self) -> f64 {
        // From +0: an empty sum of floats is -0, which would print as -0.00.
        self.steps
            .iter()
            .filter_map(|s| s.spend)
            .fold(0.0, |spent, s| spent + s)
    }

    /// The agent a step's line is about, by id.
    #[must_use]
    pub fn agent_of(&self, step: usize, who: &Who) -> Option<&str> {
        let plan = self.plan.as_ref()?;
        let session = plan.steps.get(step)?.session;
        match who {
            Who::Lead => Some(plan.sessions[session].root.as_str()),
            Who::Member(key) => self
                .keys
                .get(session)?
                .get(key)
                .map(String::as_str)
                .or(Some(plan.sessions[session].root.as_str())),
        }
    }

    fn apply(&mut self, event: RunEvent) {
        match event {
            RunEvent::Planned { run, plan } => {
                self.keys = plan
                    .sessions
                    .iter()
                    .map(|s| {
                        s.members
                            .iter()
                            .map(|m| (m.key.clone(), m.id.clone()))
                            .collect()
                    })
                    .collect();
                self.steps = plan
                    .steps
                    .iter()
                    .map(|_| StepLive {
                        status: Status::Waiting,
                        started: None,
                        spend: None,
                    })
                    .collect();
                for session in &plan.sessions {
                    self.cards.entry(session.root.clone()).or_default().status =
                        Some(Status::Waiting);
                }
                self.run = Some(run);
                self.plan = Some(plan);
            }
            RunEvent::Status { step, status } => {
                if let Some(live) = self.steps.get_mut(step) {
                    if status == Status::Running {
                        live.started = Some(Instant::now());
                    }
                    live.status = status.clone();
                }
                if let Some(root) = self.agent_of(step, &Who::Lead).map(str::to_owned) {
                    self.cards.entry(root).or_default().status = Some(status.clone());
                }
                self.log.push((step, Line::Status(status)));
            }
            RunEvent::Event { step, event } => {
                let id = self.agent_of(step, event.who()).map(str::to_owned);
                if let Some(id) = &id {
                    let card = self.cards.entry(id.clone()).or_default();
                    match &event {
                        Event::Called { tool, about, .. } => {
                            card.calls += 1;
                            card.doing = format!("{tool} {about}").trim().to_owned();
                            if is_lab(tool) {
                                self.lab_calls += 1;
                            }
                        }
                        Event::Delegated { to, task, .. } => {
                            card.calls += 1;
                            card.doing = format!("hands {to}: {task}");
                        }
                        _ => {}
                    }
                }
                match &event {
                    Event::Delegated { to, task, .. } => {
                        if let Some(member) = self.member(step, to) {
                            let card = self.cards.entry(member).or_default();
                            card.status = Some(Status::Running);
                            card.doing.clone_from(task);
                        }
                    }
                    Event::Returned { from, .. } => {
                        if let Some(member) = self.member(step, from) {
                            self.cards.entry(member).or_default().status = Some(Status::Done);
                        }
                    }
                    Event::Finished(finish) => {
                        if let Some(live) = self.steps.get_mut(step) {
                            live.spend = Some(finish.spend);
                        }
                        if let Some(id) = &id {
                            self.cards.entry(id.clone()).or_default().spend += finish.spend;
                        }
                    }
                    _ => {}
                }
                self.log.push((step, Line::Event(event)));
            }
            RunEvent::Note(note) => self.notes.push(note),
            RunEvent::Ended(summary) => self.ended = Some(summary),
        }
    }

    fn member(&self, step: usize, key: &str) -> Option<String> {
        let session = self.plan.as_ref()?.steps.get(step)?.session;
        self.keys.get(session)?.get(key).cloned()
    }
}

/// One agent's files, as the Outputs tab shows them.
pub struct Outputs {
    pub agent: String,
    pub folder: PathBuf,
    /// Each one's title and what's in it.
    pub files: Vec<(String, String)>,
    /// The user's own files there, to edit.
    pub vetoed: text_editor::Content,
    pub tasks: text_editor::Content,
    /// Whether this agent has a folder of its own, or is a member in its lead's.
    pub own: bool,
}

/// The Agents view's state, and the teams it has.
pub struct Agents {
    pub saved: AgentsSaved,
    pub teams: Vec<Team>,
    pub selected: Selected,
    pub link_as: LinkKind,
    pub tab: Tab,
    pub role: text_editor::Content,
    pub brief: text_editor::Content,
    pub principles: text_editor::Content,
    /// The team and agent the editors were filled from.
    filled: (Option<String>, Option<String>),
    /// What Check last found, the design's and this computer's.
    pub problems: Vec<Problem>,
    /// What this computer has, once looked for.
    pub found: Option<Found>,
    pub looking: bool,
    pub live: Option<Live>,
    /// The current team's past runs, newest first.
    pub past: Vec<Summary>,
    /// A past run, read again: its record and its lines.
    pub viewing: Option<(Summary, Vec<(usize, Line)>)>,
    pub outputs: Option<Outputs>,
    /// Every agent's bugs and ideas for the app: its name, and each file's text.
    pub feedback: Vec<(String, String, String)>,
    pub note: Option<String>,
    /// What's been typed into a number field that doesn't read as a number yet.
    pub typing: HashMap<&'static str, String>,
    /// The window to close once the run has stopped.
    pub closing: Option<window::Id>,
}

impl Default for Agents {
    fn default() -> Self {
        Self {
            saved: AgentsSaved::default(),
            teams: Vec::new(),
            selected: Selected::Team,
            link_as: LinkKind::ReportsTo,
            tab: Tab::Setup,
            role: text_editor::Content::new(),
            brief: text_editor::Content::new(),
            principles: text_editor::Content::new(),
            filled: (None, None),
            problems: Vec::new(),
            found: None,
            looking: false,
            live: None,
            past: Vec::new(),
            viewing: None,
            outputs: None,
            feedback: Vec::new(),
            note: None,
            typing: HashMap::new(),
            closing: None,
        }
    }
}

impl Agents {
    #[must_use]
    pub fn from_saved(saved: Option<AgentsSaved>) -> Self {
        let mut agents = Self {
            saved: saved.unwrap_or_default(),
            ..Self::default()
        };
        if agents.saved.open {
            agents.load();
        }
        agents
    }

    /// Where teams live.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.saved.root.clone().unwrap_or_else(folder::default_root)
    }

    /// The team on the canvas.
    #[must_use]
    pub fn team(&self) -> Option<&Team> {
        let id = self.saved.team.as_deref()?;
        self.teams.iter().find(|t| t.id == id)
    }

    fn team_mut(&mut self) -> Option<&mut Team> {
        let id = self.saved.team.clone()?;
        self.teams.iter_mut().find(|t| t.id == id)
    }

    /// Whether a run of any team is going.
    #[must_use]
    pub fn running(&self) -> bool {
        self.live.as_ref().is_some_and(Live::going)
    }

    /// The run on screen, if it's the current team's.
    #[must_use]
    pub fn live(&self) -> Option<&Live> {
        let team = self.saved.team.as_deref()?;
        self.live.as_ref().filter(|live| live.team == team)
    }

    /// Reads the teams from disk, keeping the current one if it's still there.
    pub fn load(&mut self) {
        self.teams = folder::load_teams(&self.root());
        let current = self.saved.team.clone();
        if !self.teams.iter().any(|t| Some(&t.id) == current.as_ref()) {
            self.saved.team = self.teams.first().map(|t| t.id.clone());
            self.selected = Selected::Team;
        }
        self.after_switch();
    }

    /// What follows a change of team: its past runs, and editors filled anew.
    fn after_switch(&mut self) {
        self.past = self
            .team()
            .map(|team| run::past_runs(&self.root(), team))
            .unwrap_or_default();
        self.viewing = None;
        self.problems.clear();
        self.load_feedback();
        self.load_outputs();
        self.fill_editors();
    }

    /// Fills the text editors from the team and agent they're for, if those changed.
    fn fill_editors(&mut self) {
        let agent = match &self.selected {
            Selected::Agent(id) => Some(id.clone()),
            _ => None,
        };
        let now = (self.saved.team.clone(), agent.clone());
        if self.filled == now {
            return;
        }
        let Some(team) = self.team() else {
            self.filled = now;
            return;
        };
        let role = agent
            .as_deref()
            .and_then(|id| team.agent(id))
            .map_or_else(String::new, |a| a.role.clone());
        let (brief, principles) = (team.brief.clone(), team.principles.clone());
        self.role = text_editor::Content::with_text(&role);
        if self.filled.0 != now.0 {
            self.brief = text_editor::Content::with_text(&brief);
            self.principles = text_editor::Content::with_text(&principles);
        }
        self.filled = now;
    }

    fn save_team(&mut self) {
        let root = self.root();
        if let Some(team) = self.team()
            && let Err(why) = folder::save_team(&root, team)
        {
            self.note = Some(why);
        }
    }

    /// The selected agent's files, or the first session's when it's the team.
    fn load_outputs(&mut self) {
        self.outputs = None;
        let root = self.root();
        let Some(team) = self.team() else { return };
        let agent = match &self.selected {
            Selected::Agent(id) => id.clone(),
            _ => match sessions(team).first() {
                Some(session) => session.root.clone(),
                None => return,
            },
        };
        let top = team.root_of(&agent).to_owned();
        let folder = folder::agent_dir(&root, team, &top);
        let read = |path: &str| fs::read_to_string(folder.join(path)).unwrap_or_default();
        let mut files = Vec::new();
        let own = top == agent;
        if own {
            files.push(("Report".to_owned(), read("outbox/report.md")));
            if let Some(journal) = newest(&folder.join("memory/journal")) {
                let title = format!(
                    "Journal, {}",
                    journal.file_stem().unwrap_or_default().to_string_lossy()
                );
                files.push((title, fs::read_to_string(&journal).unwrap_or_default()));
            }
            files.push(("Beliefs".to_owned(), read("memory/beliefs.md")));
            files.push(("Bugs".to_owned(), read("app/bugs.md")));
            files.push(("Ideas".to_owned(), read("app/ideas.md")));
        } else if let Some(member) = team.agent(&agent) {
            files.push((
                "Its notes".to_owned(),
                read(&format!("memory/team/{}.md", key(&member.name))),
            ));
        }
        self.outputs = Some(Outputs {
            agent,
            vetoed: text_editor::Content::with_text(&read("vetoed.md")),
            tasks: text_editor::Content::with_text(&read("tasks.md")),
            folder,
            files,
            own,
        });
    }

    fn load_feedback(&mut self) {
        let root = self.root();
        self.feedback = self
            .team()
            .map(|team| {
                sessions(team)
                    .iter()
                    .map(|session| {
                        let dir = folder::agent_dir(&root, team, &session.root);
                        let read = |p: &str| fs::read_to_string(dir.join(p)).unwrap_or_default();
                        (
                            team.name_of(&session.root).to_owned(),
                            read("app/bugs.md"),
                            read("app/ideas.md"),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
    }

    /// The most a run of the current team can spend: every step's session cap.
    #[must_use]
    pub fn most_spend(&self) -> Option<f64> {
        let team = self.team()?;
        let plan = plan(team).ok()?;
        Some(
            plan.steps
                .iter()
                .filter_map(|step| team.agent(&plan.sessions[step.session].root))
                .fold(0.0, |most, agent| most + agent.spend_cap),
        )
    }

    /// Whether the selected agent can't have its own spend cap, being a member.
    #[must_use]
    pub fn is_member(&self, id: &str) -> bool {
        self.team().is_some_and(|team| !team.is_root(id))
    }
}

/// The newest file in a folder.
fn newest(dir: &Path) -> Option<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    files.sort();
    files.pop()
}

/// What the Agents view asks for.
#[derive(Clone, Debug)]
pub enum AgentsMessage {
    Toggle,
    /// Open this team, by id.
    Pick(String),
    New(Starter),
    Duplicate,
    Archive,
    TeamName(String),
    Select(Selected),
    /// A card (or [`LAB`]) dragged to here; `done` once it's let go.
    Moved {
        id: String,
        at: [f32; 2],
        done: bool,
    },
    /// A link drawn from one card to another, as the kind picked.
    Connect(String, String),
    /// The canvas panned or zoomed; `done` once it's settled.
    View(View, bool),
    /// A new agent, its card here.
    Add([f32; 2]),
    /// Take off what's selected.
    Remove,
    LinkAs(LinkKind),
    Name(String),
    Role(text_editor::Action),
    Model(Model),
    Effort(Effort),
    /// A number field typed into: which, and what's there.
    Number(&'static str, String),
    Memory(bool),
    AppNotes(bool),
    UsesLab(bool),
    Kind(LinkKind),
    Rounds(u32),
    Reverse,
    Brief(text_editor::Action),
    Principles(text_editor::Action),
    AtOnce(u32),
    LabProgram(String),
    LabOwnData(bool),
    LabFolder(String),
    Claude(String),
    Root(String),
    Check,
    Found(Found),
    Run,
    WrapUp,
    Stop,
    Running(RunEvent),
    RunFailed(String),
    Tab(Tab),
    PastRun(String),
    OpenFolder,
    Vetoed(text_editor::Action),
    Tasks(text_editor::Action),
    SaveNotes,
}

/// What the run counts as `budget` and `spend` take; at once is a pick.
pub const BUDGET: &str = "budget";
pub const SPEND: &str = "spend";

impl crate::Persephone {
    #[must_use]
    pub fn agents(&self) -> &Agents {
        &self.agents
    }

    /// Keeps teams in `root` instead of `~/persephone-agents`, and says Claude Code and Lab are
    /// as `found` says, without looking. For tests.
    #[doc(hidden)]
    pub fn use_agents_root(&mut self, root: PathBuf, found: Found) {
        self.agents.saved.root = Some(root);
        self.agents.found = Some(found);
        self.agents.load();
    }

    pub(crate) fn agents_view(&self) -> iced::Element<'_, Message> {
        view::view(&self.agents)
    }

    /// Asks every running session to stop, and closes `window` once they have.
    pub(crate) fn close_after_agents(&mut self, window: window::Id) -> Task<Message> {
        if !self.agents.running() {
            return window::close(window);
        }
        self.agents.closing = Some(window);
        if let Some(live) = &self.agents.live {
            live.control
                .stop
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Task::none()
    }

    pub(crate) fn on_agents(&mut self, message: AgentsMessage) -> Task<Message> {
        let task = self.agents_update(message);
        self.agents.fill_editors();
        task
    }

    #[allow(clippy::too_many_lines)]
    fn agents_update(&mut self, message: AgentsMessage) -> Task<Message> {
        let agents = &mut self.agents;
        match message {
            AgentsMessage::Toggle => {
                agents.saved.open = !agents.saved.open;
                self.popup = None;
                if self.agents.saved.open {
                    self.agents.load();
                    self.save();
                    return self.look_around();
                }
                self.save();
            }
            AgentsMessage::Pick(id) => {
                agents.saved.team = Some(id);
                agents.selected = Selected::Team;
                agents.after_switch();
                self.save();
            }
            AgentsMessage::New(starter) => {
                let name = (1..)
                    .map(|n| {
                        if n == 1 {
                            starter.label().to_owned()
                        } else {
                            format!("{} {n}", starter.label())
                        }
                    })
                    .find(|name| agents.teams.iter().all(|t| &t.name != name))
                    .unwrap_or_default();
                let team = Team::new(&name, starter);
                agents.saved.team = Some(team.id.clone());
                agents.teams.push(team);
                agents.selected = Selected::Team;
                agents.tab = Tab::Setup;
                agents.save_team();
                agents.after_switch();
                self.save();
            }
            AgentsMessage::Duplicate => {
                if let Some(team) = agents.team() {
                    let mut copy = team.clone();
                    copy.name = format!("{} copy", team.name);
                    copy.id = fresh_id(&copy.name);
                    // New agents, with folders of their own: none of the original's memory.
                    let ids: Vec<(String, String)> = copy
                        .agents
                        .iter()
                        .map(|a| (a.id.clone(), fresh_id(&a.name)))
                        .collect();
                    for (old, new) in &ids {
                        for agent in &mut copy.agents {
                            if &agent.id == old {
                                agent.id.clone_from(new);
                            }
                        }
                        for link in &mut copy.links {
                            if &link.from == old {
                                link.from.clone_from(new);
                            }
                            if &link.to == old {
                                link.to.clone_from(new);
                            }
                        }
                    }
                    agents.saved.team = Some(copy.id.clone());
                    agents.teams.push(copy);
                    agents.save_team();
                    agents.after_switch();
                    self.save();
                }
            }
            AgentsMessage::Archive => {
                if agents.running() {
                    agents.note = Some("Stop the run before archiving its team.".into());
                    return Task::none();
                }
                let root = agents.root();
                if let Some(team) = agents.team().cloned() {
                    match folder::archive(&root, &folder::team_dir(&root, &team), &team.id) {
                        Ok(to) => agents.note = Some(format!("Archived to {}", to.display())),
                        Err(why) => agents.note = Some(why),
                    }
                    agents.saved.team = None;
                    agents.load();
                    self.save();
                }
            }
            AgentsMessage::TeamName(name) => {
                if let Some(team) = agents.team_mut() {
                    team.name = name;
                    agents.save_team();
                }
            }
            AgentsMessage::Select(selected) => {
                agents.selected = selected;
                if agents.tab == Tab::Outputs || matches!(agents.selected, Selected::Agent(_)) {
                    agents.load_outputs();
                }
                agents.typing.clear();
            }
            AgentsMessage::Moved { id, at, done } => {
                if let Some(team) = agents.team_mut() {
                    if id == LAB {
                        team.lab.at = at;
                    } else if let Some(agent) = team.agent_mut(&id) {
                        agent.at = at;
                    }
                    if done {
                        agents.save_team();
                    }
                }
            }
            AgentsMessage::Connect(from, to) => {
                let kind = agents.link_as;
                if let Some(team) = agents.team_mut() {
                    let kind = if to == LAB { LinkKind::Uses } else { kind };
                    let (from, to) = if kind == LinkKind::Uses && from == LAB {
                        (to, from)
                    } else {
                        (from, to)
                    };
                    if kind == LinkKind::Uses && to != LAB {
                        agents.note = Some("A Uses link goes from an agent to Lab.".into());
                        return Task::none();
                    }
                    let exists = team
                        .links
                        .iter()
                        .any(|l| l.from == from && l.to == to && l.kind.same_kind(kind));
                    if !exists && from != to {
                        // An agent has one lead: a new one replaces the old.
                        if kind == LinkKind::ReportsTo {
                            team.links
                                .retain(|l| !(l.from == from && l.kind == LinkKind::ReportsTo));
                        }
                        team.links.push(Link { from, to, kind });
                        agents.selected = Selected::Link(team.links.len() - 1);
                        agents.save_team();
                    }
                }
            }
            AgentsMessage::View(view, done) => {
                if let Some(team) = agents.team_mut() {
                    team.view = view;
                    if done {
                        agents.save_team();
                    }
                }
            }
            AgentsMessage::Add(at) => {
                if let Some(team) = agents.team_mut() {
                    let id = team.add_agent(at);
                    agents.selected = Selected::Agent(id);
                    agents.tab = Tab::Setup;
                    agents.save_team();
                }
            }
            AgentsMessage::Remove => {
                let root = agents.root();
                let selected = agents.selected.clone();
                if let Some(team) = agents.team_mut() {
                    match selected {
                        Selected::Agent(id) => {
                            let was_top = team.is_root(&id);
                            team.remove_agent(&id);
                            let team = team.clone();
                            // Its folder, if it had one, goes to the archive: nothing's deleted.
                            let dir = folder::agent_dir(&root, &team, &id);
                            if was_top && dir.exists() {
                                match folder::archive(&root, &dir, &format!("{}-{id}", team.id)) {
                                    Ok(to) => {
                                        agents.note =
                                            Some(format!("Its folder is now {}", to.display()));
                                    }
                                    Err(why) => agents.note = Some(why),
                                }
                            }
                        }
                        Selected::Link(index) if index < team.links.len() => {
                            team.links.remove(index);
                        }
                        _ => return Task::none(),
                    }
                    agents.selected = Selected::Team;
                    agents.save_team();
                }
            }
            AgentsMessage::LinkAs(kind) => agents.link_as = kind,
            AgentsMessage::Name(name) => {
                if let Selected::Agent(id) = agents.selected.clone()
                    && let Some(agent) = agents.team_mut().and_then(|t| t.agent_mut(&id))
                {
                    agent.name = name;
                    agents.save_team();
                }
            }
            AgentsMessage::Role(action) => {
                let editing = action.is_edit();
                agents.role.perform(action);
                if editing && let Selected::Agent(id) = agents.selected.clone() {
                    let role = agents.role.text();
                    if let Some(agent) = agents.team_mut().and_then(|t| t.agent_mut(&id)) {
                        agent.role = role.trim_end().to_owned();
                        agents.save_team();
                    }
                }
            }
            AgentsMessage::Model(model) => {
                agents.edit_agent(|a| a.model = model);
            }
            AgentsMessage::Effort(effort) => {
                agents.edit_agent(|a| a.effort = effort);
            }
            AgentsMessage::Number(field, typed) => {
                let number = typed.trim().trim_start_matches('$').replace(',', "");
                match field {
                    BUDGET => match number.parse::<u32>() {
                        Ok(budget) if budget > 0 => {
                            agents.typing.remove(field);
                            agents.edit_agent(|a| a.budget = budget);
                        }
                        _ => {
                            agents.typing.insert(field, typed);
                        }
                    },
                    SPEND => match number.parse::<f64>() {
                        Ok(cap) if cap > 0.0 && cap.is_finite() => {
                            agents.typing.remove(field);
                            agents.edit_agent(|a| a.spend_cap = cap);
                        }
                        _ => {
                            agents.typing.insert(field, typed);
                        }
                    },
                    _ => {}
                }
            }
            AgentsMessage::Memory(on) => agents.edit_agent(|a| a.memory = on),
            AgentsMessage::AppNotes(on) => agents.edit_agent(|a| a.app_notes = on),
            AgentsMessage::UsesLab(on) => {
                if let Selected::Agent(id) = agents.selected.clone()
                    && let Some(team) = agents.team_mut()
                {
                    team.links
                        .retain(|l| !(l.from == id && l.kind == LinkKind::Uses));
                    if on {
                        team.links.push(Link {
                            from: id,
                            to: LAB.into(),
                            kind: LinkKind::Uses,
                        });
                    }
                    agents.save_team();
                }
            }
            AgentsMessage::Kind(kind) => {
                agents.edit_link(|l| {
                    if l.to != LAB && kind != LinkKind::Uses {
                        l.kind = kind;
                    }
                });
            }
            AgentsMessage::Rounds(rounds) => {
                agents.edit_link(|l| {
                    if let LinkKind::Reviews { rounds: r } = &mut l.kind {
                        *r = rounds;
                    }
                });
            }
            AgentsMessage::Reverse => {
                agents.edit_link(|l| {
                    if l.to != LAB {
                        std::mem::swap(&mut l.from, &mut l.to);
                    }
                });
            }
            AgentsMessage::Brief(action) => {
                let editing = action.is_edit();
                agents.brief.perform(action);
                if editing {
                    let brief = agents.brief.text();
                    if let Some(team) = agents.team_mut() {
                        team.brief = brief.trim_end().to_owned();
                        agents.save_team();
                    }
                }
            }
            AgentsMessage::Principles(action) => {
                let editing = action.is_edit();
                agents.principles.perform(action);
                if editing {
                    let principles = agents.principles.text();
                    if let Some(team) = agents.team_mut() {
                        team.principles = principles.trim_end().to_owned();
                        agents.save_team();
                    }
                }
            }
            AgentsMessage::AtOnce(n) => {
                if let Some(team) = agents.team_mut() {
                    team.at_once = n.max(1);
                    agents.save_team();
                }
            }
            AgentsMessage::LabProgram(path) => {
                if let Some(team) = agents.team_mut() {
                    let path = path.trim();
                    team.lab.program = (!path.is_empty()).then(|| PathBuf::from(path));
                    agents.save_team();
                }
                return self.look_around();
            }
            AgentsMessage::LabOwnData(own) => {
                if let Some(team) = agents.team_mut() {
                    team.lab.data = if own {
                        LabData::own_default()
                    } else {
                        LabData::App
                    };
                    agents.save_team();
                }
            }
            AgentsMessage::LabFolder(path) => {
                if let Some(team) = agents.team_mut() {
                    let path = path.trim();
                    team.lab.data = if path.is_empty() {
                        LabData::own_default()
                    } else {
                        LabData::Own(PathBuf::from(path))
                    };
                    agents.save_team();
                }
            }
            AgentsMessage::Claude(path) => {
                let path = path.trim();
                agents.saved.claude = (!path.is_empty()).then(|| PathBuf::from(path));
                self.save();
                return self.look_around();
            }
            AgentsMessage::Root(path) => {
                if agents.running() {
                    agents.note = Some("Stop the run before moving the teams' folder.".into());
                    return Task::none();
                }
                let path = path.trim();
                agents.saved.root = (!path.is_empty()).then(|| PathBuf::from(path));
                agents.load();
                self.save();
            }
            AgentsMessage::Check => {
                agents.check();
                return self.look_around();
            }
            AgentsMessage::Found(found) => {
                agents.found = Some(found);
                agents.looking = false;
                if !agents.problems.is_empty() {
                    agents.check();
                }
            }
            AgentsMessage::Run => return self.start_agents(),
            AgentsMessage::WrapUp => {
                if let Some(live) = &agents.live {
                    live.control
                        .wrap_up
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            AgentsMessage::Stop => {
                if let Some(live) = &agents.live {
                    live.control
                        .stop
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            AgentsMessage::Running(event) => {
                let ended = matches!(event, RunEvent::Ended(_));
                let step_ended =
                    matches!(&event, RunEvent::Status { status, .. } if status.finished());
                if let Some(live) = &mut agents.live {
                    live.apply(event);
                }
                if ended || step_ended {
                    agents.load_outputs();
                    agents.load_feedback();
                }
                if ended {
                    agents.past = agents
                        .team()
                        .map(|team| run::past_runs(&agents.root(), team))
                        .unwrap_or_default();
                    if let Some(window) = agents.closing.take() {
                        return window::close(window);
                    }
                }
            }
            AgentsMessage::RunFailed(why) => {
                if let Some(live) = &mut agents.live {
                    live.failed = Some(why.clone());
                }
                agents.note = Some(why);
                if let Some(window) = agents.closing.take() {
                    return window::close(window);
                }
            }
            AgentsMessage::Tab(tab) => {
                agents.tab = tab;
                match tab {
                    Tab::Outputs => agents.load_outputs(),
                    Tab::Feedback => agents.load_feedback(),
                    _ => {}
                }
            }
            AgentsMessage::PastRun(run) => {
                let root = agents.root();
                let found = agents.past.iter().find(|s| s.run == run).cloned();
                if let (Some(summary), Some(team)) = (found, agents.team()) {
                    let mut lines = Vec::new();
                    for (step, record) in summary.steps.iter().enumerate() {
                        lines.extend(
                            run::read_log(&root, team, &summary.run, &record.log)
                                .into_iter()
                                .map(|event| (step, Line::Event(event))),
                        );
                    }
                    agents.viewing = Some((summary, lines));
                    agents.tab = Tab::Activity;
                } else {
                    agents.viewing = None;
                }
            }
            AgentsMessage::OpenFolder => {
                if let Some(outputs) = &agents.outputs
                    && let Err(why) = opener::open(&outputs.folder)
                {
                    agents.note = Some(format!("Couldn't open the folder: {why}"));
                }
            }
            AgentsMessage::Vetoed(action) => {
                if let Some(outputs) = &mut agents.outputs {
                    outputs.vetoed.perform(action);
                }
            }
            AgentsMessage::Tasks(action) => {
                if let Some(outputs) = &mut agents.outputs {
                    outputs.tasks.perform(action);
                }
            }
            AgentsMessage::SaveNotes => {
                if let Some(outputs) = &agents.outputs {
                    for (file, content) in
                        [("vetoed.md", &outputs.vetoed), ("tasks.md", &outputs.tasks)]
                    {
                        let text = content.text();
                        if let Err(why) =
                            folder::write_aside(&outputs.folder.join(file), text.as_bytes())
                        {
                            agents.note = Some(why);
                        }
                    }
                    if agents.note.is_none() {
                        agents.note = Some("Saved vetoed.md and tasks.md.".into());
                    }
                }
            }
        }
        Task::none()
    }

    /// Looks for Claude Code and Lab, and whether Claude Code is logged in, off the window's
    /// thread.
    fn look_around(&mut self) -> Task<Message> {
        if self.agents.looking {
            return Task::none();
        }
        self.agents.looking = true;
        let claude = self.agents.saved.claude.clone();
        let lab = self.agents.team().and_then(|t| t.lab.program.clone());
        Task::perform(
            async move {
                std::thread::spawn(move || run::look_around(claude.as_deref(), lab.as_deref()))
                    .join()
                    .unwrap_or_default()
            },
            |found| Message::Agents(AgentsMessage::Found(found)),
        )
    }

    /// Starts the current team, if it can run.
    fn start_agents(&mut self) -> Task<Message> {
        let agents = &mut self.agents;
        if agents.running() {
            agents.note = Some("A run is going already.".into());
            return Task::none();
        }
        agents.check();
        if !agents.problems.is_empty() {
            agents.tab = Tab::Setup;
            return Task::none();
        }
        let (Some(team), Some(found)) = (agents.team().cloned(), agents.found.clone()) else {
            agents.note = Some("Still looking for Claude Code; try again in a moment.".into());
            return Task::none();
        };
        let Some(claude) = found.claude else {
            return Task::none();
        };
        let setup = Setup {
            root: agents.root(),
            claude,
            lab: team.lab.program.clone().or(found.lab),
            keep_out: run::code_folder().into_iter().collect(),
        };
        let control = Control::default();
        agents.live = Some(Live::new(&team.id, control.clone()));
        agents.viewing = None;
        agents.note = None;
        agents.tab = Tab::Activity;
        let stream = iced::stream::channel(64, async move |mut output: mpsc::Sender<Message>| {
            let (tx, mut rx) = mpsc::unbounded();
            std::thread::spawn(move || {
                let said = tx.clone();
                let result = run::run(&team, &setup, &control, &mut |event| {
                    let _ = said.unbounded_send(Ok(event));
                });
                if let Err(why) = result {
                    let _ = tx.unbounded_send(Err(why));
                }
            });
            while let Some(item) = rx.next().await {
                let message = match item {
                    Ok(event) => AgentsMessage::Running(event),
                    Err(why) => AgentsMessage::RunFailed(why),
                };
                if output.send(Message::Agents(message)).await.is_err() {
                    return;
                }
            }
        });
        Task::run(stream, |message| message)
    }
}

impl Agents {
    /// What's wrong with the current team, and with this computer if it's been looked at.
    fn check(&mut self) {
        let Some(team) = self.team() else {
            self.problems.clear();
            return;
        };
        let mut problems = check(team);
        if let Some(found) = &self.found {
            problems.extend(check_setup(team, found));
        }
        self.problems = problems;
    }

    fn edit_agent(&mut self, change: impl FnOnce(&mut persephone_agents::team::Agent)) {
        if let Selected::Agent(id) = self.selected.clone()
            && let Some(agent) = self.team_mut().and_then(|t| t.agent_mut(&id))
        {
            change(agent);
            self.save_team();
        }
    }

    fn edit_link(&mut self, change: impl FnOnce(&mut Link)) {
        if let Selected::Link(index) = self.selected
            && let Some(link) = self.team_mut().and_then(|t| t.links.get_mut(index))
        {
            change(link);
            self.save_team();
        }
    }
}
