//! The Build tab: a strategy as rules to click together, or as its JSON.

use std::fmt;

use iced::widget::{column, text_editor};
use persephone_core::Timeframe;

use super::*;
use crate::backtest::{
    BuildMessage, BuildSlot, BuildState,
    logic_draft::{
        Amount, Comparison, Draft, EXAMPLES, NUMBER, NodePath, Operand, Rule, VARIABLE, Wish,
        operand_kinds,
    },
};

fn build(message: BuildMessage) -> PanelMessage {
    PanelMessage::Build(message)
}

/// A kind of value, as the builder's lists name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Kind(&'static str);

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.0 {
            NUMBER => "number",
            VARIABLE => "$ variable",
            "sma" => "SMA",
            "ema" => "EMA",
            "rsi" => "RSI",
            "atr" => "ATR",
            "roc" => "ROC",
            "zscore" => "z-score",
            "stddev" => "std dev",
            "add" => "+",
            "sub" => "−",
            "mul" => "×",
            "div" => "÷",
            "on" => "on a timeframe",
            "entry_price" => "entry price",
            "bars_held" => "bars held",
            "high_since_entry" => "high since entry",
            "low_since_entry" => "low since entry",
            other => other,
        })
    }
}

/// All of a group's rules, or any one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Group(bool);

impl fmt::Display for Group {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.0 { "All of" } else { "Any of" })
    }
}

/// Where to start a tree from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Start(Option<usize>);

impl fmt::Display for Start {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            None => f.write_str("Start again"),
            Some(at) => f.write_str(EXAMPLES[at].0),
        }
    }
}

fn small_list<T, F>(options: Vec<T>, picked: Option<T>, on: F) -> Element<'static, PanelMessage>
where
    T: fmt::Display + Clone + PartialEq + 'static,
    F: Fn(T) -> PanelMessage + 'static,
{
    pick_list(options, picked, on)
        .text_size(HINT)
        .padding([2, 6])
        .style(|_, status| list_style(status))
        .menu_style(|_| list_menu_style())
        .into()
}

fn small_button(label: &str, message: PanelMessage, lit: bool) -> Element<'static, PanelMessage> {
    button(text(label.to_owned()).size(HINT).center())
        .padding([2, 6])
        .on_press(message)
        .style(move |_, status| chip(lit, status))
        .into()
}

fn word(label: &str) -> Element<'static, PanelMessage> {
    text(label.to_owned())
        .size(HINT)
        .color(theme::TEXT_MUTED)
        .into()
}

/// A number's box and its # / $ switch.
fn number_box(
    state: &BuildState,
    path: &NodePath,
    slot: BuildSlot,
    shown: String,
) -> Element<'static, PanelMessage> {
    let value = state.typed(path, slot).map_or(shown, ToOwned::to_owned);
    let at = path.clone();
    row![
        text_input("0", &value)
            .size(HINT)
            .style(|_, status| input_style(status))
            .width(56)
            .padding([2, 4])
            .on_input(move |typed| build(BuildMessage::Typed(at.clone(), slot, typed))),
        tip(
            small_button("#", build(BuildMessage::Variable(path.clone(), slot)), false),
            "A fixed number. Click to make it a variable ($), which Optimize, Sweep and walk-forward can vary.",
            None,
        ),
    ]
    .spacing(2)
    .align_y(Alignment::Center)
    .into()
}

/// A variable in place of a number, and its switch back.
fn variable_box(name: &str, path: &NodePath, slot: BuildSlot) -> Element<'static, PanelMessage> {
    row![
        text(format!("${name}"))
            .size(HINT)
            .color(theme::UP)
            .font(iced::Font::MONOSPACE),
        tip(
            small_button("$", build(BuildMessage::Variable(path.clone(), slot)), true),
            "A variable, which Optimize, Sweep and walk-forward can vary. Click to make it a fixed number again.",
            None,
        ),
    ]
    .spacing(2)
    .align_y(Alignment::Center)
    .into()
}

fn amount_box(
    state: &BuildState,
    path: &NodePath,
    amount: &Amount,
) -> Element<'static, PanelMessage> {
    match amount {
        Amount::Whole(n) => number_box(state, path, BuildSlot::Bars, n.to_string()),
        Amount::Param(name) => variable_box(name, path, BuildSlot::Bars),
    }
}

/// A value, with what it's made of after it.
fn operand(state: &BuildState, path: &NodePath, value: &Operand) -> Element<'static, PanelMessage> {
    let at = path.clone();
    let kinds: Vec<Kind> = operand_kinds().into_iter().map(Kind).collect();
    let kind = small_list(kinds, Some(Kind(value.kind())), move |picked: Kind| {
        build(BuildMessage::Kind(at.clone(), picked.0))
    });
    let mut parts = row![].spacing(4).align_y(Alignment::Center);
    match value {
        // A number or variable picks its kind from a list after it.
        Operand::Number(n) => {
            parts = parts
                .push(number_box(state, path, BuildSlot::Number, label(*n)))
                .push(kind);
        }
        Operand::Param(name) => {
            parts = parts
                .push(variable_box(name, path, BuildSlot::Number))
                .push(kind);
        }
        Operand::Field(_) => parts = parts.push(kind),
        Operand::Indicator {
            kind: which,
            of,
            length,
        } => {
            parts = parts.push(kind);
            if *which != "atr" {
                let close = Operand::Field("close");
                parts = parts.push(word("of")).push(operand(
                    state,
                    &path.then(0),
                    of.as_deref().unwrap_or(&close),
                ));
            }
            parts = parts
                .push(word("length"))
                .push(amount_box(state, path, length));
        }
        Operand::Ago { of, bars } => {
            parts = parts
                .push(kind)
                .push(operand(state, &path.then(0), of))
                .push(word("bars"))
                .push(amount_box(state, path, bars));
        }
        Operand::Arith { a, b, .. } => {
            parts = parts
                .push(word("("))
                .push(operand(state, &path.then(0), a))
                .push(kind)
                .push(operand(state, &path.then(1), b))
                .push(word(")"));
        }
        Operand::Abs(inner) => {
            parts = parts.push(kind).push(operand(state, &path.then(0), inner));
        }
        Operand::On { timeframe, value } => {
            let at = path.clone();
            let frames: Vec<String> = Timeframe::ALL
                .iter()
                .map(|tf| tf.label().to_owned())
                .collect();
            parts = parts
                .push(kind)
                .push(small_list(frames, Some(timeframe.clone()), move |tf| {
                    build(BuildMessage::Timeframe(at.clone(), tf))
                }))
                .push(operand(state, &path.then(0), value));
        }
    }
    container(parts)
        .padding([1, 3])
        .style(|_| container::Style {
            border: iced::Border {
                color: theme::AXIS_LINE,
                width: 1.0,
                radius: 3.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

fn label(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{n:.0}")
    } else {
        format!("{n}")
    }
}

/// A rule and what's in it. `negated` is where the `not` around it is, if there's one.
fn rule_view(
    state: &BuildState,
    path: &NodePath,
    rule: &Rule,
    negated: Option<&NodePath>,
    root: bool,
) -> Element<'static, PanelMessage> {
    let not_at = negated.cloned().unwrap_or_else(|| path.clone());
    let not = tip(
        small_button(
            "not",
            build(BuildMessage::Not(not_at.clone())),
            negated.is_some(),
        ),
        "True when this isn't.",
        None,
    );
    let remove = |at: NodePath| small_button("✕", build(BuildMessage::Remove(at)), false);
    match rule {
        Rule::Not(inner) => rule_view(state, &path.then(0), inner, Some(path), root),
        Rule::All(rules) | Rule::Any(rules) => {
            let all = matches!(rule, Rule::All(_));
            let at = path.clone();
            let mut head = row![
                not,
                small_list(
                    vec![Group(true), Group(false)],
                    Some(Group(all)),
                    move |g: Group| { build(BuildMessage::Group(at.clone(), g.0)) }
                ),
            ]
            .spacing(4)
            .align_y(Alignment::Center);
            if !root {
                head = head.push(remove(not_at));
            }
            let mut body = column![head].spacing(3);
            for (at, child) in rules.iter().enumerate() {
                body = body.push(
                    row![
                        Space::new().width(16),
                        rule_view(state, &path.then(at as u8), child, None, false)
                    ]
                    .align_y(Alignment::Center),
                );
            }
            body = body.push(
                row![
                    Space::new().width(16),
                    small_button(
                        "+ condition",
                        build(BuildMessage::Add(path.clone(), false)),
                        false
                    ),
                    small_button(
                        "+ group",
                        build(BuildMessage::Add(path.clone(), true)),
                        false
                    ),
                ]
                .spacing(4),
            );
            body.into()
        }
        Rule::Compare { op, left, right } => {
            let at = path.clone();
            row![
                not,
                operand(state, &path.then(0), left),
                small_list(Comparison::ALL.to_vec(), Some(*op), move |op| {
                    build(BuildMessage::Comparison(at.clone(), op))
                }),
                operand(state, &path.then(1), right),
                remove(not_at),
            ]
            .spacing(4)
            .align_y(Alignment::Center)
            .into()
        }
    }
}

/// The Build tab's pane: the tree as rules, or as JSON.
pub(super) fn build_pane<'a>(
    panel: &'a Panel,
    draft: &Draft,
    on_chart: bool,
) -> Element<'a, PanelMessage> {
    let state = &panel.build;
    let json_mode = state.json_mode;
    let modes = row![
        small_button(
            "Rule builder",
            build(BuildMessage::JsonMode(false)),
            !json_mode
        ),
        tip(
            small_button("Raw JSON", build(BuildMessage::JsonMode(true)), json_mode),
            "The tree as JSON, to paste one in from an agent or a script, or to copy out.",
            None,
        ),
    ]
    .spacing(2);
    let starts: Vec<Start> = std::iter::once(Start(None))
        .chain((0..EXAMPLES.len()).map(|at| Start(Some(at))))
        .collect();
    let start = tip(
        pick_list(starts, None::<Start>, |start: Start| {
            build(BuildMessage::StartFrom(start.0))
        })
        .placeholder("Start from…")
        .text_size(HINT)
        .padding([2, 6])
        .style(|_, status| list_style(status))
        .menu_style(|_| list_menu_style()),
        "Start again from a single rule, or from one of the guide's examples.",
        None,
    );
    let name = text_input("Its name", &draft.name)
        .size(HINT)
        .style(|_, status| input_style(status))
        .width(200)
        .padding([2, 6])
        .on_input(|name| build(BuildMessage::Name(name)));
    let status: Element<'a, PanelMessage> = if on_chart {
        word("The chart is running this tree.")
    } else {
        row![
            word("Not on the chart yet: change anything, or"),
            small_button("Use on chart", build(BuildMessage::UseOnChart), false),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
    };
    let top = row![modes, start, name, status]
        .spacing(10)
        .align_y(Alignment::Center);

    let body: Element<'a, PanelMessage> = if json_mode {
        column![
            text_editor(&state.json)
                .on_action(|action| build(BuildMessage::JsonEdit(action)))
                .font(iced::Font::MONOSPACE)
                .size(HINT)
                .height(Fill),
            row![
                tip(
                    small_button("Apply", build(BuildMessage::ApplyJson), false),
                    "Read it in as the rules. A tree with a mistake says where, and nothing changes.",
                    None,
                ),
                small_button("Back to the rules", build(BuildMessage::JsonMode(false)), false),
            ]
            .spacing(6),
        ]
        .spacing(4)
        .height(Fill)
        .into()
    } else {
        let mut rules = column![].spacing(6);
        for wish in Wish::ALL {
            let (title, what) = match wish {
                Wish::Long => ("Go long when", "Hold long while this is true."),
                Wish::Short => ("Go short when", "Hold short while this is true."),
                Wish::Flat => (
                    "Go flat when",
                    "Close when this is true, unless long or short is asked for. Without it, a position is held until the other side is asked for.",
                ),
            };
            let heading = tip(
                text(title).size(SMALL).color(theme::TEXT_BRIGHT).width(110),
                what,
                None,
            );
            rules = rules.push(match draft.side(wish) {
                Some(rule) => row![
                    heading,
                    rule_view(state, &NodePath::root(wish), rule, None, true),
                    small_button("✕", build(BuildMessage::Side(wish, false)), false),
                ]
                .spacing(6)
                .align_y(Alignment::Start),
                None => row![
                    heading,
                    word("nothing"),
                    small_button("+ add", build(BuildMessage::Side(wish, true)), false),
                ]
                .spacing(6)
                .align_y(Alignment::Center),
            });
        }
        if !draft.params.is_empty() {
            let mut variables = row![tip(
                text("Variables").size(SMALL).color(theme::TEXT_BRIGHT).width(110),
                "Each is a parameter of the strategy: Optimize, Sweep and walk-forward can vary it, and the report can set it. Rename one here and it changes everywhere it's used.",
                None,
            )]
            .spacing(8)
            .align_y(Alignment::Center);
            for (name, start) in &draft.params {
                let typed_name = state
                    .renaming
                    .iter()
                    .find(|(known, _)| known == name)
                    .map_or_else(|| name.clone(), |(_, text)| text.clone());
                let typed_value = state
                    .values
                    .iter()
                    .find(|(known, _)| known == name)
                    .map_or_else(|| label(*start), |(_, text)| text.clone());
                let (renamed, valued, submitted) = (name.clone(), name.clone(), name.clone());
                variables = variables.push(
                    row![
                        text("$").size(HINT).color(theme::UP),
                        text_input("name", &typed_name)
                            .size(HINT)
                            .style(|_, status| input_style(status))
                            .width(96)
                            .padding([2, 4])
                            .on_input(move |text| build(BuildMessage::RenameTyped(
                                renamed.clone(),
                                text
                            )))
                            .on_submit(build(BuildMessage::Rename(submitted))),
                        text_input("0", &typed_value)
                            .size(HINT)
                            .style(|_, status| input_style(status))
                            .width(56)
                            .padding([2, 4])
                            .on_input(move |text| build(BuildMessage::ValueTyped(
                                valued.clone(),
                                text
                            ))),
                    ]
                    .spacing(3)
                    .align_y(Alignment::Center),
                );
            }
            rules = rules.push(variables);
        }
        scrollable(rules.padding(Padding {
            right: 12.0,
            bottom: 12.0,
            ..Padding::ZERO
        }))
        .direction(scrollable::Direction::Both {
            vertical: scrollable::Scrollbar::default(),
            horizontal: scrollable::Scrollbar::default(),
        })
        .width(Fill)
        .height(Fill)
        .into()
    };

    let save = row![
        text_input(&draft.name, &state.save_name)
            .size(HINT)
            .style(|_, status| input_style(status))
            .width(200)
            .padding([2, 6])
            .on_input(|name| build(BuildMessage::SaveName(name)))
            .on_submit(build(BuildMessage::Save)),
        tip(
            small_button("Save as strategy…", build(BuildMessage::Save), false),
            "Save it by name with this chart's settings, where every chart's strategy list, portfolio legs, Lab and agents find it.",
            None,
        ),
        small_button("Copy JSON", build(BuildMessage::CopyJson), false),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    let mut pane = column![top, body, save].spacing(8);
    if let Some(error) = &state.error {
        pane = pane.push(text(error.as_str()).size(SMALL).color(theme::DOWN));
    }
    if let Some(saved) = &state.saved {
        pane = pane.push(text(saved.as_str()).size(SMALL).color(theme::UP));
    }
    container(pane.padding(Padding {
        top: 40.0,
        right: 12.0,
        bottom: 8.0,
        left: 12.0,
    }))
    .width(Fill)
    .height(Fill)
    .into()
}
