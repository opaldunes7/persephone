//! The left-click menu and the indicator settings popover. Both float above the tiles where
//! the click happened; the app owns what they act on.
//!
//! Menu pages:
//! - root: Indicators ›, Symbol ›, and a row of timeframe chips;
//! - indicators: every built-in, grouped by where it's drawn;
//! - symbol: a search box with popular Binance pairs.

use iced::{
    Background, Border, Color, Element, Fill, Point, Shadow, Size, Vector,
    widget::{Id, button, column, container, opaque, pin, row, rule, space, text, text_input},
};
use persephone_chart::theme;
use persephone_core::{
    Timeframe,
    indicator::{BuiltIn, IndicatorSpec, Placement},
};
use persephone_tiling::TileId;

pub const WIDTH: f32 = 248.0;
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
}

impl Popup {
    pub fn tile(&self) -> TileId {
        match self {
            Self::Menu { tile, .. } | Self::Settings { tile, .. } => *tile,
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

    fn at(&self) -> Point {
        match self {
            Self::Menu { at, .. } | Self::Settings { at, .. } => *at,
        }
    }

    /// A generous guess at the panel's height, for keeping it on screen.
    fn height(&self) -> f32 {
        match self {
            Self::Menu {
                page: Page::Root, ..
            } => 150.0,
            Self::Menu {
                page: Page::Indicators,
                ..
            } => 360.0,
            Self::Menu {
                page: Page::Symbol { .. },
                ..
            } => 330.0,
            Self::Settings { inputs, .. } => 110.0 + 40.0 * inputs.len() as f32,
        }
    }
}

#[derive(Clone, Debug)]
pub enum MenuMessage {
    Open(Page),
    AddIndicator(BuiltIn),
    SetTimeframe(Timeframe),
    QueryChanged(String),
    SetSymbol(String),
    SubmitSymbol,
    ParamChanged(usize, String),
    RemoveStudy,
    Close,
}

/// What the tile under the popup shows, for labels and the current selection.
pub struct TileInfo<'a> {
    pub symbol: &'a str,
    pub timeframe: Timeframe,
    pub specs: &'a [IndicatorSpec],
}

/// The popup, pinned at `origin` (the tile content's top-left in the window) plus its click
/// point, nudged to stay inside a `window`-sized area.
pub fn view<'a>(
    popup: &'a Popup,
    tile: TileInfo<'a>,
    origin: Point,
    window: Size,
) -> Element<'a, MenuMessage> {
    let content = match popup {
        Popup::Menu { page, .. } => match page {
            Page::Root => root(&tile),
            Page::Indicators => indicators(),
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
    };

    let at = popup.at();
    let x = (origin.x + at.x)
        .min(window.width - WIDTH - MARGIN)
        .max(MARGIN);
    let y = (origin.y + at.y)
        .min(window.height - popup.height() - MARGIN)
        .max(MARGIN);
    pin(opaque(panel(content))).x(x).y(y).into()
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

    column![
        item("Indicators", "›", MenuMessage::Open(Page::Indicators)),
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

fn indicators<'a>() -> Element<'a, MenuMessage> {
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
    column![back("Indicators"), section("On the chart")]
        .extend(group(Placement::Overlay))
        .push(section("In a pane below"))
        .extend(group(Placement::Pane))
        .spacing(2)
        .into()
}

fn symbols<'a>(query: &'a str, current: &'a str) -> Element<'a, MenuMessage> {
    let needle = query.trim().to_uppercase();
    let input = text_input("Search, e.g. ETHUSDT", query)
        .id(symbol_input())
        .on_input(MenuMessage::QueryChanged)
        .on_submit(MenuMessage::SubmitSymbol)
        .padding([7, 10])
        .size(13)
        .style(|_, status| text_input::Style {
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
        });

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
        .push(
            container(
                text("Enter loads any Binance spot pair")
                    .size(11)
                    .color(theme::TEXT_MUTED),
            )
            .padding([4, 10]),
        )
        .spacing(2)
        .into()
}

fn settings<'a>(
    spec: &'a IndicatorSpec,
    inputs: &'a [String],
    error: Option<&'a str>,
) -> Element<'a, MenuMessage> {
    let header = row![
        text(spec.kind.name()).size(13).color(theme::TEXT_BRIGHT),
        space::horizontal(),
        button(text("✕").size(12))
            .padding([2, 6])
            .on_press(MenuMessage::Close)
            .style(|_, status| item_style(status)),
    ]
    .align_y(iced::Center);

    let fields = spec
        .kind
        .params()
        .iter()
        .zip(inputs)
        .enumerate()
        .map(|(i, (param, value))| {
            row![
                text(param.name).size(12).color(theme::TEXT).width(80),
                text_input("", value)
                    .on_input(move |v| MenuMessage::ParamChanged(i, v))
                    .padding([5, 8])
                    .size(13)
                    .style(|_, status| text_input::Style {
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
                    }),
            ]
            .align_y(iced::Center)
            .spacing(8)
            .into()
        });

    let remove = button(text("Remove").size(12).center())
        .width(Fill)
        .padding([6, 0])
        .on_press(MenuMessage::RemoveStudy)
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
        });

    let mut body = column![header].extend(fields).spacing(8);
    if let Some(error) = error {
        body = body.push(text(error).size(11).color(theme::DOWN));
    }
    container(body.push(remove)).padding([6, 8]).into()
}

fn panel<'a>(content: Element<'a, MenuMessage>) -> Element<'a, MenuMessage> {
    container(content)
        .padding(6)
        .width(WIDTH)
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
            text(hint).size(12).color(theme::TEXT_MUTED),
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
