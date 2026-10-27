//! Persephone: a tiled, chart-only backtesting app.
//!
//! Usage: `persephone [SYMBOL] [TIMEFRAME]`. With no arguments the last layout is restored
//! (one Binance BTCUSDT 1m chart on first launch). With arguments you get a single chart for
//! that market, and the session's layout is not saved. `SYMBOL.VENUE` (e.g. `EURUSD.LOCAL`)
//! opens a market from the local store, filled by `persephone-cli import`.
//!
//! The window is only charts, tiled like Hyprland's dwindle layout; see [`keymap`] for keys.
//! Right-click a chart for its [`menu`]: what's active on it (with checkboxes), indicators, a
//! strategy to backtest, symbol and timeframe. In a chart's header, clicking the symbol turns
//! it into a search box and clicking the timeframe drops down the others. Custom indicators and strategies are WebAssembly
//! [`plugins`] dropped into the plugins folder; they load, and reload when the files change, without a restart. CSV tick
//! files dropped on a chart import into the local store and open there. A strategy's report
//! can start paper trading it on live Binance bars.

mod importing;
pub mod keymap;
pub mod live;
mod local;
pub mod menu;
mod motion;
pub mod persist;
pub mod plugins;
mod studies;
mod sweep;

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use iced::{
    Background, Border, Color, Element, Fill, Length, Padding, Point, Size, Subscription, Task,
    Theme, event,
    futures::channel::oneshot,
    keyboard::{
        self,
        key::{Code, Physical},
    },
    widget::{button, container, mouse_area, operation, pin, row, stack, text, tooltip},
    window,
};
use persephone_chart::{
    ChartData, ChartMessage, ChartState, HeaderTag, PRICE_AXIS_WIDTH, TIME_AXIS_HEIGHT, theme,
};
use persephone_core::{
    BarSpec, Candle, InstrumentSpec, Timeframe,
    indicator::{IndicatorKind, IndicatorSpec},
    signal::SignalFactory,
    strategy::{StrategyKind, StrategySpec},
    tick::{TickFeed, TickKind},
};
use persephone_data::{
    binance::BinanceClient,
    import::{Detection, Format, MarketOptions, Progress, Summary, Zone, split_symbol},
    store::{MarketInfo, Store, check_name},
};
use persephone_engine::{
    BacktestReport, BacktestRequest, FeeRates,
    live::{LiveEvent, LiveFeed, LiveMode, LiveRequest, start_live},
};
use persephone_plugin_host::{Host, Plugins};
use persephone_tiling::{Layout, Rect, TileId};

use crate::{
    keymap::Action,
    live::{Live, LiveState},
    menu::{
        Active, ImportState, LiveInfo, MenuMessage, Page, Popup, SweepResult, SweepRow, TileInfo,
    },
    motion::Motion,
    persist::{Saved, SavedIndicator, SavedStrategy, SavedTile, SavedWorkspace},
};

/// How many bars each market loads.
const HISTORY_BARS: u64 = 5_000;
/// Every backtest starts with this much of the quote currency.
pub const STARTING_BALANCE: f64 = 100_000.0;
/// Space around and between tiles when there is more than one.
const GAP: f32 = 6.0;
const BORDER: f32 = 2.0;
/// How far one Mod+Ctrl press moves a divider, as a share of its split.
const RESIZE_STEP: f32 = 0.05;
/// How long the border flashes when an action is refused (17th chart, last chart).
const FLASH: Duration = Duration::from_millis(450);
/// How long to wait after a plugin file changes before reloading, so a build that writes the
/// file in several steps reloads once.
const PLUGIN_SETTLE: Duration = Duration::from_millis(300);
/// How long the workspace number shows after switching.
const WORKSPACE_BADGE: Duration = Duration::from_millis(900);
const DEFAULT_SYMBOL: &str = "BTCUSDT";
const DEFAULT_WINDOW: Size = Size::new(1280.0, 800.0);

/// Opens the app window and runs until it closes.
pub fn run() -> iced::Result {
    iced::application(Persephone::boot, Persephone::update, Persephone::view)
        .title(Persephone::title)
        .subscription(Persephone::subscription)
        .theme(Theme::Dark)
        .window_size(DEFAULT_WINDOW)
        .run()
}

/// What a tile shows. Tiles showing the same market share its bars.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Market {
    /// The venue of a market in the local store; `None` for Binance, fetched live.
    local: Option<String>,
    symbol: String,
    /// How its bars are cut: by time on any market, by ticks or volume on local ones.
    bars: BarSpec,
}

/// One market's bars plus what the engine needs to trade it.
#[derive(Clone, Debug)]
pub struct MarketData {
    pub data: Arc<ChartData>,
    pub instrument: InstrumentSpec,
    pub fees: FeeRates,
    /// Ticks to fill backtest orders on; `None` fills them on the bars.
    pub ticks: Option<Arc<dyn TickFeed>>,
    /// How backtest orders fill, for the report; `None` for a plain bar market.
    pub fills: Option<String>,
}

enum Bars {
    Loading,
    Ready(MarketData),
    Failed(String),
}

/// The latest backtest of a tile's strategy.
#[derive(Clone, Debug)]
pub enum Run {
    /// The tile's bars haven't arrived yet.
    Waiting,
    Running,
    Done(Arc<BacktestReport>),
    Failed(String),
}

struct Strategy {
    spec: StrategySpec,
    run: Run,
    /// Bumped on every new run so results from older runs are dropped.
    generation: u64,
}

struct Tile {
    market: Market,
    /// The tile's indicators, in legend order. They survive symbol and timeframe changes.
    indicators: Vec<IndicatorSpec>,
    /// Backtested on the tile's bars; reruns when the bars or settings change.
    strategy: Option<Strategy>,
    /// `None` until the market's bars arrive.
    chart: Option<ChartState>,
    motion: Motion,
    /// When a refused action started flashing this tile's border.
    flash: Option<Instant>,
    /// The sweep it's part of, if any.
    sweep: Option<u64>,
    /// Its strategy trading live bars, or how that went.
    live: Option<Live>,
    /// What its Active list has switched off.
    hidden: Hidden,
}

/// What a chart's Active list has switched off. Hidden things stay on the chart (and keep
/// computing, so turning them back on is instant); they just aren't drawn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hidden {
    /// Indices into the chart's indicators.
    pub indicators: BTreeSet<usize>,
    /// Its trades, equity pane and summary.
    pub strategy: bool,
    pub drawings: bool,
    /// The volume bars.
    pub volume: bool,
}

/// The charts of a workspace that isn't on screen.
struct Workspace {
    layout: Layout,
    tiles: BTreeMap<TileId, Tile>,
}

impl Workspace {
    /// A workspace about to get its first chart, which takes the layout's one tile id.
    fn empty(number: u8) -> Self {
        Self {
            layout: Layout::starting_at(first_tile(number)),
            tiles: BTreeMap::new(),
        }
    }
}

/// Each workspace numbers its charts from its own start, so no two charts anywhere share an id
/// (workspace 1 starts at 0, as layouts saved before workspaces did).
fn first_tile(workspace: u8) -> TileId {
    TileId::from(workspace.saturating_sub(1)) << 20
}

pub struct Persephone {
    /// The workspace on screen: its layout and charts.
    layout: Layout,
    tiles: BTreeMap<TileId, Tile>,
    /// Its number, 1 to 9.
    workspace: u8,
    /// The other workspaces that have charts.
    stash: BTreeMap<u8, Workspace>,
    /// When the workspace last changed, for the badge naming it.
    workspace_shown: Option<Instant>,
    bars: HashMap<Market, Bars>,
    window: Size,
    now: Instant,
    /// The open menu, settings popover or report, if any.
    popup: Option<Popup>,
    /// Where the layout is saved; `None` for one-off sessions started with arguments.
    save_path: Option<PathBuf>,
    /// Compiles and runs plugins; `None` when plugins aren't available.
    host: Option<Arc<Host>>,
    /// Where plugin files live, watched for changes.
    plugin_dir: Option<PathBuf>,
    /// What's loaded from `plugin_dir`.
    plugins: Arc<Plugins>,
    /// A reload is already on its way.
    reloading: bool,
    /// The local tick store; `None` when there's no data folder (or in tests that don't set one).
    store: Option<Store>,
    /// The store's markets, as of the last look (refreshed when the symbol page opens).
    local_markets: Vec<MarketInfo>,
    /// Files are being dragged over the window.
    drop_hint: bool,
    /// Plugins allowed to call language models, and where that list is saved.
    ai_grants: BTreeSet<String>,
    grants_path: Option<PathBuf>,
    /// Drawings by market, and where they're saved (next to the layout).
    drawings: persist::Drawings,
    drawings_path: Option<PathBuf>,
    /// Parameter sweeps by id, and the next id.
    sweeps: BTreeMap<u64, sweep::Sweep>,
    next_sweep: u64,
    /// Numbers live sessions.
    next_live: u64,
    /// Live sessions replay these bars at this pace instead of trading Binance's (tests).
    live_replay: Option<(Vec<Candle>, Duration)>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(Market, Result<MarketData, String>),
    Chart(TileId, ChartMessage),
    Key(Action),
    /// The mouse entered a tile: focus follows it, as on Hyprland.
    Hover(TileId),
    WindowResized(Size),
    Tick(Instant),
    Menu(MenuMessage),
    /// Closes the open popup.
    Escape,
    /// A backtest finished: tile, run generation, outcome.
    BacktestDone(TileId, u64, Result<Arc<BacktestReport>, String>),
    /// The strategy summary on a tile was clicked.
    OpenReport(TileId),
    /// A plugin file was added, changed or removed.
    PluginsChanged,
    PluginsLoaded(Arc<Plugins>),
    /// Files started (`true`) or stopped being dragged over the window.
    FilesHovered(bool),
    /// A file was dropped on the window; it imports into the focused chart.
    FileDropped(PathBuf),
    /// The layout of a dropped file, worked out.
    Detected(PathBuf, Result<Arc<Detection>, String>),
    ImportProgress(Progress),
    Imported(Result<Arc<Summary>, String>),
    /// The latest bars for a live session about to start: tile, session, bars.
    LiveHistory(TileId, u64, Result<MarketData, String>),
    /// What a live session did: tile, session, event.
    Live(TileId, u64, LiveEvent),
    /// One of a chart's corner buttons: the same as its shortcut, on that chart.
    TileButton(TileId, Action),
}

impl Persephone {
    fn boot() -> (Self, Task<Message>) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let save_path = args.is_empty().then(persist::default_path).flatten();
        let (mut app, loads) = Self::start(&args, save_path);

        match Host::with_services(plugins::services()) {
            Ok(host) => {
                app.grants_path = plugins::grants_path();
                app.ai_grants = app
                    .grants_path
                    .as_deref()
                    .map(plugins::load_grants)
                    .unwrap_or_default();
                for id in &app.ai_grants {
                    host.services().allow_ai(id, true);
                }
                app.host = Some(Arc::new(host));
                app.plugin_dir =
                    plugins::default_dir().filter(|dir| std::fs::create_dir_all(dir).is_ok());
            }
            Err(error) => eprintln!("persephone: plugins are off: {error}"),
        }
        let plugins = app.reload_plugins(Duration::ZERO);
        (app, Task::batch([loads, plugins]))
    }

    /// Starts from the layout saved at `save_path` (if any, and valid), else from one chart
    /// for the market in `args`. Returns the tasks that load each market's bars.
    #[doc(hidden)]
    pub fn start(args: &[String], save_path: Option<PathBuf>) -> (Self, Task<Message>) {
        let saved = save_path.as_deref().and_then(persist::load);

        let tiles: Vec<(TileId, Restored)> = match &saved {
            Some(saved) => saved
                .tiles
                .iter()
                .map(|t| (t.id, restore_tile(t)))
                .collect(),
            None => vec![(
                0,
                (market_from_args(args), Vec::new(), None, Hidden::default()),
            )],
        };

        let drawings_path = save_path.as_ref().map(|p| p.with_file_name("drawings.ron"));
        let workspace = saved.as_ref().map_or(1, |s| s.workspace);
        let others = saved.as_ref().map_or_else(Vec::new, |s| s.others.clone());
        let mut app = Self {
            layout: saved.map_or_else(Layout::new, |s| s.layout),
            tiles: BTreeMap::new(),
            workspace,
            stash: BTreeMap::new(),
            workspace_shown: None,
            bars: HashMap::new(),
            window: DEFAULT_WINDOW,
            now: Instant::now(),
            popup: None,
            save_path,
            host: None,
            plugin_dir: None,
            plugins: Arc::default(),
            reloading: false,
            store: Store::default_root().map(Store::new),
            local_markets: Vec::new(),
            drop_hint: false,
            ai_grants: BTreeSet::new(),
            grants_path: None,
            drawings: drawings_path
                .as_deref()
                .map(persist::load_drawings)
                .unwrap_or_default(),
            drawings_path,
            sweeps: BTreeMap::new(),
            next_sweep: 0,
            next_live: 0,
            live_replay: None,
        };
        app.refresh_local_markets();
        let mut loads: Vec<_> = tiles
            .into_iter()
            .map(|(id, restored)| app.restore(id, restored))
            .collect();
        // The other workspaces load the same way, each swapped in while its tiles are added.
        for other in others {
            let mut workspace = Workspace {
                layout: other.layout,
                tiles: BTreeMap::new(),
            };
            app.swap_workspace(&mut workspace);
            for tile in &other.tiles {
                loads.push(app.restore(tile.id, restore_tile(tile)));
            }
            app.swap_workspace(&mut workspace);
            app.stash.insert(other.number, workspace);
        }
        app.relayout(false);
        (app, Task::batch(loads))
    }

    /// A one-chart app over bars already in memory: no network, nothing saved. For tests and
    /// offscreen snapshots. Assumes a `…USDT` pair with 5-decimal lots.
    #[doc(hidden)]
    pub fn preloaded(data: Arc<ChartData>, window: Size) -> Self {
        let market = Market {
            local: None,
            symbol: data.symbol.clone(),
            bars: data.bars,
        };
        let instrument = InstrumentSpec {
            symbol: data.symbol.clone(),
            venue: data.venue.clone(),
            base: data.symbol.trim_end_matches("USDT").to_owned(),
            quote: "USDT".into(),
            price_precision: data.price_precision,
            size_precision: 5,
        };
        let mut app = Self {
            layout: Layout::new(),
            tiles: BTreeMap::new(),
            workspace: 1,
            stash: BTreeMap::new(),
            workspace_shown: None,
            bars: HashMap::from([(
                market.clone(),
                Bars::Ready(MarketData {
                    data,
                    instrument,
                    fees: FeeRates::BINANCE_SPOT,
                    ticks: None,
                    fills: None,
                }),
            )]),
            window,
            now: Instant::now(),
            popup: None,
            save_path: None,
            host: None,
            plugin_dir: None,
            plugins: Arc::default(),
            reloading: false,
            store: None,
            local_markets: Vec::new(),
            drop_hint: false,
            ai_grants: BTreeSet::new(),
            grants_path: None,
            drawings: persist::Drawings::new(),
            drawings_path: None,
            sweeps: BTreeMap::new(),
            next_sweep: 0,
            next_live: 0,
            live_replay: None,
        };
        let _ = app.add_tile(0, market, Vec::new(), None);
        app.relayout(false);
        app
    }

    #[must_use]
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The symbol and timeframe tile `id` shows.
    #[must_use]
    pub fn tile_market(&self, id: TileId) -> Option<(&str, BarSpec)> {
        self.tiles
            .get(&id)
            .map(|tile| (tile.market.symbol.as_str(), tile.market.bars))
    }

    /// What kind of ticks a local market holds (for which bars it can have); `None` for Binance.
    fn local_kind(&self, market: &Market) -> Option<TickKind> {
        let venue = market.local.as_deref()?;
        self.local_markets
            .iter()
            .find(|m| m.venue == venue && m.symbol == market.symbol)
            .map(|m| m.kind)
    }

    /// The local store's venue for tile `id`'s market, or `None` for a Binance market.
    #[must_use]
    pub fn tile_venue(&self, id: TileId) -> Option<&str> {
        self.tiles.get(&id)?.market.local.as_deref()
    }

    /// Uses `store` for local markets. For tests, which keep away from the real one.
    #[doc(hidden)]
    pub fn set_store(&mut self, store: Store) {
        self.store = Some(store);
        self.refresh_local_markets();
    }

    /// Loads every market still waiting, here and now instead of in a task. For tests.
    #[doc(hidden)]
    pub fn load_now(&mut self) {
        let waiting: Vec<Market> = self
            .bars
            .iter()
            .filter(|(_, bars)| matches!(bars, Bars::Loading))
            .map(|(market, _)| market.clone())
            .collect();
        for market in waiting {
            let result = fetch(&market, self.store.as_ref());
            let _ = self.update(Message::Loaded(market, result));
        }
    }

    fn refresh_local_markets(&mut self) {
        self.local_markets = self
            .store
            .as_ref()
            .map(|store| store.markets().into_iter().map(|m| m.info).collect())
            .unwrap_or_default();
    }

    /// The indicators on tile `id`, in legend order.
    #[must_use]
    pub fn tile_indicators(&self, id: TileId) -> Option<&[IndicatorSpec]> {
        self.tiles.get(&id).map(|tile| tile.indicators.as_slice())
    }

    /// Tile `id`'s strategy and the state of its latest backtest.
    #[must_use]
    pub fn tile_strategy(&self, id: TileId) -> Option<(&StrategySpec, &Run)> {
        self.tiles
            .get(&id)
            .and_then(|tile| tile.strategy.as_ref())
            .map(|s| (&s.spec, &s.run))
    }

    /// Tile `id`'s chart, once its bars are in.
    #[must_use]
    pub fn tile_hidden(&self, id: TileId) -> Option<&Hidden> {
        self.tiles.get(&id).map(|t| &t.hidden)
    }

    #[must_use]
    pub fn tile_live(&self, id: TileId) -> Option<&Live> {
        self.tiles.get(&id)?.live.as_ref()
    }

    #[must_use]
    pub fn tile_chart(&self, id: TileId) -> Option<&ChartState> {
        self.tiles.get(&id).and_then(|tile| tile.chart.as_ref())
    }

    /// Why tile `id`'s bars didn't load, if they didn't.
    #[must_use]
    pub fn tile_error(&self, id: TileId) -> Option<&str> {
        match self.bars.get(&self.tiles.get(&id)?.market)? {
            Bars::Failed(error) => Some(error),
            _ => None,
        }
    }

    #[must_use]
    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
    }

    /// The plugins currently loaded.
    #[must_use]
    pub fn plugins(&self) -> &Plugins {
        &self.plugins
    }

    /// Where tile `id` is drawn right now.
    #[must_use]
    pub fn tile_rect(&self, id: TileId) -> Option<Rect> {
        self.tiles.get(&id).map(|tile| tile.motion.rect(self.now))
    }

    #[must_use]
    pub fn is_animating(&self) -> bool {
        self.workspace_shown
            .is_some_and(|start| self.now.saturating_duration_since(start) < WORKSPACE_BADGE)
            || self.tiles.values().any(|tile| {
                tile.motion.is_moving(self.now)
                    || tile.flash.is_some_and(|start| self.now - start < FLASH)
            })
    }

    /// The backtest tile `id` would run now (its strategy on its bars) and the generation its
    /// result must carry. For tests and snapshots, which run it themselves.
    #[doc(hidden)]
    #[must_use]
    pub fn backtest_request(&self, id: TileId) -> Option<(u64, BacktestRequest)> {
        let tile = self.tiles.get(&id)?;
        let strategy = tile.strategy.as_ref()?;
        let Some(Bars::Ready(market)) = self.bars.get(&tile.market) else {
            return None;
        };
        let plugin = match &strategy.spec.kind {
            StrategyKind::Plugin(id) => self
                .plugins
                .strategies
                .get(id)
                .map(|p| Arc::clone(p) as Arc<dyn SignalFactory>),
            StrategyKind::BuiltIn(_) => None,
        };
        let request = BacktestRequest {
            instrument: market.instrument.clone(),
            bars: tile.market.bars,
            candles: market.data.candles.clone(),
            strategy: strategy.spec.clone(),
            plugin,
            ticks: market.ticks.clone(),
            starting_balance: STARTING_BALANCE,
            fees: market.fees,
        };
        Some((strategy.generation, request))
    }

    #[must_use]
    pub fn title(&self) -> String {
        let workspace = if self.stash.is_empty() && self.workspace == 1 {
            String::new()
        } else {
            format!(" · workspace {}", self.workspace)
        };
        match self.tiles.get(&self.layout.focused()) {
            Some(tile) => format!(
                "{} · {} — Persephone{workspace}",
                tile.market.symbol, tile.market.bars
            ),
            None => format!("Persephone{workspace}"),
        }
    }

    /// The workspace on screen, 1 to 9.
    #[must_use]
    pub fn workspace(&self) -> u8 {
        self.workspace
    }

    /// The workspaces that have charts, in order.
    #[must_use]
    pub fn workspaces(&self) -> Vec<u8> {
        let mut numbers: Vec<u8> = self.stash.keys().copied().collect();
        numbers.push(self.workspace);
        numbers.sort_unstable();
        numbers
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        self.now = Instant::now();
        match message {
            Message::Loaded(market, result) => {
                let bars = match result {
                    Ok(data) => Bars::Ready(data),
                    Err(error) => Bars::Failed(error),
                };
                self.bars.insert(market.clone(), bars);
                let waiting: Vec<TileId> = self
                    .tiles
                    .iter()
                    .filter(|(_, t)| t.market == market && t.chart.is_none())
                    .map(|(&id, _)| id)
                    .collect();
                return Task::batch(waiting.into_iter().map(|id| self.attach(id)));
            }
            Message::Chart(id, ChartMessage::RightClicked { at }) => {
                self.popup = Some(Popup::Menu {
                    tile: id,
                    at,
                    page: Page::Root,
                });
            }
            Message::Chart(_, ChartMessage::Clicked { .. }) => self.popup = None,
            Message::Chart(id, ChartMessage::TagClicked { tag }) => {
                // A tag shows or hides volume (the first) or an indicator, like its box in the
                // Active list.
                if let Some(tile) = self.tiles.get_mut(&id) {
                    match tag.checked_sub(1) {
                        None => tile.hidden.volume = !tile.hidden.volume,
                        Some(i) if i < tile.indicators.len() => {
                            if !tile.hidden.indicators.remove(&i) {
                                tile.hidden.indicators.insert(i);
                            }
                        }
                        Some(_) => {}
                    }
                }
                self.restudy(id);
                self.save();
            }
            Message::Chart(id, ChartMessage::SymbolClicked { .. }) => {
                self.popup = Some(Popup::SymbolSearch {
                    tile: id,
                    query: String::new(),
                });
                self.refresh_local_markets();
                return operation::focus(menu::symbol_input());
            }
            Message::Chart(id, ChartMessage::TimeframeClicked { at }) => {
                self.popup = Some(Popup::Timeframes {
                    tile: id,
                    at: below_header(at),
                });
            }
            Message::Chart(id, ChartMessage::StudyClicked { study, at }) => {
                let Some(tile) = self.tiles.get(&id) else {
                    return Task::none();
                };
                // The chart shows the indicators that aren't hidden, then the strategy's equity.
                let shown = (0..tile.indicators.len())
                    .filter(|i| !tile.hidden.indicators.contains(i))
                    .nth(study);
                self.popup = match shown {
                    Some(index) => Some(Popup::settings(id, index, at, &tile.indicators[index])),
                    None => self.report_popup(id),
                };
            }
            Message::Chart(id, ChartMessage::Hovered { ts }) => {
                // Every other chart shows a crosshair at the same time.
                for (&other, tile) in &mut self.tiles {
                    if let Some(chart) = &mut tile.chart {
                        chart.set_linked_time(if other == id { None } else { ts });
                    }
                }
            }
            Message::Menu(message) => return self.on_menu(message),
            Message::Escape => {
                self.popup = None;
                for chart in self.tiles.values_mut().filter_map(|t| t.chart.as_mut()) {
                    chart.cancel_drawing();
                }
            }
            Message::Chart(id, ChartMessage::Placed(anchor)) => {
                let finished = self
                    .tiles
                    .get_mut(&id)
                    .and_then(|t| t.chart.as_mut())
                    .and_then(|chart| chart.place(anchor));
                if let (Some(drawing), Some(market)) =
                    (finished, self.tiles.get(&id).map(|t| t.market.clone()))
                {
                    self.drawings
                        .entry(drawing_key(&market))
                        .or_default()
                        .push(drawing);
                    self.show_drawings(&market);
                }
            }
            Message::Chart(id, ChartMessage::DrawingClicked { index, at }) => {
                self.popup = match self.popup.take() {
                    Some(_) => None,
                    None => Some(Popup::Drawing {
                        tile: id,
                        index,
                        at,
                    }),
                };
            }
            Message::Chart(id, message) => {
                if let Some(chart) = self.tiles.get_mut(&id).and_then(|t| t.chart.as_mut()) {
                    chart.update(message);
                }
            }
            Message::OpenReport(id) => {
                let open = matches!(&self.popup, Some(Popup::Report { tile, .. }) if *tile == id);
                self.popup = if open { None } else { self.report_popup(id) };
            }
            Message::BacktestDone(id, generation, result) => {
                // The chart may be on another workspace by now; it's redrawn when it's back.
                let on_screen = self.tiles.contains_key(&id);
                let tile = if on_screen {
                    self.tiles.get_mut(&id)
                } else {
                    self.stash.values_mut().find_map(|w| w.tiles.get_mut(&id))
                };
                if let Some(strategy) = tile
                    .and_then(|t| t.strategy.as_mut())
                    .filter(|s| s.generation == generation)
                {
                    strategy.run = match result {
                        Ok(report) => Run::Done(report),
                        Err(error) => Run::Failed(error),
                    };
                    if on_screen {
                        self.restudy(id);
                    }
                }
            }
            Message::Key(action) => return self.act(action),
            Message::TileButton(id, action) => {
                self.layout.focus(id);
                return self.act(action);
            }
            Message::Hover(id) => {
                self.layout.focus(id);
            }
            Message::WindowResized(size) => {
                self.window = size;
                self.relayout(false);
            }
            Message::Tick(now) => self.now = now,
            Message::PluginsChanged => {
                if !self.reloading {
                    return self.reload_plugins(PLUGIN_SETTLE);
                }
            }
            Message::FilesHovered(hovering) => self.drop_hint = hovering,
            Message::FileDropped(path) => {
                self.drop_hint = false;
                return self.on_drop(path);
            }
            Message::Detected(path, result) => {
                if let Some(Popup::Import { files, state, .. }) = &mut self.popup
                    && files.first() == Some(&path)
                    && matches!(state, ImportState::Detecting)
                {
                    *state = match result {
                        Ok(detection) => ImportState::Ready(detection),
                        Err(error) => ImportState::Failed(error),
                    };
                }
            }
            Message::ImportProgress(progress) => {
                if let Some(Popup::Import { state, .. }) = &mut self.popup
                    && matches!(state, ImportState::Importing(_))
                {
                    *state = ImportState::Importing(progress);
                }
            }
            Message::Imported(result) => {
                self.refresh_local_markets();
                let tile = match &self.popup {
                    Some(Popup::Import { tile, .. }) => Some(*tile),
                    _ => None,
                };
                let shown = match &result {
                    Ok(summary) => self.show_imported(tile, &summary.market.info),
                    Err(_) => Task::none(),
                };
                if let Some(Popup::Import { state, .. }) = &mut self.popup {
                    *state = match result {
                        Ok(summary) => ImportState::Done(summary),
                        Err(error) => ImportState::Failed(error),
                    };
                }
                return shown;
            }
            Message::PluginsLoaded(plugins) => {
                self.reloading = false;
                return self.use_plugins(plugins);
            }
            Message::LiveHistory(id, session, result) => {
                let Some(tile) = self.tiles.get_mut(&id) else {
                    return Task::none();
                };
                let Some(live) = tile
                    .live
                    .as_mut()
                    .filter(|l| l.id == session && l.state == LiveState::Preparing)
                else {
                    return Task::none();
                };
                match result {
                    Ok(data) => {
                        let market = tile.market.clone();
                        self.bars.insert(market.clone(), Bars::Ready(data));
                        let refreshed = self.refresh_market(&market);
                        return Task::batch([refreshed, self.start_session(id)]);
                    }
                    Err(error) => live.state = LiveState::Stopped(Some(error)),
                }
            }
            Message::Live(id, session, event) => {
                let on_screen = self.tiles.contains_key(&id);
                let tile = if on_screen {
                    self.tiles.get_mut(&id)
                } else {
                    self.stash.values_mut().find_map(|w| w.tiles.get_mut(&id))
                };
                let Some(tile) = tile else {
                    return Task::none();
                };
                let Some(live) = tile.live.as_mut().filter(|l| l.id == session) else {
                    return Task::none();
                };
                let filled = matches!(event, LiveEvent::Fill(_));
                if let Some(bar) = live.apply(event) {
                    let market = tile.market.clone();
                    return self.add_live_bar(&market, bar);
                }
                if filled && on_screen {
                    self.restudy(id);
                }
            }
        }
        Task::none()
    }

    /// Starts trading tile `id`'s strategy live: first the latest bars, so it starts warm.
    fn go_live(&mut self, id: TileId, mode: LiveMode) -> Task<Message> {
        if self.live_unavailable(id).is_some() {
            return Task::none();
        }
        self.next_live += 1;
        let session = self.next_live;
        let Some(tile) = self.tiles.get_mut(&id) else {
            return Task::none();
        };
        tile.live = Some(Live::new(mode, session));
        if self.live_replay.is_some() {
            return self.start_session(id);
        }
        let market = tile.market.clone();
        Task::perform(on_thread(move || fetch(&market, None)), move |result| {
            Message::LiveHistory(id, session, result)
        })
    }

    /// Why tile `id` can't trade live, if it can't.
    fn live_unavailable(&self, id: TileId) -> Option<&'static str> {
        let tile = self.tiles.get(&id)?;
        if tile.strategy.is_none() {
            return Some("Add a strategy to trade it live.");
        }
        if tile.market.local.is_some() {
            return Some("Live trading needs a Binance market; this one is from your files.");
        }
        if tile.market.bars.timeframe().is_none() {
            return Some("Live trading uses time bars.");
        }
        None
    }

    /// The session tile `id` is about to start: its strategy on its market, sized like its
    /// backtests, warmed up on the bars the chart shows.
    #[doc(hidden)]
    pub fn live_request(&self, id: TileId) -> Option<LiveRequest> {
        let tile = self.tiles.get(&id)?;
        let live = tile.live.as_ref()?;
        let timeframe = tile.market.bars.timeframe()?;
        let (_, backtest) = self.backtest_request(id)?;
        let feed = match &self.live_replay {
            Some((bars, every)) => LiveFeed::Replay {
                bars: bars.clone(),
                every: *every,
            },
            None => LiveFeed::Binance,
        };
        Some(LiveRequest {
            mode: live.mode,
            feed,
            instrument: backtest.instrument,
            timeframe,
            history: backtest.candles,
            strategy: backtest.strategy,
            plugin: backtest.plugin,
            balance: STARTING_BALANCE,
            fees: backtest.fees,
            owns_signals: false,
        })
    }

    /// Live sessions replay `bars`, one every `every`, instead of trading Binance's. For tests
    /// and offline demos.
    #[doc(hidden)]
    pub fn rehearse_live(&mut self, bars: Vec<Candle>, every: Duration) {
        self.live_replay = Some((bars, every));
    }

    fn start_session(&mut self, id: TileId) -> Task<Message> {
        let request = self.live_request(id);
        let Some(live) = self.tiles.get_mut(&id).and_then(|t| t.live.as_mut()) else {
            return Task::none();
        };
        let Some(request) = request else {
            live.state = LiveState::Stopped(Some("the chart's bars aren't loaded".into()));
            return Task::none();
        };
        let session = live.id;
        let (events, received) = iced::futures::channel::mpsc::unbounded();
        let started = start_live(request, move |event| {
            let _ = events.unbounded_send(Message::Live(id, session, event));
        });
        match started {
            Ok(running) => {
                live.session = Some(running);
                live.state = LiveState::Connecting;
                Task::stream(received)
            }
            Err(error) => {
                live.state = LiveState::Stopped(Some(error.to_string()));
                Task::none()
            }
        }
    }

    /// A live bar closed: it joins its market's bars, and every chart of that market updates.
    fn add_live_bar(&mut self, market: &Market, bar: Candle) -> Task<Message> {
        let Some(Bars::Ready(loaded)) = self.bars.get_mut(market) else {
            return Task::none();
        };
        let mut candles = loaded.data.candles.clone();
        if !live::merge(&mut candles, bar) {
            return Task::none();
        }
        loaded.data = loaded.data.with_candles(candles);
        self.refresh_market(market)
    }

    /// Gives the on-screen charts of `market` its latest bars, keeping their views, and reruns
    /// their indicators and backtests. Charts on other workspaces pick them up when shown.
    fn refresh_market(&mut self, market: &Market) -> Task<Message> {
        let Some(Bars::Ready(loaded)) = self.bars.get(market) else {
            return Task::none();
        };
        let data = Arc::clone(&loaded.data);
        let ids: Vec<TileId> = self
            .tiles
            .iter()
            .filter(|(_, t)| &t.market == market)
            .map(|(&id, _)| id)
            .collect();
        let mut tasks = Vec::new();
        for id in ids {
            if let Some(chart) = self.tiles.get_mut(&id).and_then(|t| t.chart.as_mut()) {
                chart.set_data(Arc::clone(&data));
                self.restudy(id);
                tasks.push(self.run_backtest_keeping(id));
            }
        }
        Task::batch(tasks)
    }

    fn act(&mut self, action: Action) -> Task<Message> {
        self.popup = None;
        let area = self.tiling_area();
        let mut task = Task::none();
        let mut popped_in = None;

        let structural = match action {
            Action::NewTile => {
                let market = self.focused_market();
                match self.layout.insert(area) {
                    Ok(id) => {
                        task = self.add_tile(id, market, Vec::new(), None);
                        popped_in = Some(id);
                        true
                    }
                    Err(_) => self.refuse(),
                }
            }
            Action::CloseTile => {
                let id = self.layout.focused();
                match self.layout.close(id) {
                    Ok(()) => {
                        self.tiles.remove(&id);
                        true
                    }
                    Err(_) => self.refuse(),
                }
            }
            Action::Focus(direction) => {
                self.layout.focus_towards(direction, area);
                false
            }
            Action::Swap(direction) => self.layout.swap_towards(direction, area),
            Action::Resize(direction) => self.layout.resize(direction, RESIZE_STEP),
            Action::ToggleSplit => self.layout.toggle_split(),
            Action::Fullscreen => {
                self.layout.toggle_fullscreen();
                true
            }
            Action::Workspace(number) => return self.switch_workspace(number),
            Action::MoveToWorkspace(number) => return self.move_to_workspace(number),
        };

        self.relayout(true);
        if let Some(tile) = popped_in.and_then(|id| self.tiles.get_mut(&id)) {
            tile.motion = Motion::pop_in(tile.motion.target(), self.now);
        }
        if structural {
            self.save();
        }
        task
    }

    /// Lets tile `id`'s strategy plugin call language models (or stops it), remembers that, and
    /// reruns every chart using the plugin.
    fn allow_ai(&mut self, id: TileId, allow: bool) -> Task<Message> {
        let Some(StrategyKind::Plugin(plugin_id)) = self
            .tiles
            .get(&id)
            .and_then(|t| t.strategy.as_ref())
            .map(|s| s.spec.kind.clone())
        else {
            return Task::none();
        };
        if let Some(plugin) = self.plugins.strategies.get(&plugin_id) {
            plugin.allow_ai(allow);
        }
        if allow {
            self.ai_grants.insert(plugin_id.clone());
        } else {
            self.ai_grants.remove(&plugin_id);
        }
        if let Some(path) = &self.grants_path
            && let Err(e) = plugins::save_grants(path, &self.ai_grants)
        {
            eprintln!("persephone: couldn't save {}: {e}", path.display());
        }
        let using: Vec<TileId> = self
            .tiles
            .iter()
            .filter(|(_, t)| {
                t.strategy
                    .as_ref()
                    .is_some_and(|s| s.spec.kind == StrategyKind::Plugin(plugin_id.clone()))
            })
            .map(|(&id, _)| id)
            .collect();
        Task::batch(using.into_iter().map(|id| self.run_backtest(id)))
    }

    /// A dropped file: the first opens the import panel on the focused chart; more dropped
    /// with it (or before its import starts) join that import.
    fn on_drop(&mut self, path: PathBuf) -> Task<Message> {
        if let Some(Popup::Import { files, state, .. }) = &mut self.popup
            && matches!(state, ImportState::Detecting | ImportState::Ready(_))
        {
            if !files.contains(&path) {
                files.push(path);
            }
            return Task::none();
        }
        let tile = self.layout.focused();
        self.popup = Some(Popup::import(
            tile,
            path.clone(),
            importing::guess_symbol(&path),
        ));
        importing::detect(path)
    }

    fn start_import(&mut self, mut popup: Popup) -> Task<Message> {
        let mut task = Task::none();
        if let Popup::Import {
            files,
            state,
            symbol,
            venue,
            zone,
            error,
            ..
        } = &mut popup
            && let ImportState::Ready(detection) = state
        {
            let checked = check_name("symbol", symbol)
                .and_then(|()| check_name("venue", venue))
                .and_then(|()| zone.parse::<Zone>());
            match (checked, &self.store) {
                (Err(e), _) => *error = Some(e.to_string()),
                (Ok(_), None) => *error = Some("there's no data folder to keep ticks in".into()),
                (Ok(zone), Some(store)) => {
                    // Names that don't split into two currencies (an index, say) count in USD.
                    let (base, quote) = match split_symbol(symbol) {
                        Some(_) => (None, None),
                        None => (Some(symbol.clone()), Some("USD".to_owned())),
                    };
                    let options = MarketOptions {
                        symbol: symbol.clone(),
                        venue: venue.clone(),
                        base,
                        quote,
                        fee: None,
                    };
                    let format = Format {
                        zone,
                        ..detection.format.clone()
                    };
                    task = importing::run(store.clone(), files.clone(), format, options);
                    *state = ImportState::Importing(Progress::default());
                    *error = None;
                }
            }
        }
        self.popup = Some(popup);
        task
    }

    /// After an import: charts already showing the market reload it, and `tile` switches to it.
    fn show_imported(&mut self, tile: Option<TileId>, info: &MarketInfo) -> Task<Message> {
        let is_it =
            |m: &Market| m.local.as_deref() == Some(info.venue.as_str()) && m.symbol == info.symbol;
        self.bars.retain(|market, _| !is_it(market));
        let showing: Vec<TileId> = self
            .tiles
            .iter()
            .filter(|(_, t)| is_it(&t.market))
            .map(|(&id, _)| id)
            .collect();
        let mut tasks = Vec::new();
        for id in showing {
            let Some(t) = self.tiles.get_mut(&id) else {
                continue;
            };
            t.chart = None;
            if let Some(strategy) = &mut t.strategy {
                strategy.run = Run::Waiting;
            }
            let market = t.market.clone();
            tasks.push(self.request(&market));
            tasks.push(self.attach(id));
        }
        if let Some(id) = tile
            && let Some(t) = self.tiles.get(&id)
        {
            let market = Market {
                local: Some(info.venue.clone()),
                symbol: info.symbol.clone(),
                bars: t.market.bars,
            };
            tasks.push(self.set_market(id, market));
        }
        Task::batch(tasks)
    }

    /// Opens the sweep the popup describes, or says why not.
    fn start_sweep(&mut self, mut popup: Popup) -> Task<Message> {
        let Popup::Sweep {
            tile,
            param,
            inputs,
            error,
            ..
        } = &mut popup
        else {
            return Task::none();
        };
        let Some(source) = self.tiles.get(tile) else {
            return Task::none();
        };
        let Some(spec) = source.strategy.as_ref().map(|s| s.spec.clone()) else {
            return Task::none();
        };
        let parsed = (
            inputs[0].trim().parse::<f64>(),
            inputs[1].trim().parse::<f64>(),
            inputs[2].trim().parse::<usize>(),
        );
        let values = match (parsed, spec.info.params.get(*param)) {
            ((Ok(from), Ok(to), Ok(charts)), Some(p)) => sweep::values(p, from, to, charts),
            _ => Err("type numbers for from, to and charts".to_owned()),
        };
        let free = self.free_workspace();
        match (values, free) {
            (Err(message), _) => *error = Some(message),
            (Ok(_), None) => *error = Some("every workspace is in use; close one first".into()),
            (Ok(values), Some(number)) => {
                let (market, indicators) = (source.market.clone(), source.indicators.clone());
                return self.open_sweep(number, market, indicators, spec, *param, &values);
            }
        }
        self.popup = Some(popup);
        Task::none()
    }

    /// The lowest workspace with no charts.
    fn free_workspace(&self) -> Option<u8> {
        (1..=9).find(|n| *n != self.workspace && !self.stash.contains_key(n))
    }

    /// Workspace `number` with a chart for each value of parameter `param`.
    fn open_sweep(
        &mut self,
        number: u8,
        market: Market,
        indicators: Vec<IndicatorSpec>,
        spec: StrategySpec,
        param: usize,
        values: &[f64],
    ) -> Task<Message> {
        let id = self.next_sweep;
        self.next_sweep += 1;
        self.popup = None;
        let mut next = Workspace::empty(number);
        self.swap_workspace(&mut next);
        self.stash.insert(self.workspace, next);
        self.workspace = number;
        self.workspace_shown = Some(self.now);

        let area = self.tiling_area();
        let mut charts = Vec::new();
        let mut tasks = Vec::new();
        for (k, &value) in values.iter().enumerate() {
            let tile_id = if k == 0 {
                self.layout.focused()
            } else {
                // Stop where the charts would get too small to read.
                match self.layout.insert(area) {
                    Ok(tile_id) => tile_id,
                    Err(_) => break,
                }
            };
            let mut variant = spec.clone();
            if variant.set_param(param, value).is_err() {
                continue;
            }
            tasks.push(self.add_tile(tile_id, market.clone(), indicators.clone(), Some(variant)));
            if let Some(tile) = self.tiles.get_mut(&tile_id) {
                tile.sweep = Some(id);
            }
            charts.push((tile_id, value));
        }
        self.sweeps.insert(
            id,
            sweep::Sweep {
                name: spec.info.params[param].name.to_string(),
                charts,
            },
        );
        self.relayout(false);
        self.save();
        Task::batch(tasks)
    }

    /// The sweep tile `id` is in, as rows for its report.
    fn sweep_rows(&self, id: TileId) -> Option<(&str, Vec<SweepRow>)> {
        let sweep = self.sweeps.get(&self.tiles.get(&id)?.sweep?)?;
        let rows = sweep
            .charts
            .iter()
            .filter_map(|&(tile, value)| {
                let chart = self
                    .tiles
                    .get(&tile)
                    .or_else(|| self.stash.values().find_map(|w| w.tiles.get(&tile)))?;
                let result = match &chart.strategy.as_ref()?.run {
                    Run::Done(report) => SweepResult::Return(report.net_return()),
                    Run::Failed(_) => SweepResult::Failed,
                    Run::Waiting | Run::Running => SweepResult::Running,
                };
                Some(SweepRow {
                    value: sweep::label(value),
                    result,
                    current: tile == id,
                })
            })
            .collect();
        Some((sweep.name.as_str(), rows))
    }

    /// Gives every chart of `market` (on any timeframe, on screen or not) its current
    /// drawings, and saves them.
    fn show_drawings(&mut self, market: &Market) {
        let key = drawing_key(market);
        if self.drawings.get(&key).is_some_and(Vec::is_empty) {
            self.drawings.remove(&key);
        }
        let list = self.drawings.get(&key).cloned().unwrap_or_default();
        let tiles = self
            .tiles
            .values_mut()
            .chain(self.stash.values_mut().flat_map(|w| w.tiles.values_mut()));
        for tile in tiles {
            if drawing_key(&tile.market) == key
                && let Some(chart) = &mut tile.chart
            {
                chart.set_drawings(if tile.hidden.drawings {
                    Vec::new()
                } else {
                    list.clone()
                });
            }
        }
        if let Some(path) = &self.drawings_path
            && let Err(e) = persist::save_drawings(path, &self.drawings)
        {
            eprintln!(
                "persephone: couldn't save drawings to {}: {e}",
                path.display()
            );
        }
    }

    /// Puts `workspace` on screen and the current one in its place.
    fn swap_workspace(&mut self, workspace: &mut Workspace) {
        std::mem::swap(&mut self.layout, &mut workspace.layout);
        std::mem::swap(&mut self.tiles, &mut workspace.tiles);
    }

    /// Shows workspace `number`, opening it with a chart of the focused market if it's new.
    fn switch_workspace(&mut self, number: u8) -> Task<Message> {
        if number == self.workspace || !(1..=9).contains(&number) {
            return Task::none();
        }
        let market = self.focused_market();
        let mut next = self
            .stash
            .remove(&number)
            .unwrap_or_else(|| Workspace::empty(number));
        self.swap_workspace(&mut next);
        self.stash.insert(self.workspace, next);
        self.workspace = number;
        self.workspace_shown = Some(self.now);

        let mut tasks = Vec::new();
        if self.tiles.is_empty() {
            tasks.push(self.add_tile(self.layout.focused(), market, Vec::new(), None));
        }
        self.relayout(false);
        // Charts back on screen catch up: built if their bars came while away, redrawn with
        // backtests that finished meanwhile.
        let ids: Vec<TileId> = self.tiles.keys().copied().collect();
        for id in ids {
            match self.tiles.get_mut(&id).and_then(|t| t.chart.as_mut()) {
                Some(chart) => {
                    chart.set_linked_time(None);
                    self.restudy(id);
                }
                None => tasks.push(self.attach(id)),
            }
        }
        self.save();
        Task::batch(tasks)
    }

    /// Moves the focused chart to workspace `number`. The last chart on a workspace stays.
    fn move_to_workspace(&mut self, number: u8) -> Task<Message> {
        if number == self.workspace || !(1..=9).contains(&number) {
            return Task::none();
        }
        if self.layout.len() == 1 {
            self.refuse();
            return Task::none();
        }
        let area = self.tiling_area();
        let mut target = self
            .stash
            .remove(&number)
            .unwrap_or_else(|| Workspace::empty(number));
        let new_id = if target.tiles.is_empty() {
            Ok(target.layout.focused())
        } else {
            target.layout.insert(area)
        };
        let id = self.layout.focused();
        let (Ok(new_id), Some(mut tile)) = (new_id, self.tiles.remove(&id)) else {
            if !target.tiles.is_empty() {
                self.stash.insert(number, target);
            }
            self.refuse();
            return Task::none();
        };
        let _ = self.layout.close(id);
        // Rebuilt, and its backtest rerun, when its new workspace shows: results on the way
        // belong to its old id.
        tile.chart = None;
        if let Some(strategy) = &mut tile.strategy {
            strategy.generation += 1;
            strategy.run = Run::Waiting;
        }
        target.tiles.insert(new_id, tile);
        self.stash.insert(number, target);
        self.relayout(true);
        self.save();
        Task::none()
    }

    /// Flashes the focused tile's border; always returns `false` (nothing changed).
    fn refuse(&mut self) -> bool {
        if let Some(tile) = self.tiles.get_mut(&self.layout.focused()) {
            tile.flash = Some(self.now);
        }
        false
    }

    fn on_menu(&mut self, message: MenuMessage) -> Task<Message> {
        let Some(mut popup) = self.popup.take() else {
            return Task::none();
        };
        let id = popup.tile();
        let Some(market) = self.tiles.get(&id).map(|t| t.market.clone()) else {
            return Task::none();
        };

        match message {
            MenuMessage::Open(page) => {
                let search = matches!(page, Page::Symbol { .. });
                if let Popup::Menu { page: current, .. } = &mut popup {
                    *current = page;
                }
                self.popup = Some(popup);
                if search {
                    // Markets imported since the last look show up.
                    self.refresh_local_markets();
                    return operation::focus(menu::symbol_input());
                }
            }
            MenuMessage::AddIndicator(kind) => self.add_indicator(id, IndicatorSpec::new(kind)),
            MenuMessage::AddPluginIndicator(plugin) => {
                if let Some(loaded) = self.plugins.indicators.get(&plugin) {
                    let spec = IndicatorSpec::from_info(
                        IndicatorKind::Plugin(plugin),
                        Arc::clone(&loaded.info),
                        &[],
                    );
                    self.add_indicator(id, spec);
                }
            }
            MenuMessage::SetTimeframe(timeframe) => {
                return self.set_market(
                    id,
                    Market {
                        bars: timeframe.into(),
                        ..market
                    },
                );
            }
            MenuMessage::SetBars(bars) => {
                return self.set_market(id, Market { bars, ..market });
            }
            MenuMessage::BarsInput(text) => {
                if let Popup::Menu {
                    page: Page::Bars { input, error },
                    ..
                } = &mut popup
                {
                    *input = text;
                    *error = None;
                }
                self.popup = Some(popup);
            }
            MenuMessage::SubmitBars => {
                if let Popup::Menu {
                    page: Page::Bars { input, error },
                    ..
                } = &mut popup
                {
                    match input.parse::<BarSpec>() {
                        Ok(BarSpec::Volume(_))
                            if self.local_kind(&market) == Some(TickKind::Quotes) =>
                        {
                            *error = Some("quotes have no traded size; use ticks".into());
                        }
                        Ok(bars) => return self.set_market(id, Market { bars, ..market }),
                        Err(e) => *error = Some(e.to_string()),
                    }
                }
                self.popup = Some(popup);
            }
            MenuMessage::SetSymbol(symbol) => {
                return self.set_market(
                    id,
                    Market {
                        local: None,
                        symbol,
                        ..market
                    },
                );
            }
            MenuMessage::SetLocal { venue, symbol } => {
                return self.set_market(
                    id,
                    Market {
                        local: Some(venue),
                        symbol,
                        ..market
                    },
                );
            }
            MenuMessage::SubmitSymbol => {
                if let Popup::Menu {
                    page: Page::Symbol { query },
                    ..
                }
                | Popup::SymbolSearch { query, .. } = &popup
                {
                    let symbol = query.trim().to_uppercase();
                    if !symbol.is_empty() {
                        return self.set_market(
                            id,
                            Market {
                                local: None,
                                symbol,
                                ..market
                            },
                        );
                    }
                }
                self.popup = Some(popup);
            }
            MenuMessage::ImportSymbolChanged(text) => {
                if let Popup::Import { symbol, .. } = &mut popup {
                    *symbol = text.trim().to_uppercase();
                }
                self.popup = Some(popup);
            }
            MenuMessage::ImportVenueChanged(text) => {
                if let Popup::Import { venue, .. } = &mut popup {
                    *venue = text.trim().to_uppercase();
                }
                self.popup = Some(popup);
            }
            MenuMessage::ImportZoneChanged(text) => {
                if let Popup::Import { zone, .. } = &mut popup {
                    *zone = text.trim().to_owned();
                }
                self.popup = Some(popup);
            }
            MenuMessage::StartImport => return self.start_import(popup),
            MenuMessage::AllowAi(allow) => {
                self.popup = Some(popup);
                return self.allow_ai(id, allow);
            }
            MenuMessage::OpenSweep => {
                if let Some(spec) = self
                    .tiles
                    .get(&id)
                    .and_then(|t| t.strategy.as_ref())
                    .map(|s| &s.spec)
                    && let Some(param) = spec.info.params.first()
                {
                    let (from, to) = sweep::default_range(param, spec.params[0]);
                    let at = popup.at_point();
                    self.popup = Some(Popup::Sweep {
                        tile: id,
                        at,
                        param: 0,
                        inputs: [sweep::label(from), sweep::label(to), "4".into()],
                        error: None,
                    });
                    return Task::none();
                }
                self.popup = Some(popup);
            }
            MenuMessage::SweepParam(index) => {
                if let Popup::Sweep {
                    param,
                    inputs,
                    error,
                    ..
                } = &mut popup
                    && let Some(spec) = self
                        .tiles
                        .get(&id)
                        .and_then(|t| t.strategy.as_ref())
                        .map(|s| &s.spec)
                    && let Some(p) = spec.info.params.get(index)
                {
                    let (from, to) = sweep::default_range(p, spec.params[index]);
                    *param = index;
                    inputs[0] = sweep::label(from);
                    inputs[1] = sweep::label(to);
                    *error = None;
                }
                self.popup = Some(popup);
            }
            MenuMessage::SweepInput(index, value) => {
                if let Popup::Sweep { inputs, .. } = &mut popup
                    && let Some(input) = inputs.get_mut(index)
                {
                    *input = value;
                }
                self.popup = Some(popup);
            }
            MenuMessage::StartSweep => return self.start_sweep(popup),
            MenuMessage::Draw(tool) => {
                // Drawing on a chart whose drawings are hidden shows them again.
                if let Some(tile) = self.tiles.get_mut(&id)
                    && std::mem::take(&mut tile.hidden.drawings)
                {
                    self.show_drawings(&market);
                    self.save();
                }
                if let Some(chart) = self.tiles.get_mut(&id).and_then(|t| t.chart.as_mut()) {
                    chart.start_drawing(tool);
                }
            }
            MenuMessage::Show(what, shown) => {
                if let Some(tile) = self.tiles.get_mut(&id) {
                    match what {
                        Active::Indicator(index) if shown => {
                            tile.hidden.indicators.remove(&index);
                        }
                        Active::Indicator(index) => {
                            tile.hidden.indicators.insert(index);
                        }
                        Active::Strategy => tile.hidden.strategy = !shown,
                        Active::Volume => tile.hidden.volume = !shown,
                        Active::Drawings => tile.hidden.drawings = !shown,
                    }
                }
                if what == Active::Drawings {
                    self.show_drawings(&market);
                }
                self.restudy(id);
                self.save();
                self.popup = Some(popup);
            }
            MenuMessage::RemoveDrawing => {
                if let Popup::Drawing { index, .. } = popup
                    && let Some(list) = self.drawings.get_mut(&drawing_key(&market))
                    && index < list.len()
                {
                    list.remove(index);
                    self.show_drawings(&market);
                }
            }
            MenuMessage::ClearDrawings => {
                self.drawings.remove(&drawing_key(&market));
                self.show_drawings(&market);
            }
            MenuMessage::QueryChanged(text) => {
                if let Popup::Menu {
                    page: Page::Symbol { query },
                    ..
                }
                | Popup::SymbolSearch { query, .. } = &mut popup
                {
                    *query = text.to_uppercase();
                }
                self.popup = Some(popup);
            }
            MenuMessage::ParamChanged(index, value) => {
                if let Popup::Settings {
                    study,
                    inputs,
                    error,
                    ..
                } = &mut popup
                {
                    let spec = self
                        .tiles
                        .get_mut(&id)
                        .and_then(|t| t.indicators.get_mut(*study));
                    *error = edit_param(
                        inputs,
                        index,
                        &value,
                        spec.map(|s| move |i, v| s.set_param(i, v)),
                    );
                    if error.is_none() {
                        self.restudy(id);
                        self.save();
                    }
                }
                self.popup = Some(popup);
            }
            MenuMessage::RemoveStudy => {
                if let Popup::Settings { study, .. } = popup
                    && let Some(tile) = self.tiles.get_mut(&id)
                    && study < tile.indicators.len()
                {
                    tile.indicators.remove(study);
                    // Later indicators move up one.
                    tile.hidden.indicators = std::mem::take(&mut tile.hidden.indicators)
                        .into_iter()
                        .filter(|&i| i != study)
                        .map(|i| if i > study { i - 1 } else { i })
                        .collect();
                    self.restudy(id);
                    self.save();
                }
            }
            MenuMessage::SetStrategy(kind) => {
                return self.set_strategy(id, StrategySpec::new(kind));
            }
            MenuMessage::SetPluginStrategy(plugin) => {
                if let Some(loaded) = self.plugins.strategies.get(&plugin) {
                    let spec = StrategySpec::from_info(
                        StrategyKind::Plugin(plugin),
                        Arc::clone(&loaded.info),
                        &[],
                    );
                    return self.set_strategy(id, spec);
                }
            }
            MenuMessage::StrategyParamChanged(index, value) => {
                let mut task = Task::none();
                if let Popup::Report { inputs, error, .. } = &mut popup {
                    let spec = self
                        .tiles
                        .get_mut(&id)
                        .and_then(|t| t.strategy.as_mut())
                        .map(|s| &mut s.spec);
                    *error = edit_param(
                        inputs,
                        index,
                        &value,
                        spec.map(|s| move |i, v| s.set_param(i, v)),
                    );
                    if error.is_none() {
                        // A live session runs the old settings; it stops.
                        if let Some(tile) = self.tiles.get_mut(&id) {
                            tile.live = None;
                        }
                        self.save();
                        task = self.run_backtest(id);
                    }
                }
                self.popup = Some(popup);
                return task;
            }
            MenuMessage::RemoveStrategy => {
                if let Some(tile) = self.tiles.get_mut(&id) {
                    tile.strategy = None;
                    tile.live = None;
                }
                self.restudy(id);
                self.save();
            }
            MenuMessage::GoLive(mode) => {
                self.popup = Some(popup);
                return self.go_live(id, mode);
            }
            MenuMessage::StopLive => {
                if let Some(live) = self.tiles.get_mut(&id).and_then(|t| t.live.as_mut()) {
                    live.stop();
                }
                self.popup = Some(popup);
            }
            MenuMessage::Close => {}
        }
        Task::none()
    }

    fn add_indicator(&mut self, id: TileId, spec: IndicatorSpec) {
        if let Some(tile) = self.tiles.get_mut(&id) {
            tile.indicators.push(spec);
        }
        self.restudy(id);
        self.save();
    }

    /// Gives tile `id` a strategy (replacing any) and backtests it.
    fn set_strategy(&mut self, id: TileId, spec: StrategySpec) -> Task<Message> {
        if let Some(tile) = self.tiles.get_mut(&id) {
            let generation = tile.strategy.as_ref().map_or(0, |s| s.generation);
            tile.strategy = Some(Strategy {
                spec,
                run: Run::Waiting,
                generation,
            });
            tile.live = None;
        }
        self.save();
        self.run_backtest(id)
    }

    /// Loads the plugin folder on a worker thread after `delay`.
    fn reload_plugins(&mut self, delay: Duration) -> Task<Message> {
        let (Some(host), Some(dir)) = (self.host.clone(), self.plugin_dir.clone()) else {
            return Task::none();
        };
        self.reloading = true;
        Task::perform(
            on_thread(move || {
                std::thread::sleep(delay);
                Ok(host.load_dir(&dir))
            }),
            |loaded: Result<Plugins, String>| {
                Message::PluginsLoaded(Arc::new(loaded.unwrap_or_default()))
            },
        )
    }

    /// Switches to a new set of plugins: plugin indicators and strategies pick up their
    /// (possibly changed) descriptions, charts recompute, and plugin strategies rerun.
    fn use_plugins(&mut self, plugins: Arc<Plugins>) -> Task<Message> {
        self.plugins = plugins;
        let ids: Vec<TileId> = self.tiles.keys().copied().collect();
        let mut reruns = Vec::new();
        for id in ids {
            let Some(tile) = self.tiles.get_mut(&id) else {
                continue;
            };
            let mut uses_plugins = false;
            for spec in &mut tile.indicators {
                if let IndicatorKind::Plugin(_) = spec.kind {
                    uses_plugins = true;
                    if let Some(refreshed) = plugins::refresh_indicator(spec, &self.plugins) {
                        *spec = refreshed;
                    }
                }
            }
            let mut rerun = false;
            if let Some(strategy) = &mut tile.strategy
                && let StrategyKind::Plugin(_) = strategy.spec.kind
            {
                if let Some(refreshed) = plugins::refresh_strategy(&strategy.spec, &self.plugins) {
                    strategy.spec = refreshed;
                }
                rerun = true;
            }
            if uses_plugins {
                self.restudy(id);
            }
            if rerun {
                reruns.push(self.run_backtest(id));
            }
        }
        Task::batch(reruns)
    }

    /// Replaces the loaded plugins, as a reload would. For tests.
    #[doc(hidden)]
    pub fn set_plugins(&mut self, plugins: Plugins) -> Task<Message> {
        self.use_plugins(Arc::new(plugins))
    }

    /// The report popover for tile `id`'s strategy, tucked under its summary at the top right.
    fn report_popup(&self, id: TileId) -> Option<Popup> {
        let tile = self.tiles.get(&id)?;
        let strategy = tile.strategy.as_ref()?;
        let width = tile.motion.target().width;
        let at = Point::new(width - PRICE_AXIS_WIDTH - menu::REPORT_WIDTH - 12.0, 40.0);
        Some(Popup::report(id, at, &strategy.spec))
    }

    /// Adds a tile, loading its market's bars unless they're loaded or on the way.
    fn add_tile(
        &mut self,
        id: TileId,
        market: Market,
        indicators: Vec<IndicatorSpec>,
        strategy: Option<StrategySpec>,
    ) -> Task<Message> {
        let load = self.request(&market);
        self.tiles.insert(
            id,
            Tile {
                market,
                indicators,
                strategy: strategy.map(|spec| Strategy {
                    spec,
                    run: Run::Waiting,
                    generation: 0,
                }),
                chart: None,
                motion: Motion::at_rest(Rect::default()),
                flash: None,
                sweep: None,
                live: None,
                hidden: Hidden::default(),
            },
        );
        let backtest = self.attach(id);
        Task::batch([load, backtest])
    }

    /// Adds a tile as it was saved, with what its Active list had hidden.
    fn restore(
        &mut self,
        id: TileId,
        (market, indicators, strategy, hidden): Restored,
    ) -> Task<Message> {
        let task = self.add_tile(id, market, indicators, strategy);
        if let Some(tile) = self.tiles.get_mut(&id) {
            tile.hidden = hidden;
        }
        task
    }

    /// Points tile `id` at another market, keeping its indicators and strategy.
    fn set_market(&mut self, id: TileId, market: Market) -> Task<Message> {
        let Some(tile) = self.tiles.get_mut(&id) else {
            return Task::none();
        };
        if tile.market == market {
            return Task::none();
        }
        tile.market = market.clone();
        tile.chart = None;
        tile.live = None;
        if let Some(strategy) = &mut tile.strategy {
            strategy.run = Run::Waiting;
        }
        let load = self.request(&market);
        let backtest = self.attach(id);
        self.save();
        Task::batch([load, backtest])
    }

    /// Starts loading `market` unless its bars are loaded or on the way.
    fn request(&mut self, market: &Market) -> Task<Message> {
        match self.bars.get(market) {
            Some(Bars::Ready(_) | Bars::Loading) => Task::none(),
            Some(Bars::Failed(_)) | None => {
                self.bars.insert(market.clone(), Bars::Loading);
                load(market.clone(), self.store.clone())
            }
        }
    }

    /// Gives tile `id` a chart, with its indicators, if its market's bars are in, and starts
    /// its strategy's backtest.
    fn attach(&mut self, id: TileId) -> Task<Message> {
        let Some(tile) = self.tiles.get_mut(&id) else {
            return Task::none();
        };
        if tile.chart.is_some() {
            return Task::none();
        }
        let Some(Bars::Ready(market)) = self.bars.get(&tile.market) else {
            return Task::none();
        };
        let mut chart = ChartState::new(Arc::clone(&market.data));
        if !tile.hidden.drawings {
            chart.set_drawings(
                self.drawings
                    .get(&drawing_key(&tile.market))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        tile.chart = Some(chart);
        self.restudy(id);
        self.run_backtest(id)
    }

    /// Backtests tile `id`'s strategy on its bars, on a worker thread.
    fn run_backtest(&mut self, id: TileId) -> Task<Message> {
        let Some(strategy) = self.tiles.get_mut(&id).and_then(|t| t.strategy.as_mut()) else {
            return Task::none();
        };
        strategy.generation += 1;
        strategy.run = Run::Waiting;
        let Some((generation, request)) = self.backtest_request(id) else {
            return Task::none();
        };
        if let Some(strategy) = self.tiles.get_mut(&id).and_then(|t| t.strategy.as_mut()) {
            strategy.run = Run::Running;
        }
        Task::perform(backtest_on_thread(request), move |result| {
            Message::BacktestDone(id, generation, result)
        })
    }

    /// Reruns tile `id`'s backtest after its bars changed, showing the last result until the
    /// new one is in.
    fn run_backtest_keeping(&mut self, id: TileId) -> Task<Message> {
        let Some(strategy) = self.tiles.get_mut(&id).and_then(|t| t.strategy.as_mut()) else {
            return Task::none();
        };
        if !matches!(strategy.run, Run::Done(_)) {
            return self.run_backtest(id);
        }
        strategy.generation += 1;
        let Some((generation, request)) = self.backtest_request(id) else {
            return Task::none();
        };
        Task::perform(backtest_on_thread(request), move |result| {
            Message::BacktestDone(id, generation, result)
        })
    }

    /// Rebuilds what tile `id`'s chart draws: its indicators, plus its strategy's equity pane
    /// and trades once a backtest has finished, then a live session's trades from where it
    /// started.
    fn restudy(&mut self, id: TileId) {
        let Some(tile) = self.tiles.get_mut(&id) else {
            return;
        };
        let Some(chart) = tile.chart.as_mut() else {
            return;
        };
        let data = Arc::clone(chart.data());
        let shown: Vec<IndicatorSpec> = tile
            .indicators
            .iter()
            .enumerate()
            .filter(|(i, _)| !tile.hidden.indicators.contains(i))
            .map(|(_, spec)| spec.clone())
            .collect();
        let mut built = studies::build(&shown, &data.candles, &self.plugins);
        // Volume, then every indicator, gets a round tag in the header, lit while it's shown.
        chart.set_volume(!tile.hidden.volume);
        let volume = HeaderTag {
            label: "Vol".into(),
            shown: !tile.hidden.volume,
        };
        chart.set_tags(
            std::iter::once(volume)
                .chain(
                    tile.indicators
                        .iter()
                        .enumerate()
                        .map(|(i, spec)| HeaderTag {
                            label: spec.to_string(),
                            shown: !tile.hidden.indicators.contains(&i),
                        }),
                )
                .collect(),
        );
        // A report a bar or two short is the last one, while a rerun catches up with live bars.
        let len = data.candles.len();
        let strategy = tile.strategy.as_ref().filter(|_| !tile.hidden.strategy);
        let report = match strategy.map(|s| &s.run) {
            Some(Run::Done(report))
                if (len.saturating_sub(2)..=len).contains(&report.equity.len()) =>
            {
                Some(report)
            }
            _ => None,
        };
        if let Some(report) = report {
            built.push(studies::equity(report, len));
        }
        chart.set_studies(built);
        let live = tile
            .live
            .as_ref()
            .filter(|_| !tile.hidden.strategy)
            .and_then(|l| Some((l.since?, l.fills.as_slice())));
        chart.set_trades(match (report, live) {
            (report, Some((since, fills))) => Some(studies::live_trades(
                report.map_or(&[], |r| r.fills.as_slice()),
                fills,
                since,
                &data.candles,
            )),
            (Some(report), None) => Some(studies::trades(report, len)),
            (None, None) => None,
        });
    }

    fn focused_market(&self) -> Market {
        self.tiles.get(&self.layout.focused()).map_or_else(
            || Market {
                local: None,
                symbol: DEFAULT_SYMBOL.into(),
                bars: Timeframe::M1.into(),
            },
            |tile| tile.market.clone(),
        )
    }

    fn window_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, self.window.width, self.window.height)
    }

    /// The area tiles are arranged in when there is more than one.
    fn tiling_area(&self) -> Rect {
        self.window_rect().inset(GAP)
    }

    /// Points every tile at where the layout wants it, gliding there if `animate`.
    fn relayout(&mut self, animate: bool) {
        let full = self.window_rect();
        let targets = if self.layout.len() == 1 {
            vec![(self.layout.focused(), full)]
        } else {
            self.layout.arrange(self.tiling_area(), GAP)
        };
        let fullscreen = self.layout.is_fullscreen().then(|| self.layout.focused());

        for (id, rect) in targets {
            let rect = if fullscreen == Some(id) { full } else { rect };
            if let Some(tile) = self.tiles.get_mut(&id) {
                if animate {
                    tile.motion.retarget(rect, self.now);
                } else {
                    tile.motion.snap(rect);
                }
            }
        }
    }

    fn save(&self) {
        let Some(path) = &self.save_path else { return };
        let saved = Saved {
            layout: self.layout.clone(),
            tiles: saved_tiles(&self.tiles),
            workspace: self.workspace,
            others: self
                .stash
                .iter()
                .map(|(&number, w)| SavedWorkspace {
                    number,
                    layout: w.layout.clone(),
                    tiles: saved_tiles(&w.tiles),
                })
                .collect(),
        };
        if let Err(e) = persist::save(path, &saved) {
            eprintln!(
                "persephone: couldn't save the layout to {}: {e}",
                path.display()
            );
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        let input = event::listen_with(|event, _status, _window| match event {
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                physical_key: Physical::Code(Code::Escape),
                ..
            }) => Some(Message::Escape),
            iced::Event::Keyboard(keyboard::Event::KeyPressed {
                physical_key,
                modifiers,
                repeat,
                ..
            }) => keymap::action(physical_key, modifiers, repeat).map(Message::Key),
            iced::Event::Window(
                window::Event::Opened { size, .. } | window::Event::Resized(size),
            ) => Some(Message::WindowResized(size)),
            iced::Event::Window(window::Event::FileHovered(_)) => Some(Message::FilesHovered(true)),
            iced::Event::Window(window::Event::FilesHoveredLeft) => {
                Some(Message::FilesHovered(false))
            }
            iced::Event::Window(window::Event::FileDropped(path)) => {
                Some(Message::FileDropped(path))
            }
            _ => None,
        });

        let mut subscriptions = vec![input];
        if let Some(dir) = &self.plugin_dir {
            subscriptions.push(Subscription::run_with(dir.clone(), |dir: &PathBuf| {
                plugins::watch(dir)
            }));
        }
        if self.is_animating() {
            subscriptions.push(window::frames().map(Message::Tick));
        }
        Subscription::batch(subscriptions)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let focused = self.layout.focused();
        let fullscreen = self.layout.is_fullscreen();
        let framed = self.layout.len() > 1 && !fullscreen;

        let mut layers: Vec<Element<'_, Message>> = self
            .tiles
            .iter()
            .filter(|(id, _)| !fullscreen || **id == focused)
            .map(|(&id, tile)| self.tile_view(id, tile, framed, id == focused))
            .collect();

        // The popup goes above every tile, so neighbouring charts can't cover it.
        if let Some(popup) = &self.popup
            && let Some(tile) = self.tiles.get(&popup.tile())
        {
            let rect = tile.motion.rect(self.now);
            let border = if framed { BORDER } else { 0.0 };
            let ready = match self.bars.get(&tile.market) {
                Some(Bars::Ready(market)) => Some(market),
                _ => None,
            };
            let ai_allowed = tile
                .strategy
                .as_ref()
                .and_then(|s| match &s.spec.kind {
                    StrategyKind::Plugin(id) => self.plugins.strategies.get(id),
                    StrategyKind::BuiltIn(_) => None,
                })
                .filter(|plugin| plugin.uses_ai)
                .map(|plugin| plugin.ai_allowed());
            let info = TileInfo {
                symbol: &tile.market.symbol,
                venue: tile.market.local.as_deref(),
                ai_allowed,
                drawings: tile.chart.as_ref().map(ChartState::drawings).unwrap_or(&[]),
                free_workspace: self.free_workspace(),
                sweep: self.sweep_rows(popup.tile()),
                quote: ready.map_or("", |m| m.instrument.quote.as_str()),
                base: ready.map_or("", |m| m.instrument.base.as_str()),
                fills: ready.and_then(|m| m.fills.as_deref()),
                local_markets: &self.local_markets,
                bars: tile.market.bars,
                tick_kind: self.local_kind(&tile.market),
                specs: &tile.indicators,
                strategy: tile.strategy.as_ref().map(|s| (&s.spec, &s.run)),
                live: LiveInfo {
                    session: tile.live.as_ref(),
                    unavailable: self.live_unavailable(popup.tile()),
                    testnet: LiveMode::Testnet.has_keys(),
                },
                hidden: &tile.hidden,
                plugins: &self.plugins,
                plugin_dir: self.plugin_dir.as_deref(),
            };
            let origin = Point::new(rect.x + border, rect.y + border);
            layers.push(menu::view(popup, info, origin, self.window).map(Message::Menu));
        }
        // Which workspace this is, for a moment after switching.
        if self
            .workspace_shown
            .is_some_and(|start| self.now.saturating_duration_since(start) < WORKSPACE_BADGE)
        {
            let others: Vec<String> = self.stash.keys().map(u8::to_string).collect();
            let label = if others.is_empty() {
                format!("Workspace {}", self.workspace)
            } else {
                format!(
                    "Workspace {}   ·   also {}",
                    self.workspace,
                    others.join(", ")
                )
            };
            let badge = container(text(label).size(13).color(theme::TEXT_BRIGHT))
                .padding([6, 14])
                .style(|_| container::Style {
                    background: Some(Background::Color(Color {
                        a: 0.92,
                        ..theme::BACKGROUND
                    })),
                    border: Border {
                        color: theme::ACCENT,
                        width: 1.0,
                        radius: 6.0.into(),
                    },
                    ..container::Style::default()
                });
            layers.push(
                container(badge)
                    .center_x(Fill)
                    .padding(Padding {
                        top: 14.0,
                        ..Padding::ZERO
                    })
                    .into(),
            );
        }

        container(stack(layers).width(Fill).height(Fill))
            .style(|_| container::background(theme::DESKTOP))
            .into()
    }

    fn tile_view<'a>(
        &'a self,
        id: TileId,
        tile: &'a Tile,
        framed: bool,
        focused: bool,
    ) -> Element<'a, Message> {
        let rect = tile.motion.rect(self.now);
        let flash = tile
            .flash
            .map(|start| self.now.saturating_duration_since(start))
            .filter(|elapsed| *elapsed < FLASH)
            .map(|elapsed| 1.0 - elapsed.as_secs_f32() / FLASH.as_secs_f32());

        let border = if framed || flash.is_some() {
            BORDER
        } else {
            0.0
        };
        let idle = if focused {
            theme::ACCENT
        } else {
            theme::AXIS_LINE
        };
        let color = flash.map_or(idle, |strength| mix(idle, theme::DOWN, strength));

        let mut content: Element<'a, Message> = match &tile.chart {
            Some(chart) => chart.view().map(move |message| Message::Chart(id, message)),
            None => placeholder(&tile.market, self.bars.get(&tile.market)),
        };
        if let Some(strategy) = tile.strategy.as_ref().filter(|_| !tile.hidden.strategy) {
            let compact = rect.width < 640.0;
            let summary = container(strategy_summary(id, strategy, tile.live.as_ref(), compact))
                .align_right(Fill)
                .padding(Padding {
                    top: 6.0,
                    right: PRICE_AXIS_WIDTH + 10.0,
                    bottom: 0.0,
                    left: 0.0,
                });
            content = stack![content, summary].into();
        }
        // In the corner where the price and time axes meet.
        let buttons = container(tile_buttons(id))
            .align_right(Fill)
            .align_bottom(Fill)
            .padding(Padding {
                top: 0.0,
                right: (PRICE_AXIS_WIDTH - TILE_BUTTONS_WIDTH) / 2.0,
                bottom: (TIME_AXIS_HEIGHT - 20.0) / 2.0,
                left: 0.0,
            });
        content = stack![content, buttons].into();
        if focused && self.drop_hint {
            let hint = container(
                text("Drop CSV tick files to import them into this chart")
                    .size(15)
                    .color(theme::TEXT_BRIGHT),
            )
            .center(Fill)
            .style(|_| {
                container::background(Color {
                    a: 0.82,
                    ..theme::BACKGROUND
                })
            });
            content = stack![content, hint].into();
        }

        let frame = container(content)
            .padding(border)
            .width(Length::Fixed(rect.width.max(0.0)))
            .height(Length::Fixed(rect.height.max(0.0)))
            .style(move |_| container::Style {
                background: Some(theme::BACKGROUND.into()),
                border: Border {
                    color,
                    width: border,
                    radius: 0.0.into(),
                },
                ..container::Style::default()
            });

        pin(mouse_area(frame).on_enter(Message::Hover(id)))
            .x(rect.x)
            .y(rect.y)
            .into()
    }
}

/// One corner button's width, and all three together, 2px apart.
const TILE_BUTTON: f32 = 22.0;
const TILE_BUTTONS_WIDTH: f32 = 3.0 * TILE_BUTTON + 4.0;

/// A chart's corner buttons: a new chart (split from this one), flip the split, and close.
/// Hovering one names it with its shortcut.
fn tile_buttons<'a>(id: TileId) -> Element<'a, Message> {
    let modifier = if cfg!(target_os = "macos") {
        "⌥"
    } else {
        "Alt+"
    };
    let one =
        |icon: &'a str, size: f32, what: &str, key: &str, action: Action| -> Element<'a, Message> {
            let tip = container(
                row![
                    text(what.to_owned()).size(12).color(theme::TEXT_BRIGHT),
                    text(format!("{modifier}{key}"))
                        .size(12)
                        .color(theme::TEXT_MUTED)
                        .font(iced::Font::MONOSPACE),
                ]
                .spacing(8),
            )
            .padding([3, 7])
            .style(|_| container::Style {
                background: Some(Background::Color(theme::LABEL_BACKGROUND)),
                border: Border {
                    radius: 4.0.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            });
            let control = button(text(icon).size(size).center())
                .width(TILE_BUTTON)
                .height(20)
                .padding(0)
                .on_press(Message::TileButton(id, action))
                .style(|_, status| {
                    let (background, border, text) = match status {
                        button::Status::Hovered => {
                            (theme::CROSSHAIR, theme::CROSSHAIR, Color::WHITE)
                        }
                        button::Status::Pressed => (theme::ACCENT, theme::ACCENT, Color::WHITE),
                        _ => (
                            theme::AXIS_LINE,
                            theme::LABEL_BACKGROUND,
                            theme::TEXT_BRIGHT,
                        ),
                    };
                    button::Style {
                        background: Some(Background::Color(background)),
                        text_color: text,
                        border: Border {
                            color: border,
                            width: 1.0,
                            radius: 3.0.into(),
                        },
                        ..button::Style::default()
                    }
                });
            tooltip(control, tip, tooltip::Position::Top).gap(4).into()
        };
    row![
        one("+", 17.0, "New chart", "Enter", Action::NewTile),
        one("◫", 14.0, "Flip split direction", "T", Action::ToggleSplit),
        one("✕", 13.0, "Close chart", "Q", Action::CloseTile),
    ]
    .spacing(2)
    .into()
}

/// The clickable strategy summary in a chart's top-right corner: name and headline numbers,
/// or what the backtest is doing. Narrow charts get just the return.
fn strategy_summary<'a>(
    id: TileId,
    strategy: &'a Strategy,
    live: Option<&Live>,
    compact: bool,
) -> Element<'a, Message> {
    let mono = |content: String, color: Color| {
        text(content)
            .size(12)
            .color(color)
            .font(iced::Font::MONOSPACE)
    };
    let mut pieces = row![
        text(strategy.spec.info.name.as_str())
            .size(12)
            .color(theme::TEXT_BRIGHT)
    ]
    .spacing(10);
    pieces = match &strategy.run {
        Run::Waiting => pieces.push(mono("waiting for bars".into(), theme::TEXT_MUTED)),
        Run::Running => pieces.push(mono("backtesting…".into(), theme::TEXT_MUTED)),
        Run::Failed(_) => pieces.push(mono("couldn't run — click for details".into(), theme::DOWN)),
        Run::Done(report) => {
            let ret = report.net_return() * 100.0;
            let color = if ret >= 0.0 { theme::UP } else { theme::DOWN };
            let mut pieces = pieces.push(mono(format!("{ret:+.2}%"), color));
            // A caveat (look-ahead, unanswered questions) is in the report.
            if report.notes.iter().any(|n| n.warning) {
                pieces = pieces.push(mono("!".into(), theme::DOWN));
            }
            if compact {
                pieces
            } else {
                let win = report
                    .stat("Win Rate")
                    .map_or("—".into(), |w| format!("{:.0}%", w * 100.0));
                pieces
                    .push(mono(
                        format!("DD {:.1}%", report.max_drawdown * 100.0),
                        theme::TEXT,
                    ))
                    .push(mono(format!("Win {win}"), theme::TEXT))
                    .push(mono(format!("{} trades", report.fills.len()), theme::TEXT))
            }
        }
    };

    // A live session leads with a dot, then its profit so far.
    if let Some(live) = live {
        let pnl = live.book.pnl();
        let (dot, color) = match &live.state {
            LiveState::Running if pnl < 0.0 => ("●", theme::DOWN),
            LiveState::Running => ("●", theme::UP),
            LiveState::Stopped(Some(_)) => ("○", theme::DOWN),
            LiveState::Stopped(None) => ("○", theme::TEXT_MUTED),
            _ => ("●", theme::TEXT_MUTED),
        };
        let mut piece = row![
            mono(dot.into(), color),
            mono(live.mode.label().into(), theme::TEXT_BRIGHT)
        ]
        .spacing(4);
        if live.book.fills > 0 || matches!(live.state, LiveState::Stopped(_)) {
            piece = piece.push(mono(format!("{pnl:+.2}"), color));
        }
        pieces = row![piece, pieces].spacing(12);
    }

    button(pieces)
        .padding([3, 9])
        .on_press(Message::OpenReport(id))
        .style(|_, status| button::Style {
            background: Some(Background::Color(Color {
                a: if matches!(status, button::Status::Hovered) {
                    1.0
                } else {
                    0.88
                },
                ..Color::from_rgb8(0x1e, 0x22, 0x2d)
            })),
            text_color: theme::TEXT_BRIGHT,
            border: Border {
                color: Color::from_rgb8(0x36, 0x3a, 0x45),
                width: 1.0,
                radius: 4.0.into(),
            },
            ..button::Style::default()
        })
        .into()
}

/// Applies a typed parameter value: remembers the text, parses it, and lets `set` validate
/// it. Returns the error to show, if any.
fn edit_param(
    inputs: &mut [String],
    index: usize,
    value: &str,
    set: Option<impl FnOnce(usize, f64) -> Result<(), String>>,
) -> Option<String> {
    if let Some(input) = inputs.get_mut(index) {
        value.clone_into(input);
    }
    let set = set?;
    match value.trim().parse::<f64>() {
        Ok(number) => set(index, number).err(),
        Err(_) => Some("Enter a number".into()),
    }
}

fn placeholder<'a>(market: &Market, bars: Option<&Bars>) -> Element<'a, Message> {
    let status = match bars {
        Some(Bars::Failed(error)) => {
            text(format!("{} {}: {error}", market.symbol, market.bars)).color(theme::DOWN)
        }
        _ => text(format!(
            "Loading {} {} from Binance…",
            market.symbol, market.bars
        ))
        .color(theme::TEXT),
    };
    container(status.size(13))
        .center_x(Fill)
        .center_y(Fill)
        .into()
}

/// Where a popup opened from the chart's header goes: just under the header line, so it
/// stays readable.
fn below_header(at: Point) -> Point {
    Point::new((at.x - 16.0).max(0.0), 30.0)
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let lerp = |x: f32, y: f32| x + (y - x) * t;
    Color::from_rgba(
        lerp(a.r, b.r),
        lerp(a.g, b.g),
        lerp(a.b, b.b),
        lerp(a.a, b.a),
    )
}

/// Which drawings a market shows: the same symbol from the same source, on any timeframe.
#[must_use]
pub fn drawing_key(market: &Market) -> String {
    match &market.local {
        Some(venue) => format!("local/{venue}/{}", market.symbol),
        None => format!("binance/{}", market.symbol),
    }
}

/// A saved tile's market, indicators and strategy.
/// A saved tile, read back: its market, indicators, strategy, and what was hidden.
type Restored = (Market, Vec<IndicatorSpec>, Option<StrategySpec>, Hidden);

fn restore_tile(t: &SavedTile) -> Restored {
    let market = Market {
        local: t.local.clone(),
        symbol: t.symbol.clone(),
        bars: t.timeframe.parse().unwrap_or(BarSpec::Time(Timeframe::M1)),
    };
    let mut hidden = Hidden {
        drawings: t.drawings_hidden,
        volume: t.volume_hidden,
        ..Hidden::default()
    };
    let mut indicators = Vec::new();
    for saved in &t.indicators {
        if let Some(spec) = plugins::saved_indicator(&saved.kind, &saved.params) {
            if saved.hidden {
                hidden.indicators.insert(indicators.len());
            }
            indicators.push(spec);
        }
    }
    let strategy = t.strategy.as_ref().and_then(|s| {
        hidden.strategy = s.hidden;
        plugins::saved_strategy(&s.kind, &s.params)
    });
    (market, indicators, strategy, hidden)
}

fn saved_tiles(tiles: &BTreeMap<TileId, Tile>) -> Vec<SavedTile> {
    tiles
        .iter()
        .map(|(&id, tile)| SavedTile {
            id,
            local: tile.market.local.clone(),
            symbol: tile.market.symbol.clone(),
            timeframe: tile.market.bars.label(),
            indicators: tile
                .indicators
                .iter()
                .enumerate()
                .map(|(i, spec)| SavedIndicator {
                    kind: plugins::indicator_key(&spec.kind),
                    params: spec.params.clone(),
                    hidden: tile.hidden.indicators.contains(&i),
                })
                .collect(),
            strategy: tile.strategy.as_ref().map(|s| SavedStrategy {
                kind: plugins::strategy_key(&s.spec.kind),
                params: s.spec.params.clone(),
                hidden: tile.hidden.strategy,
            }),
            drawings_hidden: tile.hidden.drawings,
            volume_hidden: tile.hidden.volume,
        })
        .collect()
}

fn market_from_args(args: &[String]) -> Market {
    let symbol = args
        .first()
        .map_or(DEFAULT_SYMBOL.into(), |s| s.to_uppercase());
    // `SYMBOL.VENUE` is a market in the local store.
    let (symbol, local) = match symbol.rsplit_once('.') {
        Some((symbol, venue)) => (symbol.to_owned(), Some(venue.to_owned())),
        None => (symbol, None),
    };
    let bars = match args.get(1).map(|tf| tf.parse::<BarSpec>()) {
        Some(Ok(bars)) => bars,
        Some(Err(e)) => {
            eprintln!("persephone: {e}; using 1m");
            Timeframe::M1.into()
        }
        None => Timeframe::M1.into(),
    };
    Market {
        local,
        symbol,
        bars,
    }
}

fn load(market: Market, store: Option<Store>) -> Task<Message> {
    let job = market.clone();
    Task::perform(
        on_thread(move || Ok(fetch(&job, store.as_ref()))),
        move |result| Message::Loaded(market.clone(), result.and_then(|r| r)),
    )
}

/// Runs `job` on a plain thread and hands its result back to iced's executor. Used for the
/// blocking Binance client, reading the store, and backtests, which need a thread of their
/// own anyway.
async fn on_thread<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(job());
    });
    rx.await
        .map_err(|_| "the worker thread stopped".to_string())?
}

async fn backtest_on_thread(request: BacktestRequest) -> Result<Arc<BacktestReport>, String> {
    on_thread(move || {
        persephone_engine::run_backtest(&request)
            .map(Arc::new)
            .map_err(|e| e.to_string())
    })
    .await
}

/// A market's bars: from the local store, or from Binance.
fn fetch(market: &Market, store: Option<&Store>) -> Result<MarketData, String> {
    let result = match &market.local {
        Some(venue) => match store {
            Some(store) => local::load(store, venue, &market.symbol, market.bars),
            None => Err(anyhow::anyhow!("there's no local store on this system")),
        },
        None => match market.bars.timeframe() {
            Some(timeframe) => fetch_binance(&market.symbol, timeframe),
            None => Err(anyhow::anyhow!(
                "Binance serves time bars; tick and volume bars need a local market"
            )),
        },
    };
    result.map_err(|e| format!("{e:#}"))
}

fn fetch_binance(symbol: &str, timeframe: Timeframe) -> anyhow::Result<MarketData> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let end = now - now % timeframe.nanos() + timeframe.nanos(); // include the forming bar
    let start = end.saturating_sub(HISTORY_BARS * timeframe.nanos());

    let binance = BinanceClient::new()?;
    let instrument = binance.instrument(symbol)?;
    let candles = binance.klines(symbol, timeframe, start, end)?;
    anyhow::ensure!(!candles.is_empty(), "Binance returned no bars for {symbol}");
    let data = ChartData::new(
        instrument.symbol.clone(),
        instrument.venue.clone(),
        timeframe,
        instrument.price_precision,
        candles,
    );
    Ok(MarketData {
        data,
        instrument,
        fees: FeeRates::BINANCE_SPOT,
        ticks: None,
        fills: None,
    })
}
