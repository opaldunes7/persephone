//! What's wrong with a design, before it runs: each problem says what to change and which cards
//! and links it's about.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use crate::{
    plan,
    team::{LAB, LinkKind, Team, key},
};

/// Something to fix before the team can run.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub what: String,
    /// The agents it's about, by id; the first is the one to show.
    pub agents: Vec<String>,
    /// The links it's about, by index.
    pub links: Vec<usize>,
}

impl Problem {
    fn new(what: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            agents: Vec::new(),
            links: Vec::new(),
        }
    }

    fn about(mut self, agents: &[&str]) -> Self {
        self.agents.extend(
            agents
                .iter()
                .filter(|a| **a != LAB)
                .map(|a| (*a).to_owned()),
        );
        self
    }

    fn link(mut self, index: usize) -> Self {
        self.links.push(index);
        self
    }
}

/// What a run needs from this computer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Found {
    /// Claude Code's program, if it was found.
    pub claude: Option<PathBuf>,
    /// Whether Claude Code is logged in; none if that couldn't be told.
    pub logged_in: Option<bool>,
    /// Lab's program, if it was found.
    pub lab: Option<PathBuf>,
}

/// Everything wrong with the design itself, first things first. Empty when it can run.
#[must_use]
pub fn check(team: &Team) -> Vec<Problem> {
    let mut problems = Vec::new();
    if team.agents.is_empty() {
        problems.push(Problem::new("Add an agent to the team."));
        return problems;
    }
    if team.brief.trim().is_empty() {
        problems.push(Problem::new(
            "The brief is empty: say what the team should study.",
        ));
    }
    agents(team, &mut problems);
    let links_fine = links(team, &mut problems);
    if links_fine {
        hierarchy(team, &mut problems);
    }
    if problems.is_empty()
        && let Err(circle) = plan::steps(team)
    {
        let names: Vec<&str> = circle.iter().map(|id| team.name_of(id)).collect();
        let ids: Vec<&str> = circle.iter().map(String::as_str).collect();
        problems.push(
            Problem::new(format!(
                "{} wait on each other through their hand-offs and reviews, so the run could never finish.",
                list(&names)
            ))
            .about(&ids),
        );
    }
    problems
}

/// What's missing on this computer for `team` to run.
#[must_use]
pub fn check_setup(team: &Team, found: &Found) -> Vec<Problem> {
    let mut problems = Vec::new();
    if found.claude.is_none() {
        problems.push(Problem::new(
            "Claude Code isn't where the app can find it. Install it, or say where it is in the team's settings.",
        ));
    } else if found.logged_in == Some(false) {
        problems.push(Problem::new(
            "Claude Code isn't logged in. Run `claude auth login` in a terminal.",
        ));
    }
    let uses_lab = team.agents.iter().any(|a| team.uses_lab(&a.id));
    if uses_lab && found.lab.is_none() {
        problems.push(Problem::new(
            "Lab isn't built. Run `cargo build --release -p persephone-lab`, or say where it is on the Lab card.",
        ));
    }
    problems
}

fn agents(team: &Team, problems: &mut Vec<Problem>) {
    let mut by_key: HashMap<String, &str> = HashMap::new();
    for agent in &team.agents {
        let id = agent.id.as_str();
        let name = agent.name.trim();
        if key(name).is_empty() {
            problems.push(
                Problem::new("An agent needs a name, with at least one letter or digit.")
                    .about(&[id]),
            );
        } else if let Some(other) = by_key.insert(key(name), id) {
            problems.push(
                Problem::new(format!(
                    "Two agents are called \"{name}\". Give each its own name."
                ))
                .about(&[other, id]),
            );
        }
        if agent.role.trim().is_empty() {
            problems.push(Problem::new(format!("{name}: describe its role.")).about(&[id]));
        }
        if agent.budget == 0 {
            problems.push(
                Problem::new(format!("{name}: give it a budget of at least 1 tool call."))
                    .about(&[id]),
            );
        }
        if team.is_root(id) && (agent.spend_cap.is_nan() || agent.spend_cap <= 0.0) {
            problems.push(
                Problem::new(format!("{name}: give its sessions a spend cap above $0."))
                    .about(&[id]),
            );
        }
    }
}

/// Says whether every link is well formed, so the rest can be looked at.
fn links(team: &Team, problems: &mut Vec<Problem>) -> bool {
    let before = problems.len();
    let mut seen: HashSet<(&str, &str, std::mem::Discriminant<LinkKind>)> = HashSet::new();
    for (i, link) in team.links.iter().enumerate() {
        let (from, to) = (link.from.as_str(), link.to.as_str());
        let (a, b) = (team.name_of(from), team.name_of(to));
        let ends_exist = team.agent(from).is_some() && (to == LAB || team.agent(to).is_some());
        if !ends_exist {
            problems.push(Problem::new("A link goes to a card that's gone.").link(i));
            continue;
        }
        if from == to {
            problems.push(
                Problem::new(format!("{a} is linked to itself."))
                    .about(&[from])
                    .link(i),
            );
            continue;
        }
        let to_lab = to == LAB;
        if link.kind == LinkKind::Uses && !to_lab {
            problems.push(
                Problem::new(format!("{a} \"uses\" {b}: a Uses link goes to Lab."))
                    .about(&[from, to])
                    .link(i),
            );
        } else if link.kind != LinkKind::Uses && to_lab {
            problems.push(
                Problem::new(format!("{a}'s link to Lab must be a Uses link."))
                    .about(&[from])
                    .link(i),
            );
        }
        if let LinkKind::Reviews { rounds } = link.kind
            && !(1..=5).contains(&rounds)
        {
            problems.push(
                Problem::new(format!(
                    "{a} reviews {b} for {rounds} rounds: it can be 1 to 5."
                ))
                .about(&[from, to])
                .link(i),
            );
        }
        if !seen.insert((from, to, std::mem::discriminant(&link.kind))) {
            problems.push(
                Problem::new(format!("{a} and {b} are linked twice the same way."))
                    .about(&[from, to])
                    .link(i),
            );
        }
    }
    problems.len() == before
}

fn hierarchy(team: &Team, problems: &mut Vec<Problem>) {
    // One lead each.
    let mut leads: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, link) in team.links.iter().enumerate() {
        if link.kind == LinkKind::ReportsTo {
            leads.entry(link.from.as_str()).or_default().push(i);
        }
    }
    let mut two_leads = false;
    for agent in &team.agents {
        if let Some(several) = leads.get(agent.id.as_str())
            && several.len() > 1
        {
            two_leads = true;
            let names: Vec<&str> = several
                .iter()
                .map(|&i| team.name_of(&team.links[i].to))
                .collect();
            let mut problem = Problem::new(format!(
                "{} reports to {}: it can have one lead.",
                agent.name,
                list(&names)
            ))
            .about(&[&agent.id]);
            problem.links.clone_from(several);
            problems.push(problem);
        }
    }
    if two_leads {
        return;
    }
    // No loop of leads.
    let mut in_loop = HashSet::new();
    for agent in &team.agents {
        let mut path = vec![agent.id.as_str()];
        let mut at = agent.id.as_str();
        while let Some(lead) = team.lead_of(at) {
            if let Some(start) = path.iter().position(|p| *p == lead) {
                let circle = &path[start..];
                if circle.iter().all(|id| in_loop.insert(*id)) {
                    let names: Vec<&str> = circle.iter().map(|id| team.name_of(id)).collect();
                    problems.push(
                        Problem::new(format!(
                            "{} report to each other in a circle: someone has to be at the top.",
                            list(&names)
                        ))
                        .about(circle),
                    );
                }
                break;
            }
            path.push(lead);
            at = lead;
        }
    }
    if !in_loop.is_empty() {
        return;
    }
    // Hand-offs and reviews join agents at the top, or agents with the same lead.
    let mut pairs: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
    for (i, link) in team.links.iter().enumerate() {
        let (from, to) = (link.from.as_str(), link.to.as_str());
        if !matches!(link.kind, LinkKind::HandsTo | LinkKind::Reviews { .. }) {
            continue;
        }
        let (a, b) = (team.name_of(from), team.name_of(to));
        let together = match (team.lead_of(from), team.lead_of(to)) {
            (None, None) => true,
            (Some(x), Some(y)) => x == y,
            _ => false,
        };
        if !together {
            let verb = if link.kind == LinkKind::HandsTo {
                "hands to"
            } else {
                "reviews"
            };
            problems.push(
                Problem::new(format!(
                    "{a} {verb} {b}, but they're in different places. Link agents with the same lead, or agents that report to no one."
                ))
                .about(&[from, to])
                .link(i),
            );
        }
        let pair = if from < to { (from, to) } else { (to, from) };
        pairs.entry(pair).or_default().push(i);
    }
    for ((x, y), indices) in &pairs {
        let kinds: Vec<LinkKind> = indices.iter().map(|&i| team.links[i].kind).collect();
        let reviews = kinds
            .iter()
            .filter(|k| matches!(k, LinkKind::Reviews { .. }))
            .count();
        let hands = kinds.len() - reviews;
        let (a, b) = (team.name_of(x), team.name_of(y));
        let message = if reviews > 1 {
            Some(format!("{a} and {b} review each other: keep one."))
        } else if reviews == 1 && hands > 0 {
            Some(format!(
                "{a} and {b} are joined by both a hand-off and a review: keep one."
            ))
        } else {
            None
        };
        if let Some(message) = message {
            let mut problem = Problem::new(message).about(&[x, y]);
            problem.links.clone_from(indices);
            problems.push(problem);
        }
    }
    // Between sessions, a reviewer isn't reviewed itself.
    for (i, link) in team.links.iter().enumerate() {
        if !matches!(link.kind, LinkKind::Reviews { .. })
            || !team.is_root(&link.from)
            || !team.is_root(&link.to)
        {
            continue;
        }
        let reviewed = team
            .links
            .iter()
            .any(|other| matches!(other.kind, LinkKind::Reviews { .. }) && other.to == link.from);
        if reviewed {
            let (c, b) = (team.name_of(&link.from), team.name_of(&link.to));
            problems.push(
                Problem::new(format!(
                    "{c} reviews {b} and is reviewed itself: a reviewer can't be reviewed too."
                ))
                .about(&[&link.from])
                .link(i),
            );
        }
    }
}

/// `a`, `a and b`, `a, b and c`.
fn list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::{Link, Starter};

    fn team(starter: Starter) -> Team {
        let mut team = Team::new("T", starter);
        team.brief = "Study BTC.".into();
        team
    }

    fn link(team: &mut Team, from: usize, to: usize, kind: LinkKind) {
        let (from, to) = (team.agents[from].id.clone(), team.agents[to].id.clone());
        team.links.push(Link { from, to, kind });
    }

    fn says(problems: &[Problem], words: &str) -> bool {
        problems.iter().any(|p| p.what.contains(words))
    }

    #[test]
    fn the_starters_can_run() {
        for starter in [
            Starter::OneAnalyst,
            Starter::LeadAndAnalysts,
            Starter::ResearcherAndReviewer,
        ] {
            assert_eq!(check(&team(starter)), vec![], "{starter}");
        }
    }

    #[test]
    fn an_empty_team_or_brief_is_caught() {
        assert!(says(&check(&team(Starter::Blank)), "Add an agent"));
        let mut solo = team(Starter::OneAnalyst);
        solo.brief = "  ".into();
        assert!(says(&check(&solo), "brief is empty"));
    }

    #[test]
    fn names_roles_budgets_and_caps_are_needed() {
        let mut t = team(Starter::LeadAndAnalysts);
        t.agents[1].name = "Analyst A".into();
        t.agents[2].name = "analyst a!".into();
        t.agents[0].role.clear();
        t.agents[1].budget = 0;
        t.agents[0].spend_cap = 0.0;
        // A member's cap isn't used, so it isn't asked for.
        t.agents[2].spend_cap = 0.0;
        let problems = check(&t);
        assert!(says(&problems, "Two agents are called"), "{problems:?}");
        assert!(says(&problems, "Lead: describe its role"));
        assert!(says(&problems, "budget of at least 1"));
        assert_eq!(
            problems
                .iter()
                .filter(|p| p.what.contains("spend cap"))
                .count(),
            1
        );
        let named = problems
            .iter()
            .find(|p| p.what.contains("Two agents"))
            .unwrap();
        assert_eq!(named.agents.len(), 2);
    }

    #[test]
    fn links_must_make_sense() {
        let mut t = team(Starter::ResearcherAndReviewer);
        link(&mut t, 0, 0, LinkKind::HandsTo);
        let uses_agent = Link {
            from: t.agents[0].id.clone(),
            to: t.agents[1].id.clone(),
            kind: LinkKind::Uses,
        };
        t.links.push(uses_agent);
        t.links.push(Link {
            from: t.agents[1].id.clone(),
            to: LAB.into(),
            kind: LinkKind::HandsTo,
        });
        let problems = check(&t);
        assert!(says(&problems, "linked to itself"), "{problems:?}");
        assert!(says(&problems, "a Uses link goes to Lab"));
        assert!(says(&problems, "link to Lab must be a Uses link"));
    }

    #[test]
    fn rounds_are_one_to_five() {
        let mut t = team(Starter::ResearcherAndReviewer);
        for link in &mut t.links {
            if let LinkKind::Reviews { rounds } = &mut link.kind {
                *rounds = 6;
            }
        }
        assert!(says(&check(&t), "it can be 1 to 5"));
    }

    #[test]
    fn an_agent_has_one_lead_and_leads_dont_loop() {
        let mut t = team(Starter::LeadAndAnalysts);
        link(&mut t, 1, 2, LinkKind::ReportsTo);
        assert!(says(&check(&t), "it can have one lead"));

        let mut t = team(Starter::LeadAndAnalysts);
        link(&mut t, 0, 1, LinkKind::ReportsTo);
        let problems = check(&t);
        assert!(says(&problems, "in a circle"), "{problems:?}");
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn hand_offs_join_the_top_or_the_same_lead() {
        let mut t = team(Starter::LeadAndAnalysts);
        // Between two analysts of one lead: fine.
        link(&mut t, 1, 2, LinkKind::HandsTo);
        assert_eq!(check(&t), vec![]);
        // From the lead down to its own analyst: not.
        link(&mut t, 0, 1, LinkKind::HandsTo);
        assert!(says(&check(&t), "in different places"));
    }

    #[test]
    fn a_pair_has_one_hand_off_or_review() {
        let mut t = team(Starter::ResearcherAndReviewer);
        link(&mut t, 1, 0, LinkKind::HandsTo);
        assert!(says(&check(&t), "both a hand-off and a review"));
        let mut t = team(Starter::ResearcherAndReviewer);
        link(&mut t, 0, 1, LinkKind::Reviews { rounds: 1 });
        assert!(says(&check(&t), "review each other"));
    }

    #[test]
    fn a_reviewer_isnt_reviewed() {
        let mut t = team(Starter::ResearcherAndReviewer);
        t.add_agent([0.0, 200.0]);
        let third = t.agents.len() - 1;
        t.agents[third].role = "Checks the reviewer.".into();
        link(&mut t, third, 1, LinkKind::Reviews { rounds: 1 });
        assert!(says(&check(&t), "can't be reviewed too"));
    }

    #[test]
    fn hand_offs_in_a_circle_are_caught() {
        let mut t = team(Starter::ResearcherAndReviewer);
        t.links
            .retain(|l| !matches!(l.kind, LinkKind::Reviews { .. }));
        link(&mut t, 0, 1, LinkKind::HandsTo);
        link(&mut t, 1, 0, LinkKind::HandsTo);
        let problems = check(&t);
        assert!(says(&problems, "could never finish"), "{problems:?}");
        assert_eq!(problems[0].agents.len(), 2);
    }

    #[test]
    fn what_this_computer_lacks_is_said() {
        let t = team(Starter::OneAnalyst);
        let none = check_setup(&t, &Found::default());
        assert!(says(&none, "Claude Code isn't where"));
        assert!(says(&none, "Lab isn't built"));
        let logged_out = Found {
            claude: Some("claude".into()),
            logged_in: Some(false),
            lab: Some("lab".into()),
        };
        assert!(says(&check_setup(&t, &logged_out), "claude auth login"));
    }
}
