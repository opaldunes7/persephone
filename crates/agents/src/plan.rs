//! A design turned into what runs: one Claude Code session for each agent that reports to no
//! one, with its team as sub-agents, and the order the sessions' steps go in.
//!
//! Links between agents of one session become what their lead is told. Links between sessions
//! become steps: a hand-off starts the receiver after the giver, with the giver's report in its
//! inbox, and a review runs the reviewer on the author's report and then the author on the
//! review, round by round.

use std::collections::{HashMap, VecDeque};

use crate::team::{LinkKind, Team, key};

/// What a run does, in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub sessions: Vec<Session>,
    /// Each in an order it can run in: every step comes after those it waits on.
    pub steps: Vec<Step>,
}

/// An agent at the top, and everyone under it, run as one Claude Code session in its folder.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// The agent at the top, by id.
    pub root: String,
    /// Its name in Claude Code, and its folder's in a run's records.
    pub key: String,
    /// Everyone under it, each lead before its own team.
    pub members: Vec<Member>,
    /// How many levels its team goes down: 0 with no members.
    pub depth: u32,
    /// Whether anyone in it uses Lab, which the whole session then reaches.
    pub lab: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub id: String,
    /// Its name in `--agents`, and what its lead calls it by.
    pub key: String,
    /// Its lead's id.
    pub lead: String,
    /// How far down: 1 for the root's own team.
    pub depth: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    /// Its own work on the brief.
    Work,
    /// Reviewing another session's report, by its index, in this round.
    Review { author: usize, round: u32 },
    /// Answering its reviews of this round.
    Answer { round: u32 },
}

/// One session of one agent.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub session: usize,
    pub kind: StepKind,
    /// Steps that must have finished well before this one starts.
    pub after: Vec<usize>,
    /// Reports it's given, copied into its inbox.
    pub inputs: Vec<Input>,
    /// The earlier step of the same agent whose conversation it carries on.
    pub resumes: Option<usize>,
}

/// Another step's report, for this one's inbox.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub step: usize,
    /// Its folder in `inbox/<run>/`.
    pub folder: String,
    /// Who it's from, by agent id.
    pub from: String,
}

impl Plan {
    /// The agent whose folder and conversation a step is.
    #[must_use]
    pub fn agent_of(&self, step: usize) -> &str {
        &self.sessions[self.steps[step].session].root
    }

    /// What a step is, in a few words, as a commit message and a run's records say it.
    #[must_use]
    pub fn label(&self, team: &Team, step: usize) -> String {
        let name = team.name_of(self.agent_of(step));
        match self.steps[step].kind {
            StepKind::Work => format!("{name}: work"),
            StepKind::Review { author, round } => format!(
                "{name}: review of {}, round {round}",
                team.name_of(&self.sessions[author].root)
            ),
            StepKind::Answer { round } => format!("{name}: answer, round {round}"),
        }
    }
}

/// The sessions and steps of a design that [`check`](crate::check::check) passed.
///
/// # Errors
///
/// Says which agents wait on each other in a circle, by id, should the design have one.
pub fn plan(team: &Team) -> Result<Plan, Vec<String>> {
    Ok(Plan {
        sessions: sessions(team),
        steps: steps(team)?,
    })
}

/// One session for each agent that reports to no one, in the order their cards are listed.
#[must_use]
pub fn sessions(team: &Team) -> Vec<Session> {
    team.agents
        .iter()
        .filter(|a| team.is_root(&a.id))
        .map(|root| {
            let mut members = Vec::new();
            gather(team, &root.id, 1, &mut members);
            Session {
                root: root.id.clone(),
                key: key(&root.name),
                depth: members.iter().map(|m| m.depth).max().unwrap_or(0),
                lab: team.session_uses_lab(&root.id),
                members,
            }
        })
        .collect()
}

fn gather(team: &Team, lead: &str, depth: u32, members: &mut Vec<Member>) {
    // A loop of leads never reaches here once checked; the depth bounds it anyway.
    if depth as usize > team.agents.len() {
        return;
    }
    for report in team.reports_of(lead) {
        members.push(Member {
            id: report.id.clone(),
            key: key(&report.name),
            lead: lead.to_owned(),
            depth,
        });
        gather(team, &report.id, depth + 1, members);
    }
}

/// A step before it has its number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Sym {
    Work(usize),
    Review(usize, usize, u32),
    Answer(usize, u32),
}

/// The steps, in an order they can run in.
///
/// # Errors
///
/// The agents, by id, whose steps wait on each other in a circle.
pub fn steps(team: &Team) -> Result<Vec<Step>, Vec<String>> {
    let sessions = sessions(team);
    let index: HashMap<&str, usize> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| (s.root.as_str(), i))
        .collect();
    let top = |from: &str, to: &str| Some((*index.get(from)?, *index.get(to)?));
    let mut hands = Vec::new();
    let mut reviews = Vec::new();
    for link in &team.links {
        match (link.kind, top(&link.from, &link.to)) {
            (LinkKind::HandsTo, Some((a, b))) => hands.push((a, b)),
            (LinkKind::Reviews { rounds }, Some((c, b))) => reviews.push((c, b, rounds.max(1))),
            _ => {}
        }
    }

    // Each agent's own steps, in the order it does them.
    let mut own: Vec<Vec<Sym>> = vec![Vec::new(); sessions.len()];
    for (s, steps) in own.iter_mut().enumerate() {
        let reviewing: Vec<(usize, u32)> = reviews
            .iter()
            .filter(|(c, _, _)| *c == s)
            .map(|&(_, b, n)| (b, n))
            .collect();
        if reviewing.is_empty() {
            steps.push(Sym::Work(s));
        }
        let most = reviewing.iter().map(|&(_, n)| n).max().unwrap_or(0);
        for round in 1..=most {
            for &(b, n) in &reviewing {
                if round <= n {
                    steps.push(Sym::Review(s, b, round));
                }
            }
        }
        let answers = reviews
            .iter()
            .filter(|(_, b, _)| *b == s)
            .map(|&(_, _, n)| n)
            .max()
            .unwrap_or(0);
        for round in 1..=answers {
            steps.push(Sym::Answer(s, round));
        }
    }
    let last = |s: usize| *own[s].last().expect("every session has a step");
    // An author's report as of a round: its work, then each answer.
    let stage = |b: usize, round: u32| {
        if round == 0 {
            Sym::Work(b)
        } else {
            Sym::Answer(b, round)
        }
    };
    let name = |s: usize| team.name_of(&sessions[s].root);

    struct Draft {
        sym: Sym,
        session: usize,
        after: Vec<Sym>,
        inputs: Vec<(Sym, String, usize)>,
        resumes: Option<Sym>,
    }
    let mut drafts = Vec::new();
    for (s, steps) in own.iter().enumerate() {
        for (n, &sym) in steps.iter().enumerate() {
            let mut after: Vec<Sym> = n.checked_sub(1).map(|p| steps[p]).into_iter().collect();
            let mut inputs = Vec::new();
            let mut resumes = None;
            // What's handed to it comes in once, at its first step.
            if n == 0 {
                for &(a, _) in hands.iter().filter(|(_, to)| *to == s) {
                    after.push(last(a));
                    inputs.push((last(a), key(name(a)), a));
                }
            }
            match sym {
                Sym::Work(_) => {}
                Sym::Review(_, b, round) => {
                    after.push(stage(b, round - 1));
                    inputs.push((
                        stage(b, round - 1),
                        format!("{}-round-{round}", key(name(b))),
                        b,
                    ));
                    if round > 1 {
                        resumes = Some(Sym::Review(s, b, round - 1));
                    }
                }
                Sym::Answer(_, round) => {
                    for &(c, _, rounds) in reviews.iter().filter(|(_, b, _)| *b == s) {
                        if round <= rounds {
                            let review = Sym::Review(c, s, round);
                            after.push(review);
                            inputs.push((review, format!("{}-round-{round}", key(name(c))), c));
                        }
                    }
                    resumes = Some(stage(s, round - 1));
                }
            }
            drafts.push(Draft {
                sym,
                session: s,
                after,
                inputs,
                resumes,
            });
        }
    }

    // In an order they can run in, by Kahn's method; what's left over is a circle.
    let place: HashMap<Sym, usize> = drafts.iter().enumerate().map(|(i, d)| (d.sym, i)).collect();
    let mut waiting_on: Vec<usize> = drafts.iter().map(|d| d.after.len()).collect();
    let mut freed: Vec<Vec<usize>> = vec![Vec::new(); drafts.len()];
    for (i, draft) in drafts.iter().enumerate() {
        for sym in &draft.after {
            freed[place[sym]].push(i);
        }
    }
    let mut ready: VecDeque<usize> = (0..drafts.len()).filter(|&i| waiting_on[i] == 0).collect();
    let mut order = Vec::with_capacity(drafts.len());
    while let Some(i) = ready.pop_front() {
        order.push(i);
        for &next in &freed[i] {
            waiting_on[next] -= 1;
            if waiting_on[next] == 0 {
                ready.push_back(next);
            }
        }
    }
    if order.len() < drafts.len() {
        let mut stuck: Vec<String> = Vec::new();
        for (i, draft) in drafts.iter().enumerate() {
            let root = &sessions[draft.session].root;
            if waiting_on[i] > 0 && !stuck.contains(root) {
                stuck.push(root.clone());
            }
        }
        return Err(stuck);
    }
    let number: HashMap<Sym, usize> = order
        .iter()
        .enumerate()
        .map(|(n, &i)| (drafts[i].sym, n))
        .collect();
    Ok(order
        .iter()
        .map(|&i| {
            let draft = &drafts[i];
            let kind = match draft.sym {
                Sym::Work(_) => StepKind::Work,
                Sym::Review(_, author, round) => StepKind::Review { author, round },
                Sym::Answer(_, round) => StepKind::Answer { round },
            };
            let mut after: Vec<usize> = draft.after.iter().map(|sym| number[sym]).collect();
            after.sort_unstable();
            after.dedup();
            Step {
                session: draft.session,
                kind,
                after,
                inputs: draft
                    .inputs
                    .iter()
                    .map(|(sym, folder, from)| Input {
                        step: number[sym],
                        folder: folder.clone(),
                        from: sessions[*from].root.clone(),
                    })
                    .collect(),
                resumes: draft.resumes.map(|sym| number[&sym]),
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::team::{Link, Starter};

    fn link(team: &mut Team, from: usize, to: usize, kind: LinkKind) {
        let (from, to) = (team.agents[from].id.clone(), team.agents[to].id.clone());
        team.links.push(Link { from, to, kind });
    }

    #[test]
    fn a_lead_and_its_team_are_one_session() {
        let team = Team::new("Desk", Starter::LeadAndAnalysts);
        let plan = plan(&team).unwrap();
        assert_eq!(plan.sessions.len(), 1);
        let session = &plan.sessions[0];
        assert_eq!(session.key, "lead");
        let keys: Vec<&str> = session.members.iter().map(|m| m.key.as_str()).collect();
        assert_eq!(keys, ["analyst-a", "analyst-b"]);
        assert_eq!(session.depth, 1);
        assert!(session.lab);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].kind, StepKind::Work);
    }

    #[test]
    fn teams_nest() {
        let mut team = Team::new("Desk", Starter::LeadAndAnalysts);
        team.add_agent([0.0, 200.0]);
        link(&mut team, 3, 1, LinkKind::ReportsTo);
        let session = &sessions(&team)[0];
        assert_eq!(session.depth, 2);
        let order: Vec<(&str, u32)> = session
            .members
            .iter()
            .map(|m| (m.key.as_str(), m.depth))
            .collect();
        assert_eq!(order, [("analyst-a", 1), ("agent-4", 2), ("analyst-b", 1)]);
    }

    #[test]
    fn a_hand_off_orders_sessions_and_fills_the_inbox() {
        let mut team = Team::new("Line", Starter::OneAnalyst);
        team.add_agent([200.0, 0.0]);
        team.agents[1].name = "Writer".into();
        link(&mut team, 0, 1, LinkKind::HandsTo);
        let plan = plan(&team).unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.agent_of(0), team.agents[0].id);
        let second = &plan.steps[1];
        assert_eq!(second.after, [0]);
        assert_eq!(second.inputs.len(), 1);
        assert_eq!(second.inputs[0].folder, "analyst");
        assert_eq!(second.inputs[0].from, team.agents[0].id);
    }

    #[test]
    fn reviews_unroll_into_rounds() {
        let mut team = Team::new("Pair", Starter::ResearcherAndReviewer);
        for link in &mut team.links {
            if let LinkKind::Reviews { rounds } = &mut link.kind {
                *rounds = 2;
            }
        }
        let plan = plan(&team).unwrap();
        let labels: Vec<String> = (0..plan.steps.len())
            .map(|s| plan.label(&team, s))
            .collect();
        assert_eq!(
            labels,
            [
                "Researcher: work",
                "Reviewer: review of Researcher, round 1",
                "Researcher: answer, round 1",
                "Reviewer: review of Researcher, round 2",
                "Researcher: answer, round 2",
            ]
        );
        // Each step waits on the one before, and carries on its agent's conversation.
        assert_eq!(plan.steps[1].after, [0]);
        assert_eq!(plan.steps[2].after, [0, 1]);
        assert_eq!(plan.steps[2].resumes, Some(0));
        assert_eq!(plan.steps[3].after, [1, 2]);
        assert_eq!(plan.steps[3].resumes, Some(1));
        assert_eq!(plan.steps[3].inputs[0].folder, "researcher-round-2");
        assert_eq!(plan.steps[4].inputs[0].folder, "reviewer-round-2");
    }

    #[test]
    fn two_reviewers_review_each_round_before_the_answer() {
        let mut team = Team::new("Pair", Starter::ResearcherAndReviewer);
        team.add_agent([0.0, 200.0]);
        link(&mut team, 2, 0, LinkKind::Reviews { rounds: 1 });
        let plan = plan(&team).unwrap();
        let answer = plan
            .steps
            .iter()
            .position(|s| s.kind == StepKind::Answer { round: 1 })
            .unwrap();
        let reviews: Vec<usize> = (0..plan.steps.len())
            .filter(|&s| matches!(plan.steps[s].kind, StepKind::Review { .. }))
            .collect();
        assert_eq!(reviews.len(), 2);
        assert!(reviews.iter().all(|r| plan.steps[answer].after.contains(r)));
        assert_eq!(plan.steps[answer].inputs.len(), 2);
        assert_eq!(answer, plan.steps.len() - 1);
    }

    #[test]
    fn what_a_reviewed_agent_hands_on_waits_for_its_last_answer() {
        let mut team = Team::new("Pair", Starter::ResearcherAndReviewer);
        team.add_agent([0.0, 200.0]);
        link(&mut team, 0, 2, LinkKind::HandsTo);
        let plan = plan(&team).unwrap();
        let last = plan.steps.len() - 1;
        assert_eq!(plan.agent_of(last), team.agents[2].id);
        let answer = plan
            .steps
            .iter()
            .position(|s| s.kind == StepKind::Answer { round: 1 })
            .unwrap();
        assert_eq!(plan.steps[last].after, [answer]);
    }

    #[test]
    fn links_inside_a_session_make_no_steps() {
        let mut team = Team::new("Desk", Starter::LeadAndAnalysts);
        link(&mut team, 1, 2, LinkKind::HandsTo);
        assert_eq!(steps(&team).unwrap().len(), 1);
    }

    #[test]
    fn a_circle_is_found() {
        let mut team = Team::new("Line", Starter::OneAnalyst);
        team.add_agent([200.0, 0.0]);
        link(&mut team, 0, 1, LinkKind::HandsTo);
        link(&mut team, 1, 0, LinkKind::HandsTo);
        let stuck = steps(&team).unwrap_err();
        assert_eq!(stuck.len(), 2);
    }
}
