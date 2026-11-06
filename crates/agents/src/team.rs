//! A team as it's drawn: agent cards, the links between them, and the Lab card. Saved as the
//! team folder's `team.json`.

use std::{
    collections::hash_map::RandomState,
    fmt,
    hash::{BuildHasher, Hasher},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};

/// The Lab card's id, which a [`LinkKind::Uses`] link goes to.
pub const LAB: &str = "lab";

/// The models an agent can be: Claude Code's names follow the newest of each.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Model {
    Fable,
    Opus,
    Sonnet,
    Haiku,
}

impl Model {
    pub const ALL: [Self; 4] = [Self::Fable, Self::Opus, Self::Sonnet, Self::Haiku];

    /// What Claude Code calls it, for `--model` and an agent's `model`.
    #[must_use]
    pub const fn cli(self) -> &'static str {
        match self {
            Self::Fable => "fable",
            Self::Opus => "opus",
            Self::Sonnet => "sonnet",
            Self::Haiku => "haiku",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fable => "Fable 5.1",
            Self::Opus => "Opus 5.5",
            Self::Sonnet => "Sonnet 5.5",
            Self::Haiku => "Haiku 5.5",
        }
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// How hard an agent thinks before it acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub const ALL: [Self; 5] = [Self::Low, Self::Medium, Self::High, Self::Xhigh, Self::Max];

    /// What Claude Code calls it, for `--effort` and an agent's `effort`.
    #[must_use]
    pub const fn cli(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::Xhigh => "Extra high",
            Self::Max => "Max",
        }
    }
}

impl fmt::Display for Effort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One agent: a card on the canvas.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    /// Fixed when it's made, and its folder's name, so renaming it keeps its memory.
    pub id: String,
    pub name: String,
    /// What it does, in its own words: the heart of its prompt.
    pub role: String,
    pub model: Model,
    pub effort: Effort,
    /// The tool calls it's asked to stay within each session.
    pub budget: u32,
    /// The most one session may spend, in US dollars at API prices. Only for an agent that
    /// reports to no one: a member spends from its lead's session.
    pub spend_cap: f64,
    /// Whether it keeps a memory from one run to the next.
    pub memory: bool,
    /// Whether it writes bugs and ideas for the app.
    pub app_notes: bool,
    /// Where its card's centre is on the canvas.
    pub at: [f32; 2],
}

/// What a link means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LinkKind {
    /// `from` is one of `to`'s team: `to` hands it work and checks what comes back.
    ReportsTo,
    /// `to` works from what `from` found.
    HandsTo,
    /// `from` re-runs `to`'s claims, and `to` answers; so many rounds.
    Reviews { rounds: u32 },
    /// `from` has Lab's tools; `to` is [`LAB`].
    Uses,
}

impl LinkKind {
    /// The kinds the canvas can draw a new link as, in order.
    pub const DRAWN: [Self; 4] = [
        Self::ReportsTo,
        Self::HandsTo,
        Self::Reviews { rounds: 1 },
        Self::Uses,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReportsTo => "Reports to",
            Self::HandsTo => "Hands to",
            Self::Reviews { .. } => "Reviews",
            Self::Uses => "Uses",
        }
    }

    /// The same kind, whatever its rounds.
    #[must_use]
    pub fn same_kind(self, other: Self) -> bool {
        std::mem::discriminant(&self) == std::mem::discriminant(&other)
    }
}

impl fmt::Display for LinkKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub from: String,
    pub to: String,
    #[serde(flatten)]
    pub kind: LinkKind,
}

/// Whose data Lab works on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabData {
    /// A folder of the agents' own, so their runs don't mix with yours.
    Own(PathBuf),
    /// The app's, so what they save shows in your saved results.
    App,
}

impl LabData {
    /// `~/persephone-agent-data`.
    #[must_use]
    pub fn own_default() -> Self {
        Self::Own(home().join("persephone-agent-data"))
    }
}

/// The Lab card: which Lab every agent that uses it starts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LabCard {
    /// Lab's program; none for the one beside the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<PathBuf>,
    pub data: LabData,
    pub at: [f32; 2],
}

/// Where the canvas was looking: the point at its centre, and how far in.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub at: [f32; 2],
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        Self {
            at: [0.0, 0.0],
            zoom: 1.0,
        }
    }
}

/// A team: what it studies, how it works, and who's in it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Team {
    /// Fixed when it's made, and its folder's name.
    pub id: String,
    pub name: String,
    /// What to study: every agent's `subject.md`.
    pub brief: String,
    /// How to work: every agent's `principles.md`.
    pub principles: String,
    pub agents: Vec<Agent>,
    pub links: Vec<Link>,
    pub lab: LabCard,
    /// How many sessions may run at once.
    pub at_once: u32,
    #[serde(default)]
    pub view: View,
}

/// The teams to start from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Starter {
    Blank,
    /// One agent: the agent kit's quant analyst.
    OneAnalyst,
    /// An Opus lead with two Sonnet analysts in one session.
    LeadAndAnalysts,
    /// Two sessions: a researcher, and a reviewer re-running its claims.
    ResearcherAndReviewer,
}

impl Starter {
    pub const ALL: [Self; 4] = [
        Self::Blank,
        Self::OneAnalyst,
        Self::LeadAndAnalysts,
        Self::ResearcherAndReviewer,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Blank => "Blank",
            Self::OneAnalyst => "One analyst",
            Self::LeadAndAnalysts => "Lead and analysts",
            Self::ResearcherAndReviewer => "Researcher and reviewer",
        }
    }
}

impl fmt::Display for Starter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The agent kit's principles: method only, no market views.
pub const PRINCIPLES: &str = include_str!("../template/principles.md");

const ANALYST: &str = "A professional quantitative analyst and a beta user of Persephone. Study what the brief asks through Lab, and report what's true, including that nothing is worth trading when that's what you find.";
const LEAD: &str = "Leads the research. Turns the brief into questions that can be tested, gives each analyst one to work, checks what they bring back by re-running what matters, and writes the team's report.";
const TEAM_ANALYST: &str = "Works the question the lead gives it through Lab, with realistic costs, and brings back the figures and the run ids behind them.";
const RESEARCHER: &str = "A professional quantitative analyst. Studies what the brief asks through Lab, and writes up what it found with the runs behind it, for the reviewer to check.";
const REVIEWER: &str = "Checks the researcher's claims by re-running them in Lab, and with tests that could break them: other periods, shifted starts, random entries and higher costs. Says plainly what holds and what doesn't.";

impl Team {
    #[must_use]
    pub fn new(name: &str, starter: Starter) -> Self {
        let mut team = Self {
            id: fresh_id(name),
            name: name.to_owned(),
            brief: String::new(),
            principles: PRINCIPLES.to_owned(),
            agents: Vec::new(),
            links: Vec::new(),
            lab: LabCard {
                program: None,
                data: LabData::own_default(),
                at: [0.0, 260.0],
            },
            at_once: 2,
            view: View::default(),
        };
        let add = |team: &mut Self, name: &str, role: &str, model, effort, budget, at| {
            let id = team.add_agent(at);
            let agent = team.agent_mut(&id).expect("just added");
            agent.name = name.to_owned();
            agent.role = role.to_owned();
            agent.model = model;
            agent.effort = effort;
            agent.budget = budget;
            team.links.push(Link {
                from: id.clone(),
                to: LAB.to_owned(),
                kind: LinkKind::Uses,
            });
            id
        };
        match starter {
            Starter::Blank => {}
            Starter::OneAnalyst => {
                add(
                    &mut team,
                    "Analyst",
                    ANALYST,
                    Model::Opus,
                    Effort::High,
                    150,
                    [0.0, 0.0],
                );
            }
            Starter::LeadAndAnalysts => {
                let lead = add(
                    &mut team,
                    "Lead",
                    LEAD,
                    Model::Opus,
                    Effort::High,
                    60,
                    [0.0, -120.0],
                );
                for (name, x) in [("Analyst A", -150.0), ("Analyst B", 150.0)] {
                    let id = add(
                        &mut team,
                        name,
                        TEAM_ANALYST,
                        Model::Sonnet,
                        Effort::Medium,
                        80,
                        [x, 60.0],
                    );
                    team.links.push(Link {
                        from: id,
                        to: lead.clone(),
                        kind: LinkKind::ReportsTo,
                    });
                }
                if let Some(lead) = team.agent_mut(&lead) {
                    lead.spend_cap = 20.0;
                }
            }
            Starter::ResearcherAndReviewer => {
                let researcher = add(
                    &mut team,
                    "Researcher",
                    RESEARCHER,
                    Model::Opus,
                    Effort::High,
                    150,
                    [-150.0, 0.0],
                );
                let reviewer = add(
                    &mut team,
                    "Reviewer",
                    REVIEWER,
                    Model::Sonnet,
                    Effort::High,
                    80,
                    [150.0, 0.0],
                );
                team.links.push(Link {
                    from: reviewer,
                    to: researcher,
                    kind: LinkKind::Reviews { rounds: 1 },
                });
            }
        }
        team
    }

    /// Adds an agent with everyday settings, its card's centre at `at`, and says its id.
    pub fn add_agent(&mut self, at: [f32; 2]) -> String {
        let number = self.agents.len() + 1;
        let name = (number..)
            .map(|n| format!("Agent {n}"))
            .find(|name| self.agents.iter().all(|a| a.name != *name))
            .unwrap_or_default();
        let id = fresh_id(&name);
        self.agents.push(Agent {
            id: id.clone(),
            name,
            role: String::new(),
            model: Model::Sonnet,
            effort: Effort::Medium,
            budget: 80,
            spend_cap: 10.0,
            memory: true,
            app_notes: true,
            at,
        });
        id
    }

    /// Takes an agent off, and every link to or from it.
    pub fn remove_agent(&mut self, id: &str) {
        self.agents.retain(|a| a.id != id);
        self.links.retain(|l| l.from != id && l.to != id);
    }

    #[must_use]
    pub fn agent(&self, id: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.id == id)
    }

    pub fn agent_mut(&mut self, id: &str) -> Option<&mut Agent> {
        self.agents.iter_mut().find(|a| a.id == id)
    }

    /// An agent's name, or its id if it's gone.
    #[must_use]
    pub fn name_of<'a>(&'a self, id: &'a str) -> &'a str {
        if id == LAB {
            return "Lab";
        }
        self.agent(id).map_or(id, |a| a.name.as_str())
    }

    /// Who it reports to, if anyone: the first, should a design have two.
    #[must_use]
    pub fn lead_of(&self, id: &str) -> Option<&str> {
        self.links
            .iter()
            .find(|l| l.from == id && l.kind == LinkKind::ReportsTo)
            .map(|l| l.to.as_str())
    }

    /// Who reports to it, in the order their cards are, left to right.
    #[must_use]
    pub fn reports_of(&self, id: &str) -> Vec<&Agent> {
        let mut reports: Vec<&Agent> = self
            .links
            .iter()
            .filter(|l| l.to == id && l.kind == LinkKind::ReportsTo)
            .filter_map(|l| self.agent(&l.from))
            .collect();
        reports.sort_by(|a, b| a.at[0].total_cmp(&b.at[0]));
        reports
    }

    /// Whether its own card is linked to Lab.
    #[must_use]
    pub fn uses_lab(&self, id: &str) -> bool {
        self.links
            .iter()
            .any(|l| l.from == id && l.kind == LinkKind::Uses)
    }

    /// The agent at the top of its session: itself if it reports to no one. Stops at a loop,
    /// which [`check`](crate::check::check) reports.
    #[must_use]
    pub fn root_of<'a>(&'a self, id: &'a str) -> &'a str {
        let mut at = id;
        for _ in 0..=self.agents.len() {
            match self.lead_of(at) {
                Some(lead) => at = lead,
                None => return at,
            }
        }
        at
    }

    /// Whether it reports to no one, so it's a session of its own.
    #[must_use]
    pub fn is_root(&self, id: &str) -> bool {
        self.lead_of(id).is_none()
    }

    /// Whether anyone in its session uses Lab, which then the whole session can reach.
    #[must_use]
    pub fn session_uses_lab(&self, root: &str) -> bool {
        self.agents
            .iter()
            .any(|a| self.root_of(&a.id) == root && self.uses_lab(&a.id))
    }
}

/// The name an agent goes by in Claude Code (`--agents` and `subagent_type`): letters, digits
/// and dashes.
#[must_use]
pub fn key(name: &str) -> String {
    let mut key = String::new();
    for c in name.trim().chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            key.push(c);
        } else if !key.is_empty() && !key.ends_with('-') {
            key.push('-');
        }
    }
    let key = key.trim_end_matches('-');
    let key: String = key.chars().take(40).collect();
    key.trim_end_matches('-').to_owned()
}

/// A random-enough id, from `name` and four letters or digits: `lead-x7k2`.
#[must_use]
pub fn fresh_id(name: &str) -> String {
    let base = key(name);
    let suffix: String = random_u64()
        .to_le_bytes()
        .iter()
        .take(4)
        .map(|b| char::from(b"abcdefghijklmnopqrstuvwxyz0123456789"[usize::from(*b) % 36]))
        .collect();
    if base.is_empty() {
        suffix
    } else {
        format!("{base}-{suffix}")
    }
}

/// A session id for Claude Code's `--session-id`: a UUID, version 4.
#[must_use]
pub fn session_uuid() -> String {
    let (a, b) = (random_u64(), random_u64());
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&a.to_le_bytes());
    bytes[8..].copy_from_slice(&b.to_le_bytes());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// 64 bits no other call in this process, or likely any other, gets: the standard library's
/// randomly keyed hasher over a count and the time.
fn random_u64() -> u64 {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(COUNT.fetch_add(1, Ordering::Relaxed));
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    hasher.finish()
}

/// The home folder, or the current one on a system without.
#[must_use]
pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_what_claude_code_takes() {
        assert_eq!(key("Analyst A"), "analyst-a");
        assert_eq!(key("  Head of Research!  "), "head-of-research");
        assert_eq!(key("Lead/Analyst (Opus)"), "lead-analyst-opus");
        assert_eq!(key("Ünïcode"), "n-code");
        assert!(key(&"x".repeat(99)).len() <= 40);
    }

    #[test]
    fn ids_differ_and_start_with_the_name() {
        let a = fresh_id("Lead");
        let b = fresh_id("Lead");
        assert_ne!(a, b);
        assert!(a.starts_with("lead-"), "{a}");
    }

    #[test]
    fn session_ids_are_uuids() {
        let id = session_uuid();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"), "{id}");
        assert_ne!(id, session_uuid());
    }

    #[test]
    fn starters_are_what_they_say() {
        let one = Team::new("Solo", Starter::OneAnalyst);
        assert_eq!(one.agents.len(), 1);
        assert!(one.uses_lab(&one.agents[0].id));

        let team = Team::new("Desk", Starter::LeadAndAnalysts);
        let lead = &team.agents[0];
        assert_eq!(lead.model, Model::Opus);
        let reports = team.reports_of(&lead.id);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].name, "Analyst A", "left to right");
        assert_eq!(team.root_of(&reports[1].id), lead.id);

        let pair = Team::new("Pair", Starter::ResearcherAndReviewer);
        assert!(pair.agents.iter().all(|a| pair.is_root(&a.id)));
        assert!(
            pair.links
                .iter()
                .any(|l| l.kind == LinkKind::Reviews { rounds: 1 })
        );
    }

    #[test]
    fn a_team_reads_back_as_it_was_saved() {
        let team = Team::new("Desk", Starter::LeadAndAnalysts);
        let text = serde_json::to_string_pretty(&team).unwrap();
        assert!(text.contains("\"kind\": \"reports_to\""), "{text}");
        let again: Team = serde_json::from_str(&text).unwrap();
        assert_eq!(again, team);
    }

    #[test]
    fn removing_an_agent_takes_its_links() {
        let mut team = Team::new("Desk", Starter::LeadAndAnalysts);
        let lead = team.agents[0].id.clone();
        team.remove_agent(&lead);
        assert_eq!(team.agents.len(), 2);
        assert!(team.links.iter().all(|l| l.from != lead && l.to != lead));
        assert!(team.agents.iter().all(|a| team.is_root(&a.id)));
    }
}
