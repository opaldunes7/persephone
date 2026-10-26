//! Persephone: a tiled, chart-only backtesting app.
//!
//! Usage: `persephone [SYMBOL] [TIMEFRAME]`. With no arguments the last layout is restored
//! (one Binance BTCUSDT 1m chart on first launch). With arguments you get a single chart for
//! that market, and the session's layout is not saved.
//!
//! The window is only charts, tiled like Hyprland's dwindle layout; see [`keymap`] for keys.
//! Left-click a chart for its [`menu`]: indicators, a strategy to backtest, symbol and
//! timeframe. Custom indicators and strategies are WebAssembly [`plugins`] dropped into the
//! plugins folder; they load, and reload when the files change, without a restart.

pub mod keymap;
pub mod menu;
mod motion;
pub mod persist;
pub mod plugins;
mod studies;

use std::{
    collections::{BTreeMap, HashMap},
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
};
use persephone_data::binance::BinanceClient;
use persephone_engine::{BacktestReport, BacktestRequest, FeeRates};
use persephone_plugin_host::{Host, Plugins};
use persephone_tiling::{Layout, Rect, TileId};

use crate::{
    keymap::Action,
    menu::{MenuMessage, Page, Popup, TileInfo},
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
    symbol: String,
    timeframe: Timeframe,
}

/// One market's bars plus what the engine needs to trade it.
#[derive(Clone, Debug)]
pub struct MarketData {
    pub data: Arc<ChartData>,
    pub instrument: InstrumentSpec,
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
}

impl Persephone {
    fn boot() -> (Self, Task<Message>) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let save_path = args.is_empty().then(persist::default_path).flatten();
        let (mut app, loads) = Self::start(&args, save_path);

        match Host::new() {
            Ok(host) => {
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
        };
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
            bars: HashMap::from([(market.clone(), Bars::Ready(MarketData { data, instrument }))]),
            window,
            now: Instant::now(),
            popup: None,
            save_path: None,
            host: None,
            plugin_dir: None,
            plugins: Arc::default(),
            reloading: false,
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
            starting_balance: STARTING_BALANCE,
            fees: FeeRates::BINANCE_SPOT,
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
                return self.set_market(id, Market { symbol, ..market });
            }
            MenuMessage::SubmitSymbol => {
                if let Popup::Menu {
                    page: Page::Symbol { query },
                    ..
                } = &popup
                {
                    let symbol = query.trim().to_uppercase();
                    if !symbol.is_empty() {
                        return self.set_market(id, Market { symbol, ..market });
                    }
                }
                self.popup = Some(popup);
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
                load(market.clone())
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
            let quote = match self.bars.get(&tile.market) {
                Some(Bars::Ready(market)) => market.instrument.quote.as_str(),
                _ => "",
            };
            let info = TileInfo {
                symbol: &tile.market.symbol,
                quote,
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
            let pieces = pieces.push(mono(format!("{ret:+.2}%"), color));
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
    let timeframe = match args.get(1).map(|tf| tf.parse::<Timeframe>()) {
        Some(Ok(tf)) => tf,
        Some(Err(e)) => {
            eprintln!("persephone: {e}; using 1m");
            Timeframe::M1
        }
        None => Timeframe::M1,
    };
    Market { symbol, timeframe }
}

fn load(market: Market) -> Task<Message> {
    let Market { symbol, timeframe } = market.clone();
    Task::perform(fetch_on_thread(symbol, timeframe), move |result| {
        Message::Loaded(market.clone(), result)
    })
}

/// Runs `job` on a plain thread and hands its result back to iced's executor. Used for the
/// blocking Binance client and for backtests, which need a thread of their own anyway.
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

async fn fetch_on_thread(symbol: String, timeframe: Timeframe) -> Result<MarketData, String> {
    on_thread(move || fetch(&symbol, timeframe).map_err(|e| format!("{e:#}"))).await
}

async fn backtest_on_thread(request: BacktestRequest) -> Result<Arc<BacktestReport>, String> {
    on_thread(move || {
        persephone_engine::run_backtest(&request)
            .map(Arc::new)
            .map_err(|e| e.to_string())
    })
    .await
}

fn fetch(symbol: &str, timeframe: Timeframe) -> anyhow::Result<MarketData> {
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
    Ok(MarketData { data, instrument })
}
