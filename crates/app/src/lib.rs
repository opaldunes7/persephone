//! Persephone: a tiled, chart-only backtesting app.
//!
//! Usage: `persephone [SYMBOL] [TIMEFRAME]`. With no arguments the last layout is restored
//! (one Binance BTCUSDT 1m chart on first launch). With arguments you get a single chart for
//! that market, and the session's layout is not saved.
//!
//! The window is only charts, tiled like Hyprland's dwindle layout; see [`keymap`] for keys.
//! Left-click a chart for its [`menu`]: indicators, symbol and timeframe.

pub mod keymap;
pub mod menu;
mod motion;
pub mod persist;
mod studies;

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use iced::{
    Border, Color, Element, Fill, Length, Point, Size, Subscription, Task, Theme, event,
    futures::channel::oneshot,
    keyboard::{
        self,
        key::{Code, Physical},
    },
    widget::{container, mouse_area, operation, pin, stack, text},
    window,
};
use persephone_chart::{ChartData, ChartMessage, ChartState, theme};
use persephone_core::{
    Timeframe,
    indicator::{BuiltIn, IndicatorSpec},
};
use persephone_data::binance::BinanceClient;
use persephone_tiling::{Layout, Rect, TileId};

use crate::{
    keymap::Action,
    menu::{MenuMessage, Page, Popup, TileInfo},
    motion::Motion,
    persist::{Saved, SavedIndicator, SavedTile},
};

/// How many bars each market loads.
const HISTORY_BARS: u64 = 5_000;
/// Space around and between tiles when there is more than one.
const GAP: f32 = 6.0;
const BORDER: f32 = 2.0;
/// How far one Mod+Ctrl press moves a divider, as a share of its split.
const RESIZE_STEP: f32 = 0.05;
/// How long the border flashes when an action is refused (17th chart, last chart).
const FLASH: Duration = Duration::from_millis(450);
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

enum Bars {
    Loading,
    Ready(Arc<ChartData>),
    Failed(String),
}

struct Tile {
    market: Market,
    /// The tile's indicators, in legend order. They survive symbol and timeframe changes.
    indicators: Vec<IndicatorSpec>,
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
    /// The open menu or settings popover, if any.
    popup: Option<Popup>,
    /// Where the layout is saved; `None` for one-off sessions started with arguments.
    save_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Loaded(Market, Result<Arc<ChartData>, String>),
    Chart(TileId, ChartMessage),
    Key(Action),
    /// The mouse entered a tile: focus follows it, as on Hyprland.
    Hover(TileId),
    WindowResized(Size),
    Tick(Instant),
    Menu(MenuMessage),
    /// Closes the open popup.
    Escape,
}

impl Persephone {
    fn boot() -> (Self, Task<Message>) {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let save_path = args.is_empty().then(persist::default_path).flatten();
        Self::start(&args, save_path)
    }

    /// Starts from the layout saved at `save_path` (if any, and valid), else from one chart
    /// for the market in `args`. Returns the tasks that load each market's bars.
    #[doc(hidden)]
    pub fn start(args: &[String], save_path: Option<PathBuf>) -> (Self, Task<Message>) {
        let saved = save_path.as_deref().and_then(persist::load);

        let (layout, tiles) = match saved {
            Some(saved) => {
                let tiles = saved
                    .tiles
                    .into_iter()
                    .map(|t| {
                        let timeframe = t.timeframe.parse().unwrap_or(Timeframe::M1);
                        let indicators = t
                            .indicators
                            .iter()
                            .filter_map(|i| {
                                BuiltIn::from_id(&i.kind)
                                    .map(|kind| IndicatorSpec::with_params(kind, &i.params))
                            })
                            .collect();
                        let market = Market {
                            symbol: t.symbol,
                            timeframe,
                        };
                        (t.id, market, indicators)
                    })
                    .collect();
                (saved.layout, tiles)
            }
            None => (Layout::new(), vec![(0, market_from_args(args), Vec::new())]),
        };

        let mut app = Self {
            layout,
            tiles: BTreeMap::new(),
            bars: HashMap::new(),
            window: DEFAULT_WINDOW,
            now: Instant::now(),
            popup: None,
            save_path,
        };
        let loads: Vec<_> = tiles
            .into_iter()
            .map(|(id, market, indicators)| app.add_tile(id, market, indicators))
            .collect();
        app.relayout(false);
        (app, Task::batch(loads))
    }

    /// A one-chart app over bars already in memory: no network, nothing saved. For tests and
    /// offscreen snapshots.
    #[doc(hidden)]
    pub fn preloaded(data: Arc<ChartData>, window: Size) -> Self {
        let market = Market {
            symbol: data.symbol.clone(),
            timeframe: data.timeframe,
        };
        let mut app = Self {
            layout: Layout::new(),
            tiles: BTreeMap::new(),
            bars: HashMap::from([(market.clone(), Bars::Ready(data))]),
            window,
            now: Instant::now(),
            popup: None,
            save_path: None,
        };
        let _ = app.add_tile(0, market, Vec::new());
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

    /// Tile `id`'s chart, once its bars are in.
    #[must_use]
    pub fn tile_chart(&self, id: TileId) -> Option<&ChartState> {
        self.tiles.get(&id).and_then(|tile| tile.chart.as_ref())
    }

    #[must_use]
    pub fn popup(&self) -> Option<&Popup> {
        self.popup.as_ref()
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
                for id in waiting {
                    self.attach(id);
                }
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
                self.popup = self
                    .tiles
                    .get(&id)
                    .and_then(|tile| tile.indicators.get(study))
                    .map(|spec| Popup::settings(id, study, at, spec));
            }
            Message::Chart(id, message) => {
                if let Some(chart) = self.tiles.get_mut(&id).and_then(|t| t.chart.as_mut()) {
                    chart.update(message);
                }
            }
            Message::Menu(message) => return self.on_menu(message),
            Message::Escape => self.popup = None,
            Message::Key(action) => return self.act(action),
            Message::Hover(id) => {
                self.layout.focus(id);
            }
            Message::WindowResized(size) => {
                self.window = size;
                self.relayout(false);
            }
            Message::Tick(now) => self.now = now,
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
                        task = self.add_tile(id, market, Vec::new());
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
            MenuMessage::AddIndicator(kind) => {
                if let Some(tile) = self.tiles.get_mut(&id) {
                    tile.indicators.push(IndicatorSpec::new(kind));
                }
                self.restudy(id);
                self.save();
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
                    if let Some(input) = inputs.get_mut(index) {
                        input.clone_from(&value);
                    }
                    let spec = self
                        .tiles
                        .get_mut(&id)
                        .and_then(|t| t.indicators.get_mut(*study));
                    let outcome = match (spec, value.trim().parse::<f64>()) {
                        (Some(spec), Ok(number)) => spec.set_param(index, number),
                        (Some(_), Err(_)) => Err("Enter a number".into()),
                        (None, _) => Ok(()),
                    };
                    *error = outcome.err();
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
            MenuMessage::Close => {}
        }
        Task::none()
    }

    /// Adds a tile, loading its market's bars unless they're loaded or on the way.
    fn add_tile(
        &mut self,
        id: TileId,
        market: Market,
        indicators: Vec<IndicatorSpec>,
    ) -> Task<Message> {
        let task = self.request(&market);
        self.tiles.insert(
            id,
            Tile {
                market,
                indicators,
                chart: None,
                motion: Motion::at_rest(Rect::default()),
                flash: None,
            },
        );
        self.attach(id);
        task
    }

    /// Points tile `id` at another market, keeping its indicators.
    fn set_market(&mut self, id: TileId, market: Market) -> Task<Message> {
        let Some(tile) = self.tiles.get_mut(&id) else {
            return Task::none();
        };
        if tile.market == market {
            return Task::none();
        }
        tile.market = market.clone();
        tile.chart = None;
        let task = self.request(&market);
        self.attach(id);
        self.save();
        task
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

    /// Gives tile `id` a chart, with its indicators, if its market's bars are in.
    fn attach(&mut self, id: TileId) {
        let Some(tile) = self.tiles.get_mut(&id) else {
            return;
        };
        if tile.chart.is_some() {
            return;
        }
        if let Some(Bars::Ready(data)) = self.bars.get(&tile.market) {
            let mut chart = ChartState::new(Arc::clone(data));
            chart.set_studies(studies::build(&tile.indicators, &data.candles));
            tile.chart = Some(chart);
        }
    }

    /// Recomputes tile `id`'s indicators after they change.
    fn restudy(&mut self, id: TileId) {
        if let Some(tile) = self.tiles.get_mut(&id)
            && let Some(chart) = tile.chart.as_mut()
        {
            let data = Arc::clone(chart.data());
            chart.set_studies(studies::build(&tile.indicators, &data.candles));
        }
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
                            kind: spec.kind.id().into(),
                            params: spec.params.clone(),
                        })
                        .collect(),
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

        if self.is_animating() {
            Subscription::batch([input, window::frames().map(Message::Tick)])
        } else {
            input
        }
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
            let info = TileInfo {
                symbol: &tile.market.symbol,
                timeframe: tile.market.timeframe,
                specs: &tile.indicators,
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

        let content: Element<'a, Message> = match &tile.chart {
            Some(chart) => chart.view().map(move |message| Message::Chart(id, message)),
            None => placeholder(&tile.market, self.bars.get(&tile.market)),
        };
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

/// Fetches bars on a plain thread (the Binance client is blocking) and hands them back to
/// iced's executor.
async fn fetch_on_thread(symbol: String, timeframe: Timeframe) -> Result<Arc<ChartData>, String> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch(&symbol, timeframe).map_err(|e| format!("{e:#}")));
    });
    rx.await
        .map_err(|_| "the loader thread stopped".to_string())?
}

fn fetch(symbol: &str, timeframe: Timeframe) -> anyhow::Result<Arc<ChartData>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let end = now - now % timeframe.nanos() + timeframe.nanos(); // include the forming bar
    let start = end.saturating_sub(HISTORY_BARS * timeframe.nanos());

    let binance = BinanceClient::new()?;
    let spec = binance.instrument(symbol)?;
    let candles = binance.klines(symbol, timeframe, start, end)?;
    anyhow::ensure!(!candles.is_empty(), "Binance returned no bars for {symbol}");
    Ok(ChartData::new(
        spec.symbol,
        spec.venue,
        timeframe,
        spec.price_precision,
        candles,
    ))
}
