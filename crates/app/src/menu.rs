//! The right-click menu, the indicator settings popover and the strategy report. All float
//! above the tiles where they were opened; the app owns what they act on.
//!
//! The menu opens on a right click. The chart's header has two shortcuts of its own: clicking
//! the symbol turns it into a search box with a dropdown of matching markets, and clicking the
//! timeframe drops down every timeframe.
//!
//! Menu pages:
//! - root: what's active on the chart, each with a checkbox to show or hide it, then
//!   Indicators ›, Strategy ›, Draw ›, Symbol ›, and a row of timeframe chips;
//! - indicators: every built-in, grouped by where it's drawn, then loaded plugins;
//! - strategies: every built-in strategy and plugin, with a one-line description;
//! - symbol: a search box over markets in the local store and popular Binance pairs, with an
//!   Import row at the bottom and a way to delete a local market.
//!
//! The report opens from a chart's strategy summary: settings, full statistics, paper trading
//! on live bars, and a remove button. Dropping CSV files on a chart, or picking them from the
//! symbol search's Import row, opens the import panel: what was detected, the market to put the
//! ticks or bars in, and progress.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use iced::{
    Background, Border, Color, Element, Fill, Point, Shadow, Size, Vector,
    widget::{
        Id, button, checkbox, column, container, opaque, pin, row, rule, space, text, text_input,
    },
};
use persephone_chart::{Tool, theme};
use persephone_core::{
    BarSpec, Timeframe,
    indicator::{BuiltIn, IndicatorSpec, Param, Placement},
    strategy::{BuiltInStrategy, StrategyKind, StrategySpec},
    tick::TickKind,
    time::DateTime,
};
use persephone_data::{
    import::{Detection, Kind, Progress, Summary},
    store::{MarketInfo, MarketKind, Spacing},
};
use persephone_engine::{BacktestReport, live::LiveMode};
use persephone_plugin_host::Plugins;
use persephone_tiling::TileId;

use crate::{
    Hidden, Run,
    live::{Live, LiveState},
};

pub const WIDTH: f32 = 248.0;
pub const REPORT_WIDTH: f32 = 300.0;
const SEARCH_WIDTH: f32 = 280.0;
const TIMEFRAMES_WIDTH: f32 = 190.0;
pub const IMPORT_WIDTH: f32 = 440.0;
/// Room kept between a popup and the window edge.
const MARGIN: f32 = 6.0;

const PANEL: Color = Color::from_rgb8(0x1e, 0x22, 0x2d);
const PANEL_BORDER: Color = Color::from_rgb8(0x36, 0x3a, 0x45);
const HOVER: Color = Color::from_rgb8(0x2a, 0x2e, 0x39);

/// The most local markets the symbol page lists at once.
const LOCAL_SHOWN: usize = 8;

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
    Symbol {
        query: String,
        removing: Option<Removal>,
    },
    Draw,
    /// Tick and volume bars, for a local market: presets and a box for any count or size.
    Bars {
        input: String,
        error: Option<String>,
    },
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
    /// Setting up a sweep of the tile's strategy: which parameter, and its range.
    Sweep {
        tile: TileId,
        at: Point,
        param: usize,
        /// What's typed for from, to, and how many charts.
        inputs: [String; 3],
        error: Option<String>,
    },
    /// A drawing that was clicked: what it is, and a remove button.
    Drawing {
        tile: TileId,
        index: usize,
        at: Point,
    },
    /// The header's symbol, turned into a search box, with matching markets dropping down.
    SymbolSearch {
        tile: TileId,
        query: String,
        removing: Option<Removal>,
    },
    /// Every timeframe, dropping down from the header's timeframe.
    Timeframes {
        tile: TileId,
        at: Point,
    },
    /// CSV files dropped on the tile or picked for it, on their way into the local store.
    Import {
        tile: TileId,
        files: Vec<PathBuf>,
        state: ImportState,
        /// What's typed for the market's symbol and venue, and the zone of the file's times.
        symbol: String,
        venue: String,
        zone: String,
        error: Option<String>,
    },
}

/// A local market about to be deleted from the symbol search: its row asks first.
#[derive(Clone, Debug, PartialEq)]
pub struct Removal {
    pub venue: String,
    pub symbol: String,
    /// What its files take on disk.
    pub bytes: u64,
    pub deleting: bool,
    /// Why it can't be deleted, or why deleting failed.
    pub error: Option<String>,
}

/// Where an import of dropped files stands.
#[derive(Clone, Debug)]
pub enum ImportState {
    /// Reading the first file to work out its layout.
    Detecting,
    Ready(Arc<Detection>),
    Importing(Progress),
    Done(Arc<Summary>),
    Failed(String),
}

impl Popup {
    pub fn tile(&self) -> TileId {
        match self {
            Self::Menu { tile, .. }
            | Self::Settings { tile, .. }
            | Self::Report { tile, .. }
            | Self::Drawing { tile, .. }
            | Self::Sweep { tile, .. }
            | Self::SymbolSearch { tile, .. }
            | Self::Timeframes { tile, .. }
            | Self::Import { tile, .. } => *tile,
        }
    }

    /// The import panel for `files`, dropped on `tile` or picked for it, while the first one's
    /// layout is worked out.
    pub fn import(tile: TileId, files: Vec<PathBuf>, symbol: String) -> Self {
        Self::Import {
            tile,
            files,
            state: ImportState::Detecting,
            symbol,
            venue: persephone_data::import::DEFAULT_VENUE.into(),
            zone: "UTC".into(),
            error: None,
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

    /// Where the popup is pinned, in tile-content coordinates.
    #[must_use]
    pub fn at_point(&self) -> Point {
        self.at()
    }

    /// The local market the symbol search is asking to delete, for the search popups.
    pub fn removing_mut(&mut self) -> Option<&mut Option<Removal>> {
        match self {
            Self::Menu {
                page: Page::Symbol { removing, .. },
                ..
            }
            | Self::SymbolSearch { removing, .. } => Some(removing),
            _ => None,
        }
    }

    fn at(&self) -> Point {
        match self {
            Self::Menu { at, .. }
            | Self::Settings { at, .. }
            | Self::Report { at, .. }
            | Self::Drawing { at, .. }
            | Self::Sweep { at, .. }
            | Self::Timeframes { at, .. } => *at,
            // Over the header, so the search box's text sits where the symbol was.
            Self::SymbolSearch { .. } => Point::new(2.0, 1.0),
            Self::Import { .. } => Point::new(24.0, 48.0),
        }
    }

    fn width(&self) -> f32 {
        match self {
            Self::Report { .. } | Self::Sweep { .. } => REPORT_WIDTH,
            Self::Import { .. } => IMPORT_WIDTH,
            Self::SymbolSearch { .. } => SEARCH_WIDTH,
            Self::Timeframes { .. } => TIMEFRAMES_WIDTH,
            _ => WIDTH,
        }
    }

    /// A generous guess at the panel's height, for keeping it on screen. `extra` counts rows a
    /// page adds for plugins.
    fn height(&self, extra: usize) -> f32 {
        let extra = 22.0 * extra as f32;
        match self {
            Self::Menu {
                page: Page::Root, ..
            } => 195.0 + extra,
            Self::SymbolSearch { .. } => 60.0 + extra,
            Self::Timeframes { .. } => 180.0 + extra,
            Self::Menu {
                page: Page::Indicators,
                ..
            } => 312.0 + extra,
            Self::Menu {
                page: Page::Strategies,
                ..
            } => 200.0 + extra,
            Self::Menu {
                page: Page::Symbol { .. },
                ..
            } => 245.0,
            Self::Settings { inputs, .. } => 85.0 + 30.0 * inputs.len() as f32,
            Self::Report { inputs, .. } => 265.0 + 30.0 * inputs.len() as f32 + extra,
            Self::Import { .. } => 230.0 + extra,
            Self::Menu {
                page: Page::Draw, ..
            } => 170.0,
            Self::Menu {
                page: Page::Bars { .. },
                ..
            } => 195.0,
            Self::Drawing { .. } => 80.0,
            Self::Sweep { .. } => 225.0,
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
    /// Tick or volume bars, from a preset or what was typed on the bars page.
    SetBars(BarSpec),
    BarsInput(String),
    SubmitBars,
    QueryChanged(String),
    SetSymbol(String),
    /// A market from the local store.
    SetLocal {
        venue: String,
        symbol: String,
    },
    SubmitSymbol,
    /// Pick files to import from the symbol search.
    PickFiles,
    /// Ask before deleting a local market, then delete it or keep it.
    AskRemoveLocal {
        venue: String,
        symbol: String,
    },
    RemoveLocal,
    KeepLocal,
    /// The import panel's symbol and venue boxes, and its Import button.
    ImportSymbolChanged(String),
    ImportVenueChanged(String),
    ImportZoneChanged(String),
    StartImport,
    /// Lets the tile's strategy plugin ask language models, or stops it.
    AllowAi(bool),
    /// Start drawing with a tool on the tile.
    Draw(Tool),
    /// The sweep popup: open it, pick the parameter, edit from / to / charts, and go.
    OpenSweep,
    SweepParam(usize),
    SweepInput(usize, String),
    StartSweep,
    /// Remove the clicked drawing, or every drawing on the tile's market.
    RemoveDrawing,
    ClearDrawings,
    ParamChanged(usize, String),
    RemoveStudy,
    SetStrategy(BuiltInStrategy),
    /// A plugin strategy, by id.
    SetPluginStrategy(String),
    StrategyParamChanged(usize, String),
    RemoveStrategy,
    /// Show (`true`) or hide something in the Active list.
    Show(Active, bool),
    /// Open the backtest panel for the tile.
    OpenBacktest,
    /// Trade the tile's strategy on live bars, or stop.
    GoLive(LiveMode),
    StopLive,
    Close,
}

/// Something on a chart that the Active list can hide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Active {
    /// The volume bars.
    Volume,
    /// The indicator at this index.
    Indicator(usize),
    Strategy,
    Drawings,
}

/// What the tile under the popup shows, for labels and the current selection.
pub struct TileInfo<'a> {
    pub symbol: &'a str,
    /// The local store's venue, for a stored market.
    pub venue: Option<&'a str>,
    /// Quote currency for money figures, and the base asset for positions.
    pub quote: &'a str,
    pub base: &'a str,
    /// How the market's backtests fill orders, when worth saying.
    pub fills: Option<String>,
    /// Markets in the local store, for the symbol page.
    pub local_markets: &'a [MarketInfo],
    /// For a strategy plugin that asks language models: whether it may.
    pub ai_allowed: Option<bool>,
    /// The tile's market's drawings.
    pub drawings: &'a [persephone_chart::Drawing],
    /// The workspace a new sweep would open on, if one is free.
    pub free_workspace: Option<u8>,
    /// The sweep this tile belongs to: the parameter's name and each value's outcome.
    pub sweep: Option<(&'a str, Vec<SweepRow>)>,
    /// How the tile's bars are cut.
    pub bars: BarSpec,
    /// For a local market, what its ticks are (so which bars it can have).
    pub tick_kind: Option<TickKind>,
    /// For a market imported as bars, how far apart they are (so which timeframes it can show).
    pub spacing: Option<Spacing>,
    pub specs: &'a [IndicatorSpec],
    pub strategy: Option<(&'a StrategySpec, &'a Run)>,
    /// Trading the strategy live: the session, if one ran, or why the chart can't.
    pub live: LiveInfo<'a>,
    /// What the Active list has hidden.
    pub hidden: &'a Hidden,
    pub plugins: &'a Plugins,
    /// Where plugin files go, to tell people.
    pub plugin_dir: Option<&'a Path>,
}

#[derive(Clone, Copy, Default)]
pub struct LiveInfo<'a> {
    pub session: Option<&'a Live>,
    /// Why the chart can't trade live, if it can't.
    pub unavailable: Option<&'static str>,
    /// Binance testnet keys are set.
    pub testnet: bool,
}

/// One value of a sweep, for the report's comparison.
pub struct SweepRow {
    pub value: String,
    /// The return, once its backtest is done.
    pub result: SweepResult,
    /// It's the tile the report is open on.
    pub current: bool,
}

pub enum SweepResult {
    Running,
    Return(f64),
    Failed,
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
            Page::Symbol { query, removing } => symbols(query, removing.as_ref(), &tile),
            Page::Draw => draw_page(tile.drawings.len()),
            Page::Bars { input, error } => bars_page(&tile, input, error.as_deref()),
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
            Some((spec, run)) => report(spec, run, &tile, inputs, error.as_deref()),
            None => column![].into(),
        },
        Popup::Import {
            files,
            state,
            symbol,
            venue,
            zone,
            error,
            ..
        } => {
            let existing = tile
                .local_markets
                .iter()
                .find(|m| &m.symbol == symbol && &m.venue == venue);
            import_panel(
                files,
                state,
                [symbol, venue, zone],
                existing,
                error.as_deref(),
            )
        }
        Popup::Sweep {
            param,
            inputs,
            error,
            ..
        } => match tile.strategy {
            Some((spec, _)) => {
                sweep_panel(spec, *param, inputs, error.as_deref(), tile.free_workspace)
            }
            None => column![].into(),
        },
        Popup::SymbolSearch {
            query, removing, ..
        } => symbol_search(query, removing.as_ref(), &tile),
        Popup::Timeframes { .. } => timeframes(&tile),
        Popup::Drawing { index, .. } => match tile.drawings.get(*index) {
            Some(drawing) => container(
                column![
                    header(drawing.name()),
                    remove_button("Remove drawing", MenuMessage::RemoveDrawing)
                ]
                .spacing(5),
            )
            .padding([4, 6])
            .into(),
            None => column![].into(),
        },
    };

    let extra = match popup {
        Popup::Menu {
            page: Page::Root, ..
        } => match active_count(&tile) {
            0 => 0,
            n => n + 2,
        },
        Popup::Timeframes { .. } => usize::from(tile.tick_kind.is_some()),
        Popup::SymbolSearch { .. } => tile.local_markets.len().min(LOCAL_SHOWN) + 14,
        Popup::Menu {
            page: Page::Indicators,
            ..
        } => tile.plugins.indicators.len() + tile.plugins.errors.len() + 2,
        Popup::Menu {
            page: Page::Strategies,
            ..
        } => 2 * tile.plugins.strategies.len() + 2,
        Popup::Menu {
            page: Page::Symbol { .. },
            ..
        } => tile.local_markets.len().min(LOCAL_SHOWN) + 4,
        Popup::Import {
            state: ImportState::Ready(detection),
            ..
        } => detection.roles().len() + detection.notes.len() + 2,
        // Sweep rows, notes (about two lines each) and the AI permission text.
        Popup::Report { .. } => {
            let sweep = tile.sweep.as_ref().map_or(0, |(_, rows)| rows.len() + 2);
            let notes = match tile.strategy {
                Some((_, Run::Done(report))) => 2 * report.notes.len(),
                _ => 0,
            };
            let ai = match tile.ai_allowed {
                Some(false) => 4,
                Some(true) => 1,
                None => 0,
            };
            let live = match tile.live.session {
                Some(live) if live.is_active() => 4,
                Some(_) => 5,
                None => 2,
            };
            sweep + notes + ai + live
        }
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
    // The search box brings its own dropdown panel.
    let content = match popup {
        Popup::SymbolSearch { .. } => content,
        _ => panel(content, width),
    };
    pin(opaque(content)).x(x).y(y).into()
}

/// How many rows the Active list has.
fn active_count(tile: &TileInfo<'_>) -> usize {
    1 + tile.specs.len()
        + usize::from(tile.strategy.is_some())
        + usize::from(!tile.drawings.is_empty())
}

/// Everything on the chart, each with a checkbox that shows or hides it.
fn active<'a>(tile: &TileInfo<'a>) -> Vec<Element<'a, MenuMessage>> {
    let toggle = |label: String, shown: bool, what: Active| -> Element<'a, MenuMessage> {
        container(
            checkbox(shown)
                .label(label)
                .size(12)
                .text_size(12)
                .spacing(5)
                .on_toggle(move |on| MenuMessage::Show(what, on)),
        )
        .padding([2, 8])
        .into()
    };
    let volume = toggle("Volume".into(), !tile.hidden.volume, Active::Volume);
    let mut rows: Vec<Element<'a, MenuMessage>> = std::iter::once(volume)
        .chain(tile.specs.iter().enumerate().map(|(i, spec)| {
            toggle(
                spec.to_string(),
                !tile.hidden.indicators.contains(&i),
                Active::Indicator(i),
            )
        }))
        .collect();
    if let Some((spec, _)) = tile.strategy {
        rows.push(toggle(
            spec.to_string(),
            !tile.hidden.strategy,
            Active::Strategy,
        ));
    }
    if !tile.drawings.is_empty() {
        rows.push(toggle(
            format!("Drawings ({})", tile.drawings.len()),
            !tile.hidden.drawings,
            Active::Drawings,
        ));
    }
    rows
}

/// The header's timeframe dropdown: every timeframe, the current one ticked, and for local
/// markets the tick and volume bars.
fn timeframes<'a>(tile: &TileInfo<'a>) -> Element<'a, MenuMessage> {
    let mut page = column(Timeframe::ALL.into_iter().map(|tf| {
        let current = BarSpec::Time(tf) == tile.bars;
        let hint = if current {
            "✓".into()
        } else {
            tf.label().to_owned()
        };
        if allowed(tile, tf) {
            local_item(timeframe_name(tf), hint, MenuMessage::SetTimeframe(tf))
        } else {
            disabled_item(timeframe_name(tf), hint)
        }
    }));
    if tile.tick_kind.is_some() {
        let current = match tile.bars {
            BarSpec::Time(_) => "›".to_owned(),
            other => format!("{}  ›", other.label()),
        };
        page = page.push(local_item(
            "Tick & volume bars",
            current,
            MenuMessage::Open(Page::Bars {
                input: String::new(),
                error: None,
            }),
        ));
    }
    page.spacing(2).into()
}

const fn timeframe_name(tf: Timeframe) -> &'static str {
    match tf {
        Timeframe::M1 => "1 minute",
        Timeframe::M5 => "5 minutes",
        Timeframe::M15 => "15 minutes",
        Timeframe::M30 => "30 minutes",
        Timeframe::H1 => "1 hour",
        Timeframe::H4 => "4 hours",
        Timeframe::D1 => "1 day",
        Timeframe::W1 => "1 week",
        Timeframe::MN1 => "1 month",
    }
}

/// Whether the tile's market can show `tf`: a market imported as bars only shows the
/// timeframes its bars make.
fn allowed(tile: &TileInfo<'_>, tf: Timeframe) -> bool {
    tile.spacing.is_none_or(|spacing| spacing.allows(tf))
}

fn root<'a>(tile: &TileInfo<'a>) -> Element<'a, MenuMessage> {
    let chips = row(Timeframe::ALL.into_iter().map(|tf| {
        let selected = BarSpec::Time(tf) == tile.bars;
        let chip_button = button(text(tf.label()).size(11).center())
            .padding([2, 0])
            .width(Fill)
            .style(move |_, status| chip(selected, status));
        if allowed(tile, tf) {
            chip_button.on_press(MenuMessage::SetTimeframe(tf)).into()
        } else {
            chip_button.into()
        }
    }))
    .spacing(2);

    let strategy = tile
        .strategy
        .map_or("None", |(spec, _)| spec.info.name.as_str());
    let active = active(tile);
    let mut page = column![];
    if !active.is_empty() {
        page = page
            .push(section("Active"))
            .extend(active)
            .push(separator());
    }
    let mut page = page.extend([
        item("Indicators", "›", MenuMessage::Open(Page::Indicators)),
        item("Strategy", strategy, MenuMessage::Open(Page::Strategies)),
        item("Draw", "›", MenuMessage::Open(Page::Draw)),
        item(
            "Symbol",
            tile.symbol,
            MenuMessage::Open(Page::Symbol {
                query: String::new(),
                removing: None,
            }),
        ),
        separator(),
        section("Timeframe"),
        container(chips).padding([2, 6]).into(),
    ]);
    // Local markets have ticks, so they can also be cut by tick count or volume.
    if tile.tick_kind.is_some() {
        let current = match tile.bars {
            BarSpec::Time(_) => "›".to_owned(),
            other => format!("{}  ›", other.label()),
        };
        page = page.push(local_item(
            "Tick & volume bars",
            current,
            MenuMessage::Open(Page::Bars {
                input: String::new(),
                error: None,
            }),
        ));
    }
    page.spacing(2).into()
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
        .push(item(
            "Volume",
            "Vol",
            MenuMessage::Show(Active::Volume, true),
        ))
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
            kind.description().to_owned(),
            selected,
            MenuMessage::SetStrategy(kind),
        )
    });
    let from_plugins = plugins.strategies.values().map(move |plugin| {
        let selected = current == Some(&StrategyKind::Plugin(plugin.id.clone()));
        // Worth knowing before picking it: it sends prompts out, or runs elsewhere.
        let description = match plugin.address() {
            _ if plugin.uses_ai => format!("{} · asks AI models", plugin.info.description),
            Some(address) => format!("{} · runs at {address}", plugin.info.description),
            None => plugin.info.description.clone(),
        };
        strategy_choice(
            &plugin.info.name,
            description,
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
    description: String,
    selected: bool,
    on_press: MenuMessage,
) -> Element<'a, MenuMessage> {
    button(
        column![
            row![
                text(name).size(12),
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
    .padding([3, 8])
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

fn symbols<'a>(
    query: &'a str,
    removing: Option<&'a Removal>,
    tile: &TileInfo<'a>,
) -> Element<'a, MenuMessage> {
    let input = text_input("Search, e.g. ETHUSDT", query)
        .id(symbol_input())
        .on_input(MenuMessage::QueryChanged)
        .on_submit(MenuMessage::SubmitSymbol)
        .padding([4, 8])
        .size(12)
        .style(|_, status| input_style(status));
    column![back("Symbol"), container(input).padding([4, 6])]
        .extend(symbol_matches(query, removing, tile))
        .spacing(2)
        .into()
}

/// The header's symbol as a search box: typing narrows the dropdown under it, a click picks
/// a market, and Enter opens whatever Binance pair is typed.
fn symbol_search<'a>(
    query: &'a str,
    removing: Option<&'a Removal>,
    tile: &TileInfo<'a>,
) -> Element<'a, MenuMessage> {
    let input = text_input(tile.symbol, query)
        .id(symbol_input())
        .on_input(MenuMessage::QueryChanged)
        .on_submit(MenuMessage::SubmitSymbol)
        .padding([6, 9])
        .size(theme::LEGEND_SIZE)
        .width(SEARCH_WIDTH)
        .style(|_, status| input_style(status));
    let matches = column(symbol_matches(query, removing, tile)).spacing(2);
    // Touching the box, so the price line under it doesn't show through a gap.
    column![input, panel(matches.into(), SEARCH_WIDTH)]
        .spacing(1)
        .into()
}

/// Markets matching `query`: local ones first (when there are any), each with a button to
/// delete it, then Binance pairs, a note that Enter opens any pair, and the Import row.
fn symbol_matches<'a>(
    query: &str,
    removing: Option<&'a Removal>,
    tile: &TileInfo<'a>,
) -> Vec<Element<'a, MenuMessage>> {
    let needle = query.trim().to_uppercase();
    let (current, venue) = (tile.symbol, tile.venue);
    let matching: Vec<&MarketInfo> = tile
        .local_markets
        .iter()
        .filter(|m| m.symbol.contains(&needle) || m.venue.contains(&needle))
        .collect();
    let mut local: Vec<Element<'a, MenuMessage>> = matching
        .iter()
        .take(LOCAL_SHOWN)
        .map(|m| match removing {
            Some(removal) if removal.symbol == m.symbol && removal.venue == m.venue => {
                confirm_removal(removal)
            }
            _ => local_row(m, m.symbol == current && Some(m.venue.as_str()) == venue),
        })
        .collect();
    if matching.len() > LOCAL_SHOWN {
        local.push(note_owned(format!(
            "{} more; type to narrow",
            matching.len() - LOCAL_SHOWN
        )));
    }

    let binance = SYMBOLS
        .into_iter()
        .filter(|s| s.contains(&needle))
        .take(8)
        .map(move |s| {
            let selected = s == current && venue.is_none();
            item(
                s,
                if selected { "current" } else { "" },
                MenuMessage::SetSymbol(s.into()),
            )
        });

    let mut rows = Vec::new();
    if !local.is_empty() {
        rows.push(section("Local"));
        rows.extend(local);
        rows.push(section("Binance"));
    }
    rows.extend(binance);
    rows.push(note("Enter loads any Binance spot pair"));
    rows.push(separator());
    rows.push(item(
        "Import data…",
        "ticks or bars · csv, gz, zip",
        MenuMessage::PickFiles,
    ));
    rows
}

/// A local market: its symbol, where it's from and what it holds, and a button to delete it.
fn local_row<'a>(m: &'a MarketInfo, selected: bool) -> Element<'a, MenuMessage> {
    let hint = if selected {
        "current".to_owned()
    } else {
        format!(
            "{} · {} {}",
            m.venue,
            m.kind,
            DateTime::from_nanos(m.first).year
        )
    };
    let open = local_item(
        &m.symbol,
        hint,
        MenuMessage::SetLocal {
            venue: m.venue.clone(),
            symbol: m.symbol.clone(),
        },
    );
    let remove = button(text("×").size(13).center())
        .width(22)
        .padding([1, 0])
        .on_press(MenuMessage::AskRemoveLocal {
            venue: m.venue.clone(),
            symbol: m.symbol.clone(),
        })
        .style(|_, status| button::Style {
            background: matches!(status, button::Status::Hovered | button::Status::Pressed)
                .then_some(Background::Color(HOVER)),
            text_color: match status {
                button::Status::Hovered | button::Status::Pressed => theme::DOWN,
                _ => theme::TEXT_MUTED,
            },
            border: Border {
                radius: 3.0.into(),
                ..Border::default()
            },
            ..button::Style::default()
        });
    row![open, remove].spacing(2).align_y(iced::Center).into()
}

/// A local market's row while deleting it is being asked about.
fn confirm_removal<'a>(removal: &'a Removal) -> Element<'a, MenuMessage> {
    let id = format!("{}.{}", removal.symbol, removal.venue);
    let question = match (&removal.error, removal.deleting) {
        (Some(error), _) => text(error.as_str()).size(11).color(theme::DOWN),
        (None, true) => text(format!("Deleting {id}…"))
            .size(11)
            .color(theme::TEXT_MUTED),
        (None, false) => text(format!(
            "Delete {id} ({})? The files you imported from stay.",
            bytes(removal.bytes)
        ))
        .size(11)
        .color(theme::TEXT_BRIGHT),
    };
    let small = |label: &'a str, on_press: Option<MenuMessage>, danger: bool| {
        button(text(label).size(11).center())
            .padding([2, 10])
            .on_press_maybe(on_press)
            .style(move |_, status| {
                if danger {
                    remove_style(status)
                } else {
                    chip(false, status)
                }
            })
    };
    let can_delete = removal.error.is_none() && !removal.deleting;
    let buttons = row![
        space::horizontal(),
        small("Keep", Some(MenuMessage::KeepLocal), false),
        small(
            "Delete",
            can_delete.then_some(MenuMessage::RemoveLocal),
            true
        ),
    ]
    .spacing(5);
    container(column![question, buttons].spacing(4))
        .padding([4, 8])
        .style(|_| container::Style {
            border: Border {
                color: PANEL_BORDER,
                width: 1.0,
                radius: 3.0.into(),
            },
            ..container::Style::default()
        })
        .into()
}

/// `1536` → `1.5 KB`, and so on up to gigabytes.
fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A menu item that can't be picked, greyed out.
fn disabled_item<'a>(label: &'a str, hint: String) -> Element<'a, MenuMessage> {
    container(
        row![
            text(label).size(12).color(theme::TEXT_MUTED),
            space::horizontal(),
            text(hint).size(11).color(theme::TEXT_MUTED),
        ]
        .align_y(iced::Center),
    )
    .width(Fill)
    .padding([3, 8])
    .into()
}

/// A menu item whose hint is built for it.
fn local_item<'a>(label: &'a str, hint: String, on_press: MenuMessage) -> Element<'a, MenuMessage> {
    button(
        row![
            text(label).size(12),
            space::horizontal(),
            text(hint).size(11).color(theme::TEXT_MUTED),
        ]
        .align_y(iced::Center),
    )
    .width(Fill)
    .padding([3, 8])
    .on_press(on_press)
    .style(|_, status| item_style(status))
    .into()
}

/// Which parameter to sweep, over what range, and how many charts.
fn sweep_panel<'a>(
    spec: &'a StrategySpec,
    param: usize,
    inputs: &'a [String; 3],
    error: Option<&'a str>,
    free_workspace: Option<u8>,
) -> Element<'a, MenuMessage> {
    let chips = row(spec.info.params.iter().enumerate().map(|(i, p)| {
        button(text(p.name.as_ref()).size(12).center())
            .padding([4, 8])
            .on_press(MenuMessage::SweepParam(i))
            .style(move |_, status| chip(i == param, status))
            .into()
    }))
    .spacing(4)
    .wrap();
    let field = |i: usize, label: &'a str| {
        column![
            text(label).size(11).color(theme::TEXT_MUTED),
            text_input("", &inputs[i])
                .on_input(move |v| MenuMessage::SweepInput(i, v))
                .on_submit(MenuMessage::StartSweep)
                .padding([3, 6])
                .size(12)
                .style(|_, status| input_style(status)),
        ]
        .spacing(3)
    };
    let parsed = (
        inputs[0].trim().parse::<f64>(),
        inputs[1].trim().parse::<f64>(),
        inputs[2].trim().parse::<usize>(),
    );
    let preview = match (parsed, spec.info.params.get(param)) {
        ((Ok(from), Ok(to), Ok(charts)), Some(p)) => {
            crate::sweep::values(p, from, to, charts).map(|values| {
                values
                    .iter()
                    .map(|v| crate::sweep::label(*v))
                    .collect::<Vec<_>>()
            })
        }
        _ => Err("type numbers for from, to and charts".to_owned()),
    };

    let mut body = column![
        header(&spec.info.name),
        text("Sweep").size(11).color(theme::TEXT_MUTED),
        chips,
        row![field(0, "From"), field(1, "To"), field(2, "Charts")].spacing(5),
    ]
    .spacing(5);
    body = match &preview {
        Ok(values) => body.push(
            text(format!(
                "{}: {}",
                spec.info.params[param].name,
                values.join(", ")
            ))
            .size(12)
            .color(theme::TEXT_BRIGHT),
        ),
        Err(message) => body.push(text(message.clone()).size(11).color(theme::DOWN)),
    };
    if let Some(error) = error {
        body = body.push(text(error).size(11).color(theme::DOWN));
    }
    let go = match (free_workspace, &preview) {
        (Some(n), Ok(values)) => button(
            text(format!("Open {} charts on workspace {n}", values.len()))
                .size(12)
                .width(Fill)
                .center(),
        )
        .on_press(MenuMessage::StartSweep),
        (None, _) => button(
            text("Every workspace is in use")
                .size(12)
                .width(Fill)
                .center(),
        ),
        (_, Err(_)) => button(text("Open charts").size(12).width(Fill).center()),
    };
    container(
        body.push(
            go.width(Fill)
                .padding([3, 0])
                .style(|_, status| chip(true, status)),
        ),
    )
    .padding([4, 6])
    .into()
}

/// Tick and volume bars for a local market: tick presets and a box for any count or size.
fn bars_page<'a>(
    tile: &TileInfo<'a>,
    input: &'a str,
    error: Option<&'a str>,
) -> Element<'a, MenuMessage> {
    let current = tile.bars;
    let chips = row([100, 500, 1_000, 5_000].into_iter().map(|n| {
        let bars = BarSpec::Ticks(n);
        button(text(bars.label()).size(12).center())
            .padding([2, 0])
            .width(Fill)
            .on_press(MenuMessage::SetBars(bars))
            .style(move |_, status| chip(bars == current, status))
            .into()
    }))
    .spacing(3);
    let trades = tile.tick_kind == Some(TickKind::Trades);
    let placeholder = if trades {
        "e.g. 2000t or 2.5v"
    } else {
        "e.g. 2000t"
    };
    let input = text_input(placeholder, input)
        .on_input(MenuMessage::BarsInput)
        .on_submit(MenuMessage::SubmitBars)
        .padding([4, 8])
        .size(12)
        .style(|_, status| input_style(status));
    let mut page = column![
        back("Tick & volume bars"),
        section("A bar every"),
        container(chips).padding([2, 6]),
        container(input).padding([4, 6]),
    ];
    if let Some(error) = error {
        page = page.push(container(text(error).size(11).color(theme::DOWN)).padding([0, 10]));
    }
    let explain = if trades {
        "Type ticks (1000t) or traded size (2.5v), then Enter"
    } else {
        "Type a number of ticks (1000t), then Enter"
    };
    page.push(note(explain)).spacing(2).into()
}

/// The drawing tools, and a way to clear the market's drawings.
fn draw_page<'a>(drawings: usize) -> Element<'a, MenuMessage> {
    let tools = Tool::ALL
        .into_iter()
        .map(|tool| item(tool.name(), tool.hint(), MenuMessage::Draw(tool)));
    let mut page = column![back("Draw")].extend(tools);
    if drawings > 0 {
        page = page.push(separator()).push(item(
            "Remove all drawings",
            "",
            MenuMessage::ClearDrawings,
        ));
    }
    page.push(note("Esc stops drawing; click a drawing to remove it"))
        .spacing(2)
        .into()
}

fn import_panel<'a>(
    files: &'a [PathBuf],
    state: &'a ImportState,
    [symbol, venue, zone]: [&'a str; 3],
    existing: Option<&'a MarketInfo>,
    error: Option<&'a str>,
) -> Element<'a, MenuMessage> {
    let name = files[0]
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let files_line = match files.len() {
        1 => name,
        n => format!("{name} and {} more", n - 1),
    };
    let muted = |content: String| text(content).size(11).color(theme::TEXT_MUTED);
    let mut body = column![header("Import data"), muted(files_line)].spacing(5);

    body = match state {
        ImportState::Detecting => body.push(muted("Reading the file…".into())),
        ImportState::Failed(message) => body
            .push(text(message.as_str()).size(12).color(theme::DOWN))
            .push(muted(
                "Files that need columns named by hand can go through `persephone-cli import`."
                    .into(),
            )),
        ImportState::Ready(detection) => {
            body = body.push(text(detection.summary()).size(12).color(theme::TEXT_BRIGHT));
            for (role, i) in detection.roles() {
                let name = detection.names.get(i).map_or("", String::as_str);
                let example = detection.example.get(i).map_or("", String::as_str);
                body = body.push(
                    row![
                        text(detection.role_name(role))
                            .size(12)
                            .width(70)
                            .color(theme::TEXT_MUTED),
                        text(name).size(12).width(120).color(theme::TEXT_BRIGHT),
                        text(example)
                            .size(12)
                            .font(iced::Font::MONOSPACE)
                            .color(theme::TEXT_BRIGHT),
                    ]
                    .spacing(5),
                );
            }
            for note in &detection.notes {
                body = body.push(text(note.as_str()).size(11).color(theme::TEXT_MUTED));
            }
            let field = |placeholder, value, on_input: fn(String) -> MenuMessage| {
                text_input(placeholder, value)
                    .on_input(on_input)
                    .on_submit(MenuMessage::StartImport)
                    .padding([3, 6])
                    .size(12)
                    .style(|_, status| input_style(status))
            };
            let mut fields = row![
                column![
                    muted("Symbol".into()),
                    field("EURUSD", symbol, MenuMessage::ImportSymbolChanged)
                ]
                .spacing(3),
                column![
                    muted("Venue".into()),
                    field("LOCAL", venue, MenuMessage::ImportVenueChanged)
                ]
                .spacing(3),
            ]
            .spacing(10);
            if !detection.format.time_format.is_epoch() {
                fields = fields.push(
                    column![
                        muted("Time zone".into()),
                        field("UTC", zone, MenuMessage::ImportZoneChanged)
                    ]
                    .spacing(3),
                );
            }
            if detection.zone_assumed {
                body = body.push(muted(
                    "The times don't say their zone: UTC unless you give one (e.g. -05:00 or America/New_York).".into(),
                ));
            }
            body = body.push(fields);
            if let Some(market) = existing {
                body = body.push(adds_to(market, detection));
            }
            if let Some(error) = error {
                body = body.push(text(error).size(11).color(theme::DOWN));
            }
            body.push(
                button(text("Import").size(12).width(Fill).center())
                    .width(Fill)
                    .padding([3, 0])
                    .on_press(MenuMessage::StartImport)
                    .style(|_, status| chip(true, status)),
            )
        }
        ImportState::Importing(progress) => {
            let percent = if progress.total > 0 {
                progress.read as f64 / progress.total as f64 * 100.0
            } else {
                0.0
            };
            body.push(muted(format!(
                "Importing… {percent:.0}% · {} rows",
                thousands(progress.rows)
            )))
        }
        ImportState::Done(summary) => {
            let market = &summary.market;
            body = body.push(
                text(format!(
                    "Imported {} {} into {}, {} → {}.",
                    thousands(summary.imported),
                    market.info.kind,
                    market.id(),
                    DateTime::from_nanos(summary.first).date(),
                    DateTime::from_nanos(summary.last).date(),
                ))
                .size(12)
                .color(theme::TEXT_BRIGHT),
            );
            if summary.skipped > 0 {
                body = body.push(muted(format!(
                    "Skipped {} rows that didn't fit:",
                    thousands(summary.skipped)
                )));
                for problem in summary.problems.iter().take(3) {
                    body = body.push(text(problem.as_str()).size(11).color(theme::TEXT_MUTED));
                }
            }
            body.push(muted(
                "It's on this chart now, and in the symbol search under Local.".into(),
            ))
        }
    };
    container(body).padding([4, 6]).into()
}

/// What importing into the existing `market` does: adds to it, or can't, as it holds
/// something else.
fn adds_to<'a>(market: &MarketInfo, detection: &Detection) -> Element<'a, MenuMessage> {
    let id = format!("{}.{}", market.symbol, market.venue);
    let files = match (detection.format.kind, detection.spacing) {
        (Kind::Bars, Some(spacing)) => Some(MarketKind::Bars(spacing)),
        (Kind::Bars, None) => None,
        (kind, _) => kind.ticks().map(MarketKind::Ticks),
    };
    match files {
        Some(files) if files != market.kind => text(format!(
            "{id} holds {}, but these files have {files}; give them another symbol or venue.",
            market.kind
        ))
        .size(11)
        .color(theme::DOWN),
        _ => text(format!(
            "Adds to {id} ({} → {}); times already there are replaced.",
            DateTime::from_nanos(market.first).date(),
            DateTime::from_nanos(market.last).date(),
        ))
        .size(11)
        .color(theme::TEXT_MUTED),
    }
    .into()
}

/// `1234567` → `1,234,567`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
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
        .spacing(5);
    if let Some(error) = error {
        body = body.push(text(error).size(11).color(theme::DOWN));
    }
    container(body.push(remove_button("Remove", MenuMessage::RemoveStudy)))
        .padding([4, 6])
        .into()
}

/// The strategy's settings, its latest backtest's statistics, and a remove button.
fn report<'a>(
    spec: &'a StrategySpec,
    run: &'a Run,
    tile: &TileInfo<'a>,
    inputs: &'a [String],
    error: Option<&'a str>,
) -> Element<'a, MenuMessage> {
    let quote = tile.quote;
    let mut body = column![header(&spec.info.name)]
        .extend(param_rows(
            &spec.info.params,
            inputs,
            MenuMessage::StrategyParamChanged,
        ))
        .spacing(5);
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
        Run::Running => body.push(text("Backtesting…").size(11).color(theme::TEXT_MUTED)),
        Run::Failed(message) => body.push(text(message.as_str()).size(12).color(theme::DOWN)),
        Run::Done(report) => body.extend(statistics(report, quote)),
    };
    if let Some((name, rows)) = &tile.sweep {
        body = body.push(separator()).push(
            text(format!("Sweep of {name}"))
                .size(12)
                .color(theme::TEXT_BRIGHT),
        );
        for row in rows {
            let (result, color) = match row.result {
                SweepResult::Running => ("…".to_owned(), theme::TEXT_MUTED),
                SweepResult::Failed => ("failed".to_owned(), theme::DOWN),
                SweepResult::Return(r) => (
                    format!("{:+.2}%", r * 100.0),
                    if r >= 0.0 { theme::UP } else { theme::DOWN },
                ),
            };
            let marker = if row.current { "  ◂" } else { "" };
            body = body.push(row![
                text(format!("{}{marker}", row.value))
                    .size(12)
                    .font(iced::Font::MONOSPACE)
                    .color(theme::TEXT),
                space::horizontal(),
                text(result)
                    .size(12)
                    .font(iced::Font::MONOSPACE)
                    .color(color),
            ]);
        }
    }
    if tile.sweep.is_none() && !spec.info.params.is_empty() {
        body = body.push(
            button(text("Sweep a parameter…").size(11).color(theme::TEXT_MUTED))
                .padding([2, 0])
                .on_press(MenuMessage::OpenSweep)
                .style(|_, _| button::Style::default()),
        );
    }
    if let Run::Done(report) = run {
        for note in &report.notes {
            let color = if note.warning {
                theme::DOWN
            } else {
                theme::TEXT_MUTED
            };
            body = body.push(text(note.text.as_str()).size(11).color(color));
        }
    }
    body = body
        .push(separator())
        .push(wide_button(
            "Backtest: span, playback and settings",
            MenuMessage::OpenBacktest,
        ))
        .extend(live_section(&tile.live, quote, tile.base));
    body = match tile.ai_allowed {
        Some(false) => body
            .push(
                text("This strategy asks language models. Its prompts go to the model's provider and new calls can cost money; answers are recorded, so reruns are free.")
                    .size(11)
                    .color(theme::TEXT_MUTED),
            )
            .push(
                button(text("Allow it to ask AI models").size(12).width(Fill).center())
                    .width(Fill)
                    .padding([3, 0])
                    .on_press(MenuMessage::AllowAi(true))
                    .style(|_, status| chip(true, status)),
            ),
        Some(true) => body.push(
            button(text("Stop it asking AI models").size(11).color(theme::TEXT_MUTED))
                .padding([2, 0])
                .on_press(MenuMessage::AllowAi(false))
                .style(|_, _| button::Style::default()),
        ),
        None => body,
    };
    if let Some(fills) = &tile.fills {
        body = body.push(text(fills.clone()).size(11).color(theme::TEXT_MUTED));
    }
    container(body.push(separator()).push(remove_button(
        "Remove strategy",
        MenuMessage::RemoveStrategy,
    )))
    .padding([4, 6])
    .into()
}

/// Paper (or testnet) trading the strategy on live bars: how a session is going, with a stop
/// button, or the buttons that start one.
fn live_section<'a>(live: &LiveInfo<'a>, quote: &str, base: &str) -> Vec<Element<'a, MenuMessage>> {
    let line = |label: String, value: String, color: Color| -> Element<'a, MenuMessage> {
        row![
            text(label).size(12).color(theme::TEXT),
            space::horizontal(),
            text(value)
                .size(12)
                .color(color)
                .font(iced::Font::MONOSPACE),
        ]
        .into()
    };
    let mut rows = Vec::new();
    if let Some(session) = live.session {
        let pnl = session.book.pnl();
        let color = if pnl >= 0.0 { theme::UP } else { theme::DOWN };
        rows.push(line("Live".into(), session.status(), theme::TEXT_BRIGHT));
        rows.push(line("Profit".into(), format!("{pnl:+.2} {quote}"), color));
        rows.push(line(
            "Position".into(),
            format!(
                "{} {base} · {} fills",
                amount(session.book.position),
                session.book.fills
            ),
            theme::TEXT_BRIGHT,
        ));
        if let LiveState::Stopped(Some(error)) = &session.state {
            rows.push(text(error.clone()).size(11).color(theme::DOWN).into());
        } else if let Some(problem) = &session.problem {
            rows.push(text(problem.clone()).size(11).color(theme::DOWN).into());
        }
        if session.is_active() {
            rows.push(wide_button(
                "Stop and close the position",
                MenuMessage::StopLive,
            ));
            return rows;
        }
    }
    match live.unavailable {
        Some(reason) => rows.push(text(reason).size(11).color(theme::TEXT_MUTED).into()),
        None => {
            rows.push(wide_button(
                "Paper trade on live bars",
                MenuMessage::GoLive(LiveMode::Paper),
            ));
            if live.testnet {
                rows.push(
                    button(
                        text("Trade on Binance's testnet")
                            .size(11)
                            .color(theme::TEXT_MUTED),
                    )
                    .padding([2, 0])
                    .on_press(MenuMessage::GoLive(LiveMode::Testnet))
                    .style(|_, _| button::Style::default())
                    .into(),
                );
            }
        }
    }
    rows
}

/// A signed quantity without float noise: `+0.12525`, `-3`, `0`.
fn amount(value: f64) -> String {
    let rounded = format!("{value:+.8}");
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    match trimmed {
        "+" | "-" | "+0" | "-0" => "0".into(),
        other => other.to_owned(),
    }
}

fn wide_button(label: &str, on_press: MenuMessage) -> Element<'_, MenuMessage> {
    button(text(label).size(12).width(Fill).center())
        .width(Fill)
        .padding([3, 0])
        .on_press(on_press)
        .style(|_, status| chip(true, status))
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
        text(title).size(12).color(theme::TEXT_BRIGHT),
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
                    .padding([3, 6])
                    .size(12)
                    .style(|_, status| input_style(status)),
            ]
            .align_y(iced::Center)
            .spacing(5)
            .into()
        })
        .collect()
}

fn panel<'a>(content: Element<'a, MenuMessage>, width: f32) -> Element<'a, MenuMessage> {
    container(content)
        .padding(4)
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
            text(label).size(12),
            space::horizontal(),
            text(hint).size(11).color(theme::TEXT_MUTED)
        ]
        .align_y(iced::Center),
    )
    .width(Fill)
    .padding([3, 8])
    .on_press(on_press)
    .style(|_, status| item_style(status))
    .into()
}

fn back(title: &str) -> Element<'_, MenuMessage> {
    button(
        text(format!("‹  {title}"))
            .size(12)
            .color(theme::TEXT_BRIGHT),
    )
    .width(Fill)
    .padding([3, 8])
    .on_press(MenuMessage::Open(Page::Root))
    .style(|_, status| item_style(status))
    .into()
}

fn section(title: &str) -> Element<'_, MenuMessage> {
    container(text(title.to_uppercase()).size(10).color(theme::TEXT_MUTED))
        .padding(iced::Padding {
            top: 4.0,
            right: 8.0,
            bottom: 1.0,
            left: 8.0,
        })
        .into()
}

fn note(content: &str) -> Element<'_, MenuMessage> {
    container(text(content).size(11).color(theme::TEXT_MUTED))
        .padding([2, 8])
        .into()
}

fn note_owned<'a>(content: String) -> Element<'a, MenuMessage> {
    container(text(content).size(11).color(theme::TEXT_MUTED))
        .padding([2, 8])
        .into()
}

fn separator<'a>() -> Element<'a, MenuMessage> {
    container(rule::horizontal(1).style(|_| rule::Style {
        color: PANEL_BORDER,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    }))
    .padding([2, 0])
    .into()
}

fn remove_button(label: &str, on_press: MenuMessage) -> Element<'_, MenuMessage> {
    button(text(label).size(12).center())
        .width(Fill)
        .padding([3, 0])
        .on_press(on_press)
        .style(|_, status| remove_style(status))
        .into()
}

/// A button that deletes something: red under the mouse.
fn remove_style(status: button::Status) -> button::Style {
    button::Style {
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
        text_color: if matches!(status, button::Status::Disabled) {
            theme::TEXT_MUTED
        } else {
            theme::TEXT_BRIGHT
        },
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
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

#[cfg(test)]
mod tests {
    use super::amount;

    #[test]
    fn amounts_drop_float_noise() {
        assert_eq!(amount(0.125_249_999_999_999_94), "+0.12525");
        assert_eq!(amount(-3.0), "-3");
        assert_eq!(amount(1e-12), "0");
        assert_eq!(amount(-0.0), "0");
    }
}
