//! The left-click menu, the indicator settings popover and the strategy report. All float
//! above the tiles where they were opened; the app owns what they act on.
//!
//! Menu pages:
//! - root: Indicators ›, Strategy ›, Symbol ›, and a row of timeframe chips;
//! - indicators: every built-in, grouped by where it's drawn, then loaded plugins;
//! - strategies: every built-in strategy and plugin, with a one-line description;
//! - symbol: a search box with popular Binance pairs.
//!
//! The report opens from a chart's strategy summary: settings, full statistics and a remove
//! button.

use std::path::Path;

use iced::{
    Background, Border, Color, Element, Fill, Point, Shadow, Size, Vector,
    widget::{Id, button, column, container, opaque, pin, row, rule, space, text, text_input},
};
use persephone_chart::theme;
use persephone_core::{
    Timeframe,
    indicator::{BuiltIn, IndicatorSpec, Param, Placement},
    strategy::{BuiltInStrategy, StrategyKind, StrategySpec},
};
use persephone_engine::BacktestReport;
use persephone_plugin_host::Plugins;
use persephone_tiling::TileId;

use crate::Run;

pub const WIDTH: f32 = 248.0;
pub const REPORT_WIDTH: f32 = 300.0;
/// Room kept between a popup and the window edge.
const MARGIN: f32 = 6.0;

const PANEL: Color = Color::from_rgb8(0x1e, 0x22, 0x2d);
const PANEL_BORDER: Color = Color::from_rgb8(0x36, 0x3a, 0x45);
const HOVER: Color = Color::from_rgb8(0x2a, 0x2e, 0x39);

/// Popular Binance pairs offered in the symbol search.
const SYMBOLS: [&str; 14] = [
    "BTCUSDT", "ETHUSDT", "SOLUSDT", "BNBUSDT", "XRPUSDT", "DOGEUSDT", "ADAUSDT", "AVAXUSDT",
    "LINKUSDT", "TONUSDT", "DOTUSDT", "LTCUSDT", "SUIUSDT", "PEPEUSDT",
];

pub fn symbol_input() -> Id {
    Id::new("symbol-search")
}

#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    Root,
    Indicators,
    Strategies,
    Symbol { query: String },
}

/// What's open, if anything. Positions are in tile-content coordinates.
#[derive(Clone, Debug)]
pub enum Popup {
    Menu {
        tile: TileId,
        at: Point,
        page: Page,
    },
    Settings {
        tile: TileId,
        study: usize,
        at: Point,
        /// What's typed in each parameter box, valid or not.
        inputs: Vec<String>,
        error: Option<String>,
    },
    Report {
        tile: TileId,
        at: Point,
        /// What's typed in each strategy parameter box, valid or not.
        inputs: Vec<String>,
        error: Option<String>,
    },
}

impl Popup {
    pub fn tile(&self) -> TileId {
        match self {
            Self::Menu { tile, .. } | Self::Settings { tile, .. } | Self::Report { tile, .. } => {
                *tile
            }
        }
    }

    /// Settings for `spec`, opened just below the legend row clicked at `at`.
    pub fn settings(tile: TileId, study: usize, at: Point, spec: &IndicatorSpec) -> Self {
        Self::Settings {
            tile,
            study,
            at: Point::new(at.x, at.y + 14.0),
            inputs: spec.params.iter().map(f64::to_string).collect(),
            error: None,
        }
    }

    /// The strategy report with its top-left corner at `at`.
    pub fn report(tile: TileId, at: Point, spec: &StrategySpec) -> Self {
        Self::Report {
            tile,
            at,
            inputs: spec.params.iter().map(f64::to_string).collect(),
            error: None,
        }
    }

    fn at(&self) -> Point {
        match self {
            Self::Menu { at, .. } | Self::Settings { at, .. } | Self::Report { at, .. } => *at,
        }
    }

    fn width(&self) -> f32 {
        match self {
            Self::Report { .. } => REPORT_WIDTH,
            _ => WIDTH,
        }
    }

    /// A generous guess at the panel's height, for keeping it on screen. `extra` counts rows a
    /// page adds for plugins.
    fn height(&self, extra: usize) -> f32 {
        let extra = 30.0 * extra as f32;
        match self {
            Self::Menu {
                page: Page::Root, ..
            } => 190.0,
            Self::Menu {
                page: Page::Indicators,
                ..
            } => 400.0 + extra,
            Self::Menu {
                page: Page::Strategies,
                ..
            } => 270.0 + extra,
            Self::Menu {
                page: Page::Symbol { .. },
                ..
            } => 330.0,
            Self::Settings { inputs, .. } => 110.0 + 40.0 * inputs.len() as f32,
            Self::Report { inputs, .. } => 360.0 + 40.0 * inputs.len() as f32,
        }
    }
}

#[derive(Clone, Debug)]
pub enum MenuMessage {
    Open(Page),
    AddIndicator(BuiltIn),
    /// A plugin indicator, by id.
    AddPluginIndicator(String),
    SetTimeframe(Timeframe),
    QueryChanged(String),
    SetSymbol(String),
    SubmitSymbol,
    ParamChanged(usize, String),
    RemoveStudy,
    SetStrategy(BuiltInStrategy),
    /// A plugin strategy, by id.
    SetPluginStrategy(String),
    StrategyParamChanged(usize, String),
    RemoveStrategy,
    Close,
}

/// What the tile under the popup shows, for labels and the current selection.
pub struct TileInfo<'a> {
    pub symbol: &'a str,
    /// Quote currency for money figures.
    pub quote: &'a str,
    pub timeframe: Timeframe,
    pub specs: &'a [IndicatorSpec],
    pub strategy: Option<(&'a StrategySpec, &'a Run)>,
    pub plugins: &'a Plugins,
    /// Where plugin files go, to tell people.
    pub plugin_dir: Option<&'a Path>,
}

/// The popup, pinned at `origin` (the tile content's top-left in the window) plus its own
/// position, nudged to stay inside a `window`-sized area.
pub fn view<'a>(
    popup: &'a Popup,
    tile: TileInfo<'a>,
    origin: Point,
    window: Size,
) -> Element<'a, MenuMessage> {
    let content = match popup {
        Popup::Menu { page, .. } => match page {
            Page::Root => root(&tile),
            Page::Indicators => indicators(tile.plugins, tile.plugin_dir),
            Page::Strategies => strategies(
                tile.strategy.map(|(spec, _)| &spec.kind),
                tile.plugins,
                tile.plugin_dir,
            ),
            Page::Symbol { query } => symbols(query, tile.symbol),
        },
        Popup::Settings {
            study,
            inputs,
            error,
            ..
        } => match tile.specs.get(*study) {
            Some(spec) => settings(spec, inputs, error.as_deref()),
            None => column![].into(),
        },
        Popup::Report { inputs, error, .. } => match tile.strategy {
            Some((spec, run)) => report(spec, run, tile.quote, inputs, error.as_deref()),
            None => column![].into(),
        },
    };

    let extra = match popup {
        Popup::Menu {
            page: Page::Indicators,
            ..
        } => tile.plugins.indicators.len() + tile.plugins.errors.len() + 2,
        Popup::Menu {
            page: Page::Strategies,
            ..
        } => 2 * tile.plugins.strategies.len() + 2,
        _ => 0,
    };
    let at = popup.at();
    let width = popup.width();
    let x = (origin.x + at.x)
        .min(window.width - width - MARGIN)
        .max(MARGIN);
    let y = (origin.y + at.y)
        .min(window.height - popup.height(extra) - MARGIN)
        .max(MARGIN);
    pin(opaque(panel(content, width))).x(x).y(y).into()
}

fn root<'a>(tile: &TileInfo<'a>) -> Element<'a, MenuMessage> {
    let chips = row(Timeframe::ALL.into_iter().map(|tf| {
        let selected = tf == tile.timeframe;
        button(text(tf.label()).size(12).center())
            .padding([4, 0])
            .width(Fill)
            .on_press(MenuMessage::SetTimeframe(tf))
            .style(move |_, status| chip(selected, status))
            .into()
    }))
    .spacing(3);

    let strategy = tile
        .strategy
        .map_or("None", |(spec, _)| spec.info.name.as_str());
    column![
        item("Indicators", "›", MenuMessage::Open(Page::Indicators)),
        item("Strategy", strategy, MenuMessage::Open(Page::Strategies)),
        item(
            "Symbol",
            tile.symbol,
            MenuMessage::Open(Page::Symbol {
                query: String::new()
            })
        ),
        separator(),
        section("Timeframe"),
        container(chips).padding([2, 6]),
    ]
    .spacing(2)
    .into()
}

fn indicators<'a>(plugins: &'a Plugins, dir: Option<&'a Path>) -> Element<'a, MenuMessage> {
    let group = |placement: Placement| {
        BuiltIn::ALL
            .into_iter()
            .filter(move |kind| kind.placement() == placement)
            .map(|kind| {
                item(
                    kind.name(),
                    kind.short_name(),
                    MenuMessage::AddIndicator(kind),
                )
            })
    };
    let from_plugins = plugins.indicators.values().map(|plugin| {
        item(
            &plugin.info.name,
            &plugin.info.short_name,
            MenuMessage::AddPluginIndicator(plugin.id.clone()),
        )
    });
    let broken = plugins.errors.iter().map(|(file, error)| {
        container(text(format!("{file}: {error}")).size(11).color(theme::DOWN))
            .padding([2, 10])
            .into()
    });

    column![back("Indicators"), section("On the chart")]
        .extend(group(Placement::Overlay))
        .push(section("In a pane below"))
        .extend(group(Placement::Pane))
        .push(section("Plugins"))
        .extend(from_plugins)
        .extend(broken)
        .extend(plugin_note(dir, plugins))
        .spacing(2)
        .into()
}

fn strategies<'a>(
    current: Option<&'a StrategyKind>,
    plugins: &'a Plugins,
    dir: Option<&'a Path>,
) -> Element<'a, MenuMessage> {
    let built_in = BuiltInStrategy::ALL.into_iter().map(move |kind| {
        let selected = current == Some(&StrategyKind::BuiltIn(kind));
        strategy_choice(
            kind.name(),
            kind.description(),
            selected,
            MenuMessage::SetStrategy(kind),
        )
    });
    let from_plugins = plugins.strategies.values().map(move |plugin| {
        let selected = current == Some(&StrategyKind::Plugin(plugin.id.clone()));
        strategy_choice(
            &plugin.info.name,
            &plugin.info.description,
            selected,
            MenuMessage::SetPluginStrategy(plugin.id.clone()),
        )
    });
    column![back("Strategy")]
        .extend(built_in)
        .push(section("Plugins"))
        .extend(from_plugins)
        .push(note("Backtests this chart's bars from a 100,000 balance"))
        .extend(plugin_note(dir, plugins))
        .spacing(2)
        .into()
}

fn strategy_choice<'a>(
    name: &'a str,
    description: &'a str,
    selected: bool,
    on_press: MenuMessage,
) -> Element<'a, MenuMessage> {
    button(
        column![
            row![
                text(name).size(13),
                space::horizontal(),
                text(if selected { "current" } else { "" })
                    .size(12)
                    .color(theme::TEXT_MUTED),
            ],
            text(description).size(11).color(theme::TEXT_MUTED),
        ]
        .spacing(2),
    )
    .width(Fill)
    .padding([6, 10])
    .on_press(on_press)
    .style(|_, status| item_style(status))
    .into()
}

/// Where to drop plugin files; they load and reload on their own.
fn plugin_note<'a>(dir: Option<&Path>, plugins: &Plugins) -> Option<Element<'a, MenuMessage>> {
    match dir {
        Some(dir) => Some(note_owned(format!(
            "Drop .wasm plugins into {}",
            dir.display()
        ))),
        None if plugins.indicators.is_empty() && plugins.strategies.is_empty() => {
            Some(note("Plugins aren't available in this session"))
        }
        None => None,
    }
}

fn symbols<'a>(query: &'a str, current: &'a str) -> Element<'a, MenuMessage> {
    let needle = query.trim().to_uppercase();
    let input = text_input("Search, e.g. ETHUSDT", query)
        .id(symbol_input())
        .on_input(MenuMessage::QueryChanged)
        .on_submit(MenuMessage::SubmitSymbol)
        .padding([7, 10])
        .size(13)
        .style(|_, status| input_style(status));

    let matches = SYMBOLS
        .into_iter()
        .filter(move |s| s.contains(&needle))
        .take(8)
        .map(move |s| {
            item(
                s,
                if s == current { "current" } else { "" },
                MenuMessage::SetSymbol(s.into()),
            )
        });

    column![back("Symbol"), container(input).padding([4, 6])]
        .extend(matches)
        .push(note("Enter loads any Binance spot pair"))
        .spacing(2)
        .into()
}

fn settings<'a>(
    spec: &'a IndicatorSpec,
    inputs: &'a [String],
    error: Option<&'a str>,
) -> Element<'a, MenuMessage> {
    let mut body = column![header(&spec.info.name)]
        .extend(param_rows(
            &spec.info.params,
            inputs,
            MenuMessage::ParamChanged,
        ))
        .spacing(8);
    if let Some(error) = error {
        body = body.push(text(error).size(11).color(theme::DOWN));
    }
    container(body.push(remove_button("Remove", MenuMessage::RemoveStudy)))
        .padding([6, 8])
        .into()
}

/// The strategy's settings, its latest backtest's statistics, and a remove button.
fn report<'a>(
    spec: &'a StrategySpec,
    run: &'a Run,
    quote: &'a str,
    inputs: &'a [String],
    error: Option<&'a str>,
) -> Element<'a, MenuMessage> {
    let mut body = column![header(&spec.info.name)]
        .extend(param_rows(
            &spec.info.params,
            inputs,
            MenuMessage::StrategyParamChanged,
        ))
        .spacing(8);
    if let Some(error) = error {
        body = body.push(text(error).size(11).color(theme::DOWN));
    }
    body = body.push(separator());
    body = match run {
        Run::Waiting => body.push(
            text("Waiting for this chart's bars…")
                .size(12)
                .color(theme::TEXT_MUTED),
        ),
        Run::Running => body.push(text("Backtesting…").size(12).color(theme::TEXT_MUTED)),
        Run::Failed(message) => body.push(text(message.as_str()).size(12).color(theme::DOWN)),
        Run::Done(report) => body.extend(statistics(report, quote)),
    };
    container(body.push(separator()).push(remove_button(
        "Remove strategy",
        MenuMessage::RemoveStrategy,
    )))
    .padding([6, 8])
    .into()
}

/// Label/value rows for a finished backtest.
fn statistics<'a>(report: &BacktestReport, quote: &str) -> Vec<Element<'a, MenuMessage>> {
    let signed = |v: f64| if v >= 0.0 { theme::UP } else { theme::DOWN };
    let stat = |name: &str| {
        report
            .stat(name)
            .map_or("—".to_string(), |v| format!("{v:.2}"))
    };
    let net = report.net_pnl();
    let win_rate = report
        .stat("Win Rate")
        .map_or("—".to_string(), |v| format!("{:.1}%", v * 100.0));
    let rows: [(&str, String, Color); 9] = [
        ("Net profit", format!("{net:+.2} {quote}"), signed(net)),
        (
            "Return",
            format!("{:+.2}%", report.net_return() * 100.0),
            signed(net),
        ),
        (
            "Max drawdown",
            format!("{:.2}%", report.max_drawdown * 100.0),
            theme::DOWN,
        ),
        (
            "Sharpe",
            stat("Sharpe Ratio (252 days)"),
            theme::TEXT_BRIGHT,
        ),
        (
            "Sortino",
            stat("Sortino Ratio (252 days)"),
            theme::TEXT_BRIGHT,
        ),
        ("Profit factor", stat("Profit Factor"), theme::TEXT_BRIGHT),
        ("Win rate", win_rate, theme::TEXT_BRIGHT),
        ("Trades", report.fills.len().to_string(), theme::TEXT_BRIGHT),
        (
            "Fees",
            format!("{:.2} {quote}", report.fees),
            theme::TEXT_BRIGHT,
        ),
    ];
    rows.into_iter()
        .map(|(label, value, color)| {
            row![
                text(label).size(12).color(theme::TEXT),
                space::horizontal(),
                text(value)
                    .size(12)
                    .color(color)
                    .font(iced::Font::MONOSPACE),
            ]
            .into()
        })
        .collect()
}

/// A popover title with a close button.
fn header(title: &str) -> Element<'_, MenuMessage> {
    row![
        text(title).size(13).color(theme::TEXT_BRIGHT),
        space::horizontal(),
        button(text("✕").size(12))
            .padding([2, 6])
            .on_press(MenuMessage::Close)
            .style(|_, status| item_style(status)),
    ]
    .align_y(iced::Center)
    .into()
}

/// One labelled input per parameter; edits go out as `on_change(index, text)`.
fn param_rows<'a>(
    params: &'a [Param],
    inputs: &'a [String],
    on_change: fn(usize, String) -> MenuMessage,
) -> Vec<Element<'a, MenuMessage>> {
    params
        .iter()
        .zip(inputs)
        .enumerate()
        .map(|(i, (param, value))| {
            row![
                text(param.name.as_ref())
                    .size(12)
                    .color(theme::TEXT)
                    .width(90),
                text_input("", value)
                    .on_input(move |v| on_change(i, v))
                    .padding([5, 8])
                    .size(13)
                    .style(|_, status| input_style(status)),
            ]
            .align_y(iced::Center)
            .spacing(8)
            .into()
        })
        .collect()
}

fn panel<'a>(content: Element<'a, MenuMessage>, width: f32) -> Element<'a, MenuMessage> {
    container(content)
        .padding(6)
        .width(width)
        .style(|_| container::Style {
            background: Some(Background::Color(PANEL)),
            border: Border {
                color: PANEL_BORDER,
                width: 1.0,
                radius: 6.0.into(),
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.55),
                offset: Vector::new(0.0, 6.0),
                blur_radius: 18.0,
            },
            text_color: Some(theme::TEXT_BRIGHT),
            ..container::Style::default()
        })
        .into()
}

fn item<'a>(label: &'a str, hint: &'a str, on_press: MenuMessage) -> Element<'a, MenuMessage> {
    button(
        row![
            text(label).size(13),
            space::horizontal(),
            text(hint).size(12).color(theme::TEXT_MUTED)
        ]
        .align_y(iced::Center),
    )
    .width(Fill)
    .padding([6, 10])
    .on_press(on_press)
    .style(|_, status| item_style(status))
    .into()
}

fn back(title: &str) -> Element<'_, MenuMessage> {
    button(
        text(format!("‹  {title}"))
            .size(13)
            .color(theme::TEXT_BRIGHT),
    )
    .width(Fill)
    .padding([6, 10])
    .on_press(MenuMessage::Open(Page::Root))
    .style(|_, status| item_style(status))
    .into()
}

fn section(title: &str) -> Element<'_, MenuMessage> {
    container(text(title.to_uppercase()).size(10).color(theme::TEXT_MUTED))
        .padding(iced::Padding {
            top: 6.0,
            right: 10.0,
            bottom: 2.0,
            left: 10.0,
        })
        .into()
}

fn note(content: &str) -> Element<'_, MenuMessage> {
    container(text(content).size(11).color(theme::TEXT_MUTED))
        .padding([4, 10])
        .into()
}

fn note_owned<'a>(content: String) -> Element<'a, MenuMessage> {
    container(text(content).size(11).color(theme::TEXT_MUTED))
        .padding([4, 10])
        .into()
}

fn separator<'a>() -> Element<'a, MenuMessage> {
    container(rule::horizontal(1).style(|_| rule::Style {
        color: PANEL_BORDER,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    }))
    .padding([4, 0])
    .into()
}

fn remove_button(label: &str, on_press: MenuMessage) -> Element<'_, MenuMessage> {
    button(text(label).size(12).center())
        .width(Fill)
        .padding([6, 0])
        .on_press(on_press)
        .style(|_, status| button::Style {
            background: Some(Background::Color(
                if matches!(status, button::Status::Hovered) {
                    Color {
                        a: 0.25,
                        ..theme::DOWN
                    }
                } else {
                    HOVER
                },
            )),
            text_color: theme::TEXT_BRIGHT,
            border: Border {
                radius: 4.0.into(),
                ..Border::default()
            },
            ..button::Style::default()
        })
        .into()
}

fn input_style(status: text_input::Status) -> text_input::Style {
    text_input::Style {
        background: Background::Color(theme::BACKGROUND),
        border: Border {
            color: if matches!(status, text_input::Status::Focused { .. }) {
                theme::ACCENT
            } else {
                PANEL_BORDER
            },
            width: 1.0,
            radius: 4.0.into(),
        },
        icon: theme::TEXT_MUTED,
        placeholder: theme::TEXT_MUTED,
        value: theme::TEXT_BRIGHT,
        selection: Color {
            a: 0.4,
            ..theme::ACCENT
        },
    }
}

fn item_style(status: button::Status) -> button::Style {
    button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then_some(Background::Color(HOVER)),
        text_color: theme::TEXT_BRIGHT,
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

fn chip(selected: bool, status: button::Status) -> button::Style {
    let background = if selected {
        theme::ACCENT
    } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        HOVER
    } else {
        Color::TRANSPARENT
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: if selected { Color::WHITE } else { theme::TEXT },
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}
