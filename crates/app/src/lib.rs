//! Persephone: a tiled, chart-only backtesting app.
//!
//! Usage: `persephone [SYMBOL] [TIMEFRAME]`. With no arguments the last layout is restored
//! (one Binance BTCUSDT 1m chart on first launch). With arguments you get a single chart for
//! that market, and the session's layout is not saved.
//!
//! The window is only charts, tiled like Hyprland's dwindle layout; see [`keymap`] for keys.

pub mod keymap;
mod motion;
pub mod persist;

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use iced::{
    Border, Color, Element, Fill, Length, Size, Subscription, Task, Theme, event,
    futures::channel::oneshot,
    keyboard,
    widget::{container, mouse_area, pin, stack, text},
    window,
};
use persephone_chart::{ChartData, ChartMessage, ChartState, theme};
use persephone_core::Timeframe;
use persephone_data::binance::BinanceClient;
use persephone_tiling::{Layout, Rect, TileId};

use crate::{
    keymap::Action,
    motion::Motion,
    persist::{Saved, SavedTile},
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

        let (layout, markets) = match saved {
            Some(saved) => {
                let markets = saved
                    .tiles
                    .into_iter()
                    .map(|t| {
                        let timeframe = t.timeframe.parse().unwrap_or(Timeframe::M1);
                        (
                            t.id,
                            Market {
                                symbol: t.symbol,
                                timeframe,
                            },
                        )
                    })
                    .collect();
                (saved.layout, markets)
            }
            None => (Layout::new(), vec![(0, market_from_args(args))]),
        };

        let mut app = Self {
            layout,
            tiles: BTreeMap::new(),
            bars: HashMap::new(),
            window: DEFAULT_WINDOW,
            now: Instant::now(),
            save_path,
        };
        let loads: Vec<_> = markets
            .into_iter()
            .map(|(id, market)| app.add_tile(id, market))
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
            save_path: None,
        };
        let _ = app.add_tile(0, market);
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
                    Ok(data) => {
                        for tile in self.tiles.values_mut().filter(|t| t.market == market) {
                            tile.chart
                                .get_or_insert_with(|| ChartState::new(Arc::clone(&data)));
                        }
                        Bars::Ready(data)
                    }
                    Err(error) => Bars::Failed(error),
                };
                self.bars.insert(market, bars);
            }
            Message::Chart(id, message) => {
                if let Some(chart) = self.tiles.get_mut(&id).and_then(|t| t.chart.as_mut()) {
                    chart.update(message);
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
        }
        Task::none()
    }

    fn act(&mut self, action: Action) -> Task<Message> {
        let area = self.tiling_area();
        let mut task = Task::none();
        let mut popped_in = None;

        let structural = match action {
            Action::NewTile => {
                let market = self.focused_market();
                match self.layout.insert(area) {
                    Ok(id) => {
                        task = self.add_tile(id, market);
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

    /// Adds a tile for `market`, loading its bars unless they're loaded or on the way.
    fn add_tile(&mut self, id: TileId, market: Market) -> Task<Message> {
        let (chart, task) = match self.bars.get(&market) {
            Some(Bars::Ready(data)) => (Some(ChartState::new(Arc::clone(data))), Task::none()),
            Some(Bars::Loading) => (None, Task::none()),
            Some(Bars::Failed(_)) | None => {
                self.bars.insert(market.clone(), Bars::Loading);
                (None, load(market.clone()))
            }
        };
        self.tiles.insert(
            id,
            Tile {
                market,
                chart,
                motion: Motion::at_rest(Rect::default()),
                flash: None,
            },
        );
        task
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

        let tiles = self
            .tiles
            .iter()
            .filter(|(id, _)| !fullscreen || **id == focused)
            .map(|(&id, tile)| self.tile_view(id, tile, framed, id == focused));

        container(stack(tiles).width(Fill).height(Fill))
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
