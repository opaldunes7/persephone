//! How the Agents view looks: the teams down the left; across the top, the run bar; the canvas
//! with its toolbar; and down the right, the inspector, with the selection's setup, the run's
//! activity, an agent's files, and every agent's feedback on the app.

use iced::{
    Alignment, Background, Border, Color, Element, Fill,
    widget::{
        Space, button, canvas, checkbox, column, container, pick_list, row, scrollable, text,
        text_editor, text_input,
    },
};
use persephone_agents::{
    check::Problem,
    stream::{Event, Who},
    team::{Effort, LAB, LabData, LinkKind, Model, Starter, Team},
};
use persephone_chart::theme;

use super::{
    Agents, AgentsMessage, BUDGET, Line, SPEND, Selected, Tab,
    board::{Board, status_color},
};
use crate::Message;

const SMALL: f32 = 12.0;
const HINT: f32 = 11.0;
const TEAMS_WIDTH: f32 = 190.0;
const INSPECTOR_WIDTH: f32 = 400.0;
const HOVER: Color = Color::from_rgb8(0x2a, 0x2e, 0x39);
const EDGE: Color = Color::from_rgb8(0x36, 0x3a, 0x45);
/// The most activity lines shown, the newest.
const MOST_LINES: usize = 400;

fn say(message: AgentsMessage) -> Message {
    Message::Agents(message)
}

fn chip_style(on: bool) -> impl Fn(&iced::Theme, button::Status) -> button::Style {
    move |_, status| {
        let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let off = matches!(status, button::Status::Disabled);
        button::Style {
            background: Some(Background::Color(if on {
                theme::ACCENT
            } else if hot {
                HOVER
            } else {
                Color::TRANSPARENT
            })),
            text_color: if on {
                Color::WHITE
            } else if off {
                theme::TEXT_MUTED
            } else {
                theme::TEXT_BRIGHT
            },
            border: Border {
                color: if on { theme::ACCENT } else { EDGE },
                width: 1.0,
                radius: 4.0.into(),
            },
            ..button::Style::default()
        }
    }
}

/// A small button; `None` for one that can't be pressed now.
fn chip<'a>(
    label: impl Into<String>,
    on: bool,
    message: Option<AgentsMessage>,
) -> Element<'a, Message> {
    button(text(label.into()).size(HINT).center())
        .padding([3, 8])
        .on_press_maybe(message.map(say))
        .style(chip_style(on))
        .into()
}

fn heading<'a>(said: impl Into<String>) -> Element<'a, Message> {
    text(said.into())
        .size(SMALL)
        .color(theme::TEXT_BRIGHT)
        .into()
}

fn hint<'a>(said: impl Into<String>) -> Element<'a, Message> {
    text(said.into()).size(HINT).color(theme::TEXT_MUTED).into()
}

/// A setting: its name above its control.
fn field<'a>(name: &str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![
        text(name.to_owned()).size(HINT).color(theme::TEXT_MUTED),
        control.into()
    ]
    .spacing(3)
    .into()
}

fn input<'a>(
    placeholder: &str,
    value: &str,
    on: impl Fn(String) -> AgentsMessage + 'a,
) -> Element<'a, Message> {
    text_input(placeholder, value)
        .size(SMALL)
        .padding([4, 6])
        .on_input(move |typed| say(on(typed)))
        .into()
}

fn editor<'a>(
    content: &'a text_editor::Content,
    height: f32,
    on: impl Fn(text_editor::Action) -> AgentsMessage + 'a,
) -> Element<'a, Message> {
    text_editor(content)
        .size(SMALL)
        .height(height)
        .padding(6)
        .on_action(move |action| say(on(action)))
        .into()
}

fn panel<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(8)
        .style(|_| container::Style {
            background: Some(Background::Color(theme::BACKGROUND)),
            border: Border {
                color: theme::AXIS_LINE,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// The top bar's button that opens the view, lit while it's open.
pub fn top_button<'a>(open: bool) -> Element<'a, Message> {
    iced::widget::tooltip(
        chip("Agents", open, Some(AgentsMessage::Toggle)),
        container(
            text("Teams of Claude agents: design them, run them and watch them work (Ctrl+G).")
                .size(HINT),
        )
        .padding(6)
        .style(container::rounded_box),
        iced::widget::tooltip::Position::Bottom,
    )
    .into()
}

/// The whole view.
pub fn view(agents: &Agents) -> Element<'_, Message> {
    let main: Element<'_, Message> = match agents.team() {
        Some(team) => column![
            run_bar(agents, team),
            row![canvas_area(agents, team), inspector(agents, team)]
                .spacing(6)
                .height(Fill),
        ]
        .spacing(6)
        .into(),
        None => container(
            column![
                heading("No teams yet"),
                hint(format!(
                    "Start one from the left. Teams are kept in {}.",
                    agents.root().display()
                )),
            ]
            .spacing(6),
        )
        .center(Fill)
        .into(),
    };
    row![teams(agents), main]
        .spacing(6)
        .padding(iced::Padding {
            top: 0.0,
            ..iced::Padding::new(6.0)
        })
        .width(Fill)
        .height(Fill)
        .into()
}

fn teams(agents: &Agents) -> Element<'_, Message> {
    let current = agents.saved.team.as_deref();
    let mut list = column![
        row![
            heading("Teams"),
            Space::new().width(Fill),
            chip("✕", false, Some(AgentsMessage::Toggle))
        ]
        .align_y(Alignment::Center)
    ]
    .spacing(4);
    for team in &agents.teams {
        let on = current == Some(team.id.as_str());
        list = list.push(
            button(text(team.name.clone()).size(SMALL))
                .width(Fill)
                .padding([4, 8])
                .on_press(say(AgentsMessage::Pick(team.id.clone())))
                .style(chip_style(on)),
        );
    }
    list = list.push(Space::new().height(10)).push(hint("New team"));
    for starter in Starter::ALL {
        list = list.push(
            button(text(starter.label()).size(HINT))
                .width(Fill)
                .padding([3, 8])
                .on_press(say(AgentsMessage::New(starter)))
                .style(chip_style(false)),
        );
    }
    if agents.team().is_some() {
        list = list.push(Space::new().height(10)).push(
            row![
                chip("Duplicate", false, Some(AgentsMessage::Duplicate)),
                chip("Archive", false, Some(AgentsMessage::Archive)),
            ]
            .spacing(4),
        );
    }
    if let Some(note) = &agents.note {
        list = list
            .push(Space::new().height(10))
            .push(text(note.clone()).size(HINT).color(theme::TEXT));
    }
    container(scrollable(list).height(Fill))
        .width(TEAMS_WIDTH)
        .height(Fill)
        .into()
}

fn run_bar<'a>(agents: &'a Agents, team: &'a Team) -> Element<'a, Message> {
    let live = agents.live();
    let running = agents.running();
    let mut bar = row![heading(team.name.clone())]
        .spacing(6)
        .align_y(Alignment::Center);
    bar = bar.push(chip("Check", false, Some(AgentsMessage::Check)));
    let most = agents.most_spend().map_or_else(
        || "Run".to_owned(),
        |most| format!("Run · at most ${most:.2}"),
    );
    let can_run = !running && agents.found.as_ref().is_some_and(|f| f.claude.is_some());
    bar = bar.push(chip(most, false, can_run.then_some(AgentsMessage::Run)));
    if running {
        bar = bar
            .push(chip("Wrap up", false, Some(AgentsMessage::WrapUp)))
            .push(chip("Stop", false, Some(AgentsMessage::Stop)));
    }
    if let Some(live) = live {
        let elapsed = live.started.elapsed().as_secs();
        let state = match (&live.ended, &live.failed) {
            (Some(summary), _) => format!(
                "Run {} ended · ${:.2} · {} of {} steps done",
                summary.run,
                summary.spend,
                summary.steps.iter().filter(|s| s.ok).count(),
                summary.steps.len()
            ),
            (None, Some(why)) => format!("Couldn't run: {why}"),
            (None, None) => format!(
                "Running {} · ${:.2} so far · {}:{:02}",
                live.run.as_deref().unwrap_or("…"),
                live.spend(),
                elapsed / 60,
                elapsed % 60
            ),
        };
        bar = bar.push(text(state).size(HINT).color(theme::TEXT));
    } else if agents.looking {
        bar = bar.push(hint("Looking for Claude Code…"));
    } else if let Some(found) = &agents.found {
        let said = match (&found.claude, found.logged_in) {
            (None, _) => "Claude Code wasn't found".to_owned(),
            (Some(_), Some(false)) => "Claude Code isn't logged in".to_owned(),
            (Some(_), _) => "Spend is at API prices, as Claude Code counts it".to_owned(),
        };
        bar = bar.push(hint(said));
    }
    bar = bar.push(Space::new().width(Fill));
    if !agents.past.is_empty() {
        let runs: Vec<String> = agents.past.iter().map(|s| s.run.clone()).collect();
        let picked = agents.viewing.as_ref().map(|(s, _)| s.run.clone());
        bar = bar.push(hint("Past runs")).push(
            pick_list(runs, picked, |run| say(AgentsMessage::PastRun(run)))
                .text_size(HINT)
                .padding([3, 6]),
        );
    }
    let mut top = column![bar].spacing(4);
    if !agents.problems.is_empty() {
        top = top.push(problems(&agents.problems));
    }
    panel(top)
}

fn problems(problems: &[Problem]) -> Element<'_, Message> {
    let mut list = column![].spacing(2);
    for problem in problems {
        let select = problem
            .agents
            .first()
            .map(|id| Selected::Agent(id.clone()))
            .or_else(|| problem.links.first().map(|&i| Selected::Link(i)));
        list = list.push(
            button(
                text(format!("• {}", problem.what))
                    .size(HINT)
                    .color(theme::DOWN),
            )
            .padding([1, 4])
            .on_press_maybe(select.map(|s| say(AgentsMessage::Select(s))))
            .style(|_, _| button::Style::default()),
        );
    }
    list.into()
}

fn canvas_area<'a>(agents: &'a Agents, team: &'a Team) -> Element<'a, Message> {
    let mut kinds = row![hint("Link as")].spacing(4).align_y(Alignment::Center);
    for kind in LinkKind::DRAWN {
        kinds = kinds.push(chip(
            kind.label(),
            agents.link_as.same_kind(kind),
            Some(AgentsMessage::LinkAs(kind)),
        ));
    }
    let removable = matches!(agents.selected, Selected::Agent(_) | Selected::Link(_));
    let toolbar = row![
        kinds,
        Space::new().width(Fill),
        chip(
            "+ Agent",
            false,
            Some(AgentsMessage::Add([team.view.at[0], team.view.at[1]]))
        ),
        chip(
            "Delete",
            false,
            (removable && !agents.running()).then_some(AgentsMessage::Remove)
        ),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    let flagged: Vec<String> = agents
        .problems
        .iter()
        .flat_map(|p| p.agents.iter().cloned())
        .collect();
    let live = agents.live();
    let board = canvas(Board {
        team,
        selected: &agents.selected,
        cards: live.map(|l| &l.cards),
        lab_calls: live.map_or(0, |l| l.lab_calls),
        flagged,
    })
    .width(Fill)
    .height(Fill);
    panel(
        column![
            toolbar,
            hint("Drag a card's dot to another card to link them. Double-click empty space to add an agent; drag it to move around, scroll to zoom."),
            board
        ]
        .spacing(4),
    )
}

fn inspector<'a>(agents: &'a Agents, team: &'a Team) -> Element<'a, Message> {
    let tabs = row![
        chip(
            "Setup",
            agents.tab == Tab::Setup,
            Some(AgentsMessage::Tab(Tab::Setup))
        ),
        chip(
            "Activity",
            agents.tab == Tab::Activity,
            Some(AgentsMessage::Tab(Tab::Activity))
        ),
        chip(
            "Outputs",
            agents.tab == Tab::Outputs,
            Some(AgentsMessage::Tab(Tab::Outputs))
        ),
        chip(
            "App feedback",
            agents.tab == Tab::Feedback,
            Some(AgentsMessage::Tab(Tab::Feedback))
        ),
    ]
    .spacing(4);
    let body: Element<'a, Message> = match agents.tab {
        Tab::Setup => scrollable(setup(agents, team)).height(Fill).into(),
        Tab::Activity => activity(agents, team),
        Tab::Outputs => scrollable(outputs(agents)).height(Fill).into(),
        Tab::Feedback => scrollable(feedback(agents)).height(Fill).into(),
    };
    container(panel(column![tabs, body].spacing(8)))
        .width(INSPECTOR_WIDTH)
        .height(Fill)
        .into()
}

fn setup<'a>(agents: &'a Agents, team: &'a Team) -> Element<'a, Message> {
    match &agents.selected {
        Selected::Agent(id) => match team.agent(id) {
            Some(agent) => agent_setup(agents, team, agent),
            None => team_setup(agents, team),
        },
        Selected::Link(index) => match team.links.get(*index) {
            Some(link) => link_setup(team, link),
            None => team_setup(agents, team),
        },
        Selected::Lab => lab_setup(agents, team),
        Selected::Team => team_setup(agents, team),
    }
}

fn agent_setup<'a>(
    agents: &'a Agents,
    team: &'a Team,
    agent: &'a persephone_agents::team::Agent,
) -> Element<'a, Message> {
    let member = !team.is_root(&agent.id);
    let budget = agents
        .typing
        .get(BUDGET)
        .cloned()
        .unwrap_or_else(|| agent.budget.to_string());
    let spend = agents
        .typing
        .get(SPEND)
        .cloned()
        .unwrap_or_else(|| format!("{:.2}", agent.spend_cap));
    let place = match team.lead_of(&agent.id) {
        Some(lead) => format!(
            "Reports to {}, in its session. It works on what its lead hands it.",
            team.name_of(lead)
        ),
        None if team.reports_of(&agent.id).is_empty() => {
            "A session of its own, with a folder and memory of its own.".to_owned()
        }
        None => format!(
            "Leads a session of its own: {} report to it.",
            team.reports_of(&agent.id)
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let own_lab = team.uses_lab(&agent.id);
    let mut form = column![
        heading(agent.name.clone()),
        hint(place),
        field("Name", input("Name", &agent.name, AgentsMessage::Name)),
        field(
            "Role: what it does, in its own words",
            editor(&agents.role, 150.0, AgentsMessage::Role)
        ),
        row![
            field(
                "Model",
                pick_list(Model::ALL, Some(agent.model), |m| say(
                    AgentsMessage::Model(m)
                ))
                .text_size(SMALL)
                .padding([4, 6])
            ),
            field(
                "Effort",
                pick_list(Effort::ALL, Some(agent.effort), |e| say(
                    AgentsMessage::Effort(e)
                ))
                .text_size(SMALL)
                .padding([4, 6])
            ),
        ]
        .spacing(8),
        field(
            "Tool calls per session",
            input("80", &budget, |typed| AgentsMessage::Number(BUDGET, typed))
        ),
    ]
    .spacing(8);
    if member {
        form = form.push(hint(
            "It spends from its lead's session, whose cap holds for all of it.",
        ));
    } else {
        form = form.push(field(
            "Spend cap per session, $ at API prices",
            input("10.00", &spend, |typed| AgentsMessage::Number(SPEND, typed)),
        ));
    }
    form = form
        .push(
            checkbox(own_lab)
                .label("Uses Lab")
                .text_size(SMALL)
                .on_toggle(|on| say(AgentsMessage::UsesLab(on))),
        )
        .push(
            checkbox(agent.memory)
                .label("Keeps a memory between runs")
                .text_size(SMALL)
                .on_toggle(|on| say(AgentsMessage::Memory(on))),
        )
        .push(
            checkbox(agent.app_notes)
                .label("Writes bugs and ideas for the app")
                .text_size(SMALL)
                .on_toggle(|on| say(AgentsMessage::AppNotes(on))),
        );
    if !own_lab && !member && team.session_uses_lab(&agent.id) {
        form = form.push(hint(
            "Its team uses Lab, so it can reach Lab too: a session shares one.",
        ));
    }
    form.into()
}

fn link_setup<'a>(team: &'a Team, link: &'a persephone_agents::team::Link) -> Element<'a, Message> {
    let (from, to) = (team.name_of(&link.from), team.name_of(&link.to));
    let what = match link.kind {
        LinkKind::ReportsTo => format!(
            "{from} reports to {to}: {to} hands it work and checks what it brings back. They run as one session."
        ),
        LinkKind::HandsTo => format!("{from} hands to {to}: {to} works from what {from} found."),
        LinkKind::Reviews { rounds } => format!(
            "{from} reviews {to}: it re-runs {to}'s claims in Lab, and {to} answers. {rounds} round{}.",
            if rounds == 1 { "" } else { "s" }
        ),
        LinkKind::Uses => format!("{from} uses Lab: it has Lab's tools."),
    };
    let mut form = column![heading(format!("{from} → {to}")), hint(what)].spacing(8);
    if link.to != LAB {
        let kinds: Vec<LinkKind> = LinkKind::DRAWN
            .into_iter()
            .filter(|k| *k != LinkKind::Uses)
            .map(|k| match (k, link.kind) {
                (LinkKind::Reviews { .. }, LinkKind::Reviews { rounds }) => {
                    LinkKind::Reviews { rounds }
                }
                _ => k,
            })
            .collect();
        form = form.push(field(
            "Kind",
            pick_list(kinds, Some(link.kind), |k| say(AgentsMessage::Kind(k)))
                .text_size(SMALL)
                .padding([4, 6]),
        ));
        if let LinkKind::Reviews { rounds } = link.kind {
            form = form.push(field(
                "Rounds",
                pick_list([1u32, 2, 3, 4, 5], Some(rounds), |r| {
                    say(AgentsMessage::Rounds(r))
                })
                .text_size(SMALL)
                .padding([4, 6]),
            ));
        }
        form = form.push(chip("Reverse it", false, Some(AgentsMessage::Reverse)));
    }
    form.into()
}

fn lab_setup<'a>(agents: &'a Agents, team: &'a Team) -> Element<'a, Message> {
    let found = agents
        .found
        .as_ref()
        .and_then(|f| f.lab.as_ref())
        .map_or_else(
            || "Not found: build it with cargo build --release -p persephone-lab".to_owned(),
            |p| p.display().to_string(),
        );
    let program = team
        .lab
        .program
        .as_ref()
        .map_or_else(String::new, |p| p.display().to_string());
    let own = matches!(team.lab.data, LabData::Own(_));
    let mut form = column![
        heading("Lab"),
        hint("Persephone Lab: its tools for agents. Every agent with a Uses link reaches it. It can't edit code, place orders, or delete your data."),
        field("Program", input(&found, &program, AgentsMessage::LabProgram)),
        checkbox(own)
            .label("Its own data, apart from yours")
            .text_size(SMALL)
            .on_toggle(|on| say(AgentsMessage::LabOwnData(on))),
    ]
    .spacing(8);
    match &team.lab.data {
        LabData::Own(dir) => {
            form = form.push(field(
                "Its data folder",
                input(
                    "~/persephone-agent-data",
                    &dir.display().to_string(),
                    AgentsMessage::LabFolder,
                ),
            ));
        }
        LabData::App => {
            form = form.push(hint("It uses your app's markets and saves to your saved results, where you can open what the agents saved."));
        }
    }
    form.into()
}

fn team_setup<'a>(agents: &'a Agents, team: &'a Team) -> Element<'a, Message> {
    let found = agents.found.as_ref();
    let claude_found = found
        .and_then(|f| f.claude.as_ref())
        .map_or_else(|| "Not found".to_owned(), |p| p.display().to_string());
    let claude = agents
        .saved
        .claude
        .as_ref()
        .map_or_else(String::new, |p| p.display().to_string());
    let login = match found.map(|f| (f.claude.is_some(), f.logged_in)) {
        Some((true, Some(true))) => "Logged in.",
        Some((true, Some(false))) => "Not logged in: run `claude auth login` in a terminal.",
        Some((true, None)) => "Couldn't tell whether it's logged in.",
        Some((false, _)) => "Install Claude Code, or say where it is.",
        None => "Looking…",
    };
    let root = agents
        .saved
        .root
        .as_ref()
        .map_or_else(String::new, |p| p.display().to_string());
    column![
        heading(team.name.clone()),
        field("Name", input("Name", &team.name, AgentsMessage::TeamName)),
        field(
            "Brief: what the team should study",
            editor(&agents.brief, 120.0, AgentsMessage::Brief)
        ),
        field(
            "Principles: how every agent works. Method, not market views",
            editor(&agents.principles, 220.0, AgentsMessage::Principles)
        ),
        field(
            "Sessions at once",
            pick_list([1u32, 2, 3, 4], Some(team.at_once), |n| say(AgentsMessage::AtOnce(n)))
                .text_size(SMALL)
                .padding([4, 6])
        ),
        Space::new().height(6),
        heading("This computer"),
        field("Claude Code", input(&claude_found, &claude, AgentsMessage::Claude)),
        hint(login),
        field(
            "Where teams are kept",
            input(
                &persephone_agents::folder::default_root().display().to_string(),
                &root,
                AgentsMessage::Root
            )
        ),
        hint("Each agent at the top of a session has a folder there with its memory, committed to git after every session. It can't read Persephone's code, use a shell or reach the web."),
    ]
    .spacing(8)
    .into()
}

/// The run's lines, for the selected agent or the whole team: the past run being read, or the
/// run on screen.
fn activity<'a>(agents: &'a Agents, team: &'a Team) -> Element<'a, Message> {
    let (lines, live): (&[(usize, Line)], Option<&super::Live>) =
        match (&agents.viewing, agents.live()) {
            (Some((_, lines)), _) => (lines, None),
            (None, Some(live)) => (&live.log, Some(live)),
            (None, None) => {
                return column![hint("Nothing has run yet. Press Run, or pick a past run."),]
                    .into();
            }
        };
    let only = match &agents.selected {
        Selected::Agent(id) => Some(id.as_str()),
        _ => None,
    };
    // A past run's members are told apart only by key, as the plan isn't kept with it.
    let name_of = |step: usize, who: &Who| -> String {
        match (live.and_then(|l| l.agent_of(step, who)), who) {
            (Some(id), _) => team.name_of(id).to_owned(),
            (None, Who::Member(key)) => key.clone(),
            (None, Who::Lead) => agents
                .viewing
                .as_ref()
                .and_then(|(summary, _)| summary.steps.get(step))
                .map_or_else(String::new, |s| team.name_of(&s.agent).to_owned()),
        }
    };
    let belongs = |step: usize, who: &Who| -> bool {
        let Some(only) = only else { return true };
        match live.and_then(|l| l.agent_of(step, who)) {
            Some(id) => id == only,
            None => team.agent(only).is_some_and(|a| {
                name_of(step, who) == a.name
                    || persephone_agents::team::key(&a.name) == name_of(step, who)
            }),
        }
    };
    let mut list = column![].spacing(3);
    let shown: Vec<&(usize, Line)> = lines
        .iter()
        .filter(|(step, line)| match line {
            Line::Event(Event::Delegated { to: key, .. } | Event::Returned { from: key, .. }) => {
                belongs(*step, &Who::Lead) || belongs(*step, &Who::Member(key.clone()))
            }
            Line::Event(event) => belongs(*step, event.who()),
            Line::Status(_) => belongs(*step, &Who::Lead),
        })
        .collect();
    let skip = shown.len().saturating_sub(MOST_LINES);
    for (step, line) in shown.into_iter().skip(skip) {
        list = list.push(activity_line(*step, line, &name_of, live));
    }
    let mut notes = column![].spacing(2);
    if let Some(live) = live {
        for note in &live.notes {
            notes = notes.push(text(note.clone()).size(HINT).color(theme::DOWN));
        }
    }
    column![notes, scrollable(list).anchor_bottom().height(Fill)]
        .spacing(4)
        .into()
}

fn activity_line<'a>(
    step: usize,
    line: &'a Line,
    name_of: &dyn Fn(usize, &Who) -> String,
    live: Option<&super::Live>,
) -> Element<'a, Message> {
    let (said, color) = match line {
        Line::Status(status) => {
            let label = live.and_then(|l| l.plan.as_ref()).map_or_else(
                || format!("Step {}", step + 1),
                |plan| plan_label(plan, step, name_of),
            );
            (format!("{label}: {}", status.label()), status_color(status))
        }
        Line::Event(event) => match event {
            Event::Started { model, lab, .. } => (
                format!(
                    "{} started on {model}{}",
                    name_of(step, &Who::Lead),
                    lab.as_ref()
                        .map_or_else(String::new, |s| format!(", Lab {s}"))
                ),
                theme::TEXT_MUTED,
            ),
            Event::Said { who, text } => (
                format!("{}: {}", name_of(step, who), clip(text, 400)),
                theme::TEXT_BRIGHT,
            ),
            Event::Called { who, tool, about } => (
                format!("{} → {tool} {about}", name_of(step, who)),
                theme::TEXT,
            ),
            Event::Answered { tool, ok, text, .. } => (
                format!(
                    "   ← {tool}{}: {}",
                    if *ok { "" } else { " failed" },
                    clip(text, 160)
                ),
                if *ok { theme::TEXT_MUTED } else { theme::DOWN },
            ),
            Event::Delegated { who, to, task } => (
                format!("{} hands {to}: {task}", name_of(step, who)),
                theme::ACCENT,
            ),
            Event::Returned { from, text, .. } => (
                format!("{from} brings back: {}", clip(text, 400)),
                theme::UP,
            ),
            Event::Refused { who, tool, why } => (
                format!(
                    "{} was refused {tool}: {}",
                    name_of(step, who),
                    clip(why, 200)
                ),
                theme::DOWN,
            ),
            Event::Finished(finish) => (
                format!(
                    "{} finished: {} turns, ${:.2}{}",
                    name_of(step, &Who::Lead),
                    finish.turns,
                    finish.spend,
                    if finish.refused.is_empty() {
                        String::new()
                    } else {
                        format!(", refused {}", finish.refused.join(", "))
                    }
                ),
                if finish.ok { theme::UP } else { theme::DOWN },
            ),
        },
    };
    text(said).size(HINT).color(color).width(Fill).into()
}

fn plan_label(
    plan: &persephone_agents::plan::Plan,
    step: usize,
    name_of: &dyn Fn(usize, &Who) -> String,
) -> String {
    let name = name_of(step, &Who::Lead);
    match plan.steps.get(step).map(|s| s.kind) {
        Some(persephone_agents::plan::StepKind::Review { round, .. }) => {
            format!("{name}'s review, round {round}")
        }
        Some(persephone_agents::plan::StepKind::Answer { round }) => {
            format!("{name}'s answer, round {round}")
        }
        _ => name,
    }
}

fn outputs(agents: &Agents) -> Element<'_, Message> {
    let Some(outputs) = &agents.outputs else {
        return hint("Pick an agent to see its files.");
    };
    let mut list = column![
        row![
            text(outputs.folder.display().to_string())
                .size(HINT)
                .color(theme::TEXT_MUTED)
                .width(Fill),
            chip("Open folder", false, Some(AgentsMessage::OpenFolder)),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
    ]
    .spacing(8);
    if !outputs.own {
        list = list.push(hint(
            "It works inside its lead's session, so its files are its lead's; below are the notes it keeps there.",
        ));
    }
    for (title, said) in &outputs.files {
        let said = if said.trim().is_empty() {
            "Nothing yet.".to_owned()
        } else {
            said.clone()
        };
        list = list
            .push(heading(title.clone()))
            .push(text(said).size(HINT).color(theme::TEXT).width(Fill));
    }
    if outputs.own {
        list = list
            .push(Space::new().height(6))
            .push(heading("Yours to edit"))
            .push(field(
                "vetoed.md: what you reverted from its memory, and why",
                editor(&outputs.vetoed, 110.0, AgentsMessage::Vetoed),
            ))
            .push(field(
                "tasks.md: jobs for it to work through",
                editor(&outputs.tasks, 110.0, AgentsMessage::Tasks),
            ))
            .push(chip("Save", false, Some(AgentsMessage::SaveNotes)));
    }
    list.into()
}

fn feedback(agents: &Agents) -> Element<'_, Message> {
    if agents.feedback.is_empty() {
        return hint("No agent has written anything for the app yet.");
    }
    let mut list = column![hint(
        "What the agents found wrong with Persephone, and what they'd make better."
    )]
    .spacing(8);
    for (name, bugs, ideas) in &agents.feedback {
        list = list.push(heading(name.clone()));
        for (title, said) in [("Bugs", bugs), ("Ideas", ideas)] {
            let body = said
                .lines()
                .skip_while(|line| {
                    line.starts_with('#') || line.trim().is_empty() || line.starts_with("Each:")
                })
                .collect::<Vec<_>>()
                .join("\n");
            list = list
                .push(text(title).size(HINT).color(theme::TEXT_MUTED))
                .push(
                    text(if body.trim().is_empty() {
                        "Nothing yet.".to_owned()
                    } else {
                        body
                    })
                    .size(HINT)
                    .color(theme::TEXT)
                    .width(Fill),
                );
        }
    }
    list.into()
}

fn clip(said: &str, most: usize) -> String {
    let said = said.trim();
    if said.chars().count() <= most {
        return said.to_owned();
    }
    let mut cut: String = said.chars().take(most).collect();
    cut.push('…');
    cut
}
