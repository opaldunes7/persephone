//! Persephone: a tiled, chart-only backtesting app.
//!
//! Usage: `persephone [SYMBOL] [TIMEFRAME]`. With no arguments the last layout is restored
//! (one Binance BTCUSDT 1m chart on first launch). With arguments you get a single chart for
//! that market, and the session's layout is not saved. `SYMBOL.VENUE` (e.g. `EURUSD.LOCAL`)
//! opens a market from the local store, filled by `persephone-cli import`.
//!
//! The window is only charts, tiled like Hyprland's dwindle layout; see [`keymap`] for keys.
//! Left-click a chart for its [`menu`]: indicators, a strategy to backtest, symbol and
//! timeframe. Custom indicators and strategies are WebAssembly [`plugins`] dropped into the
//! plugins folder; they load, and reload when the files change, without a restart. CSV tick
//! files dropped on a chart import into the local store and open there.

mod importing;
pub mod keymap;
mod local;
pub mod menu;
mod motion;
pub mod persist;
pub mod plugins;
mod studies;

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
    widget::{button, container, mouse_area, operation, pin, row, stack, text},
    window,
};
use persephone_chart::{ChartData, ChartMessage, ChartState, PRICE_AXIS_WIDTH, theme};
use persephone_core::{
    InstrumentSpec, Timeframe,
    indicator::{IndicatorKind, IndicatorSpec},
    signal::SignalFactory,
    strategy::{StrategyKind, StrategySpec},
    tick::TickFeed,
};
use persephone_data::{
    binance::BinanceClient,
    import::{Detection, Format, MarketOptions, Progress, Summary, Zone, split_symbol},
    store::{MarketInfo, Store, check_name},
};
use persephone_engine::{BacktestReport, BacktestRequest, FeeRates};
use persephone_plugin_host::{Host, Plugins};
use persephone_tiling::{Layout, Rect, TileId};

use crate::{
    keymap::Action,
    menu::{ImportState, MenuMessage, Page, Popup, TileInfo},
    motion::Motion,
    persist::{Saved, SavedIndicator, SavedStrategy, SavedTile},
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
    timeframe: Timeframe,
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
}

pub struct Persephone {
    layout: Layout,
    tiles: BTreeMap<TileId, Tile>,
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

        let tiles: Vec<(TileId, Market, Vec<IndicatorSpec>, Option<StrategySpec>)> = match &saved {
            Some(saved) => saved
                .tiles
                .iter()
                .map(|t| {
                    let market = Market {
                        local: t.local.clone(),
                        symbol: t.symbol.clone(),
                        timeframe: t.timeframe.parse().unwrap_or(Timeframe::M1),
                    };
                    let indicators = t
                        .indicators
                        .iter()
                        .filter_map(|i| plugins::saved_indicator(&i.kind, &i.params))
                        .collect();
                    let strategy = t
                        .strategy
                        .as_ref()
                        .and_then(|s| plugins::saved_strategy(&s.kind, &s.params));
                    (t.id, market, indicators, strategy)
                })
                .collect(),
            None => vec![(0, market_from_args(args), Vec::new(), None)],
        };

        let mut app = Self {
            layout: saved.map_or_else(Layout::new, |s| s.layout),
            tiles: BTreeMap::new(),
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
        };
        app.refresh_local_markets();
        let loads: Vec<_> = tiles
            .into_iter()
            .map(|(id, market, indicators, strategy)| {
                app.add_tile(id, market, indicators, strategy)
            })
            .collect();
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
            timeframe: data.timeframe,
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
    pub fn tile_market(&self, id: TileId) -> Option<(&str, Timeframe)> {
        self.tiles
            .get(&id)
            .map(|tile| (tile.market.symbol.as_str(), tile.market.timeframe))
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
        self.tiles.values().any(|tile| {
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
            timeframe: tile.market.timeframe,
            candles: market.data.candles.clone(),
            strategy: strategy.spec.clone(),
            plugin,
            ticks: market.ticks.clone(),
            starting_balance: STARTING_BALANCE,
            fees: market.fees,
        };
        Some((strategy.generation, request))
    }

    fn title(&self) -> String {
        match self.tiles.get(&self.layout.focused()) {
            Some(tile) => format!(
                "{} · {} — Persephone",
                tile.market.symbol, tile.market.timeframe
            ),
            None => "Persephone".into(),
        }
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
            Message::Chart(id, ChartMessage::Clicked { at }) => {
                // A click closes whatever is open; on a chart with nothing open, it opens the menu.
                self.popup = match self.popup.take() {
                    Some(_) => None,
                    None => Some(Popup::Menu {
                        tile: id,
                        at,
                        page: Page::Root,
                    }),
                };
            }
            Message::Chart(id, ChartMessage::StudyClicked { study, at }) => {
                let Some(tile) = self.tiles.get(&id) else {
                    return Task::none();
                };
                // Studies after the indicators belong to the strategy (its equity pane).
                self.popup = match tile.indicators.get(study) {
                    Some(spec) => Some(Popup::settings(id, study, at, spec)),
                    None => self.report_popup(id),
                };
            }
            Message::Chart(id, message) => {
                if let Some(chart) = self.tiles.get_mut(&id).and_then(|t| t.chart.as_mut()) {
                    chart.update(message);
                }
            }
            Message::Menu(message) => return self.on_menu(message),
            Message::Escape => self.popup = None,
            Message::OpenReport(id) => {
                let open = matches!(&self.popup, Some(Popup::Report { tile, .. }) if *tile == id);
                self.popup = if open { None } else { self.report_popup(id) };
            }
            Message::BacktestDone(id, generation, result) => {
                if let Some(strategy) = self
                    .tiles
                    .get_mut(&id)
                    .and_then(|t| t.strategy.as_mut())
                    .filter(|s| s.generation == generation)
                {
                    strategy.run = match result {
                        Ok(report) => Run::Done(report),
                        Err(error) => Run::Failed(error),
                    };
                    self.restudy(id);
                }
            }
            Message::Key(action) => return self.act(action),
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
        }
        Task::none()
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

    /// Flashes the focused tile's border; always returns `false` (nothing changed).
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
                timeframe: t.market.timeframe,
            };
            tasks.push(self.set_market(id, market));
        }
        Task::batch(tasks)
    }

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
                        timeframe,
                        ..market
                    },
                );
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
                } = &popup
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
            MenuMessage::QueryChanged(text) => {
                if let Popup::Menu {
                    page: Page::Symbol { query },
                    ..
                } = &mut popup
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
                }
                self.restudy(id);
                self.save();
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
            },
        );
        let backtest = self.attach(id);
        Task::batch([load, backtest])
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
        tile.chart = Some(ChartState::new(Arc::clone(&market.data)));
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

    /// Rebuilds what tile `id`'s chart draws: its indicators, plus its strategy's equity pane
    /// and trades once a backtest has finished.
    fn restudy(&mut self, id: TileId) {
        let Some(tile) = self.tiles.get_mut(&id) else {
            return;
        };
        let Some(chart) = tile.chart.as_mut() else {
            return;
        };
        let data = Arc::clone(chart.data());
        let mut built = studies::build(&tile.indicators, &data.candles, &self.plugins);
        let report = match tile.strategy.as_ref().map(|s| &s.run) {
            Some(Run::Done(report)) if report.equity.len() == data.candles.len() => Some(report),
            _ => None,
        };
        if let Some(report) = report {
            built.push(studies::equity(report));
        }
        chart.set_studies(built);
        chart.set_trades(report.map(|r| studies::trades(r, data.candles.len())));
    }

    fn focused_market(&self) -> Market {
        self.tiles.get(&self.layout.focused()).map_or_else(
            || Market {
                local: None,
                symbol: DEFAULT_SYMBOL.into(),
                timeframe: Timeframe::M1,
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
            tiles: self
                .tiles
                .iter()
                .map(|(&id, tile)| SavedTile {
                    id,
                    local: tile.market.local.clone(),
                    symbol: tile.market.symbol.clone(),
                    timeframe: tile.market.timeframe.label().into(),
                    indicators: tile
                        .indicators
                        .iter()
                        .map(|spec| SavedIndicator {
                            kind: plugins::indicator_key(&spec.kind),
                            params: spec.params.clone(),
                        })
                        .collect(),
                    strategy: tile.strategy.as_ref().map(|s| SavedStrategy {
                        kind: plugins::strategy_key(&s.spec.kind),
                        params: s.spec.params.clone(),
                    }),
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
                quote: ready.map_or("", |m| m.instrument.quote.as_str()),
                fills: ready.and_then(|m| m.fills.as_deref()),
                local_markets: &self.local_markets,
                timeframe: tile.market.timeframe,
                specs: &tile.indicators,
                strategy: tile.strategy.as_ref().map(|s| (&s.spec, &s.run)),
                plugins: &self.plugins,
                plugin_dir: self.plugin_dir.as_deref(),
            };
            let origin = Point::new(rect.x + border, rect.y + border);
            layers.push(menu::view(popup, info, origin, self.window).map(Message::Menu));
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
        if let Some(strategy) = &tile.strategy {
            let compact = rect.width < 640.0;
            let summary = container(strategy_summary(id, strategy, compact))
                .align_right(Fill)
                .padding(Padding {
                    top: 6.0,
                    right: PRICE_AXIS_WIDTH + 10.0,
                    bottom: 0.0,
                    left: 0.0,
                });
            content = stack![content, summary].into();
        }
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

/// The clickable strategy summary in a chart's top-right corner: name and headline numbers,
/// or what the backtest is doing. Narrow charts get just the return.
fn strategy_summary(id: TileId, strategy: &Strategy, compact: bool) -> Element<'_, Message> {
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
            text(format!("{} {}: {error}", market.symbol, market.timeframe)).color(theme::DOWN)
        }
        _ => text(format!(
            "Loading {} {} from Binance…",
            market.symbol, market.timeframe
        ))
        .color(theme::TEXT),
    };
    container(status.size(13))
        .center_x(Fill)
        .center_y(Fill)
        .into()
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

fn market_from_args(args: &[String]) -> Market {
    let symbol = args
        .first()
        .map_or(DEFAULT_SYMBOL.into(), |s| s.to_uppercase());
    // `SYMBOL.VENUE` is a market in the local store.
    let (symbol, local) = match symbol.rsplit_once('.') {
        Some((symbol, venue)) => (symbol.to_owned(), Some(venue.to_owned())),
        None => (symbol, None),
    };
    let timeframe = match args.get(1).map(|tf| tf.parse::<Timeframe>()) {
        Some(Ok(tf)) => tf,
        Some(Err(e)) => {
            eprintln!("persephone: {e}; using 1m");
            Timeframe::M1
        }
        None => Timeframe::M1,
    };
    Market {
        local,
        symbol,
        timeframe,
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
            Some(store) => local::load(store, venue, &market.symbol, market.timeframe),
            None => Err(anyhow::anyhow!("there's no local store on this system")),
        },
        None => fetch_binance(&market.symbol, market.timeframe),
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
