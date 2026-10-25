//! GPU candlestick chart for iced, styled after TradingView.
//!
//! A chart is three layers stacked in one widget:
//! 1. `grid` (canvas): background and grid lines, cached between viewport changes;
//! 2. `gpu` (shader): candles and volume as instanced quads drawn straight into iced's render
//!    pass, so many charts share one GPU device and pass;
//! 3. `overlay` (canvas): axes, labels, crosshair and legend; it also handles mouse input.
//!
//! Every layer derives the same [`Projection`] from the [`ChartState`] and its bounds, so they
//! line up exactly without sharing anything mutable.

pub mod study;
pub mod theme;
pub mod ticks;
pub mod trades;
pub mod viewport;

mod gpu;
mod grid;
mod overlay;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use iced::{
    Element, Length, Padding, Point, Size,
    widget::{canvas, container, shader, stack},
};
use persephone_core::{Candle, Timeframe};

pub use study::{OutputStyle, Placement, Study, StudyLine};
pub use trades::{FillMark, PositionSpan, TradeMarker, Trades};
pub use viewport::{Projection, Viewport};

/// Width of the price axis on the right, in logical pixels.
pub const PRICE_AXIS_WIDTH: f32 = 76.0;
/// Height of the time axis at the bottom.
pub const TIME_AXIS_HEIGHT: f32 = 26.0;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// The bars one chart shows. Immutable and shared; swap in a new one to change data.
#[derive(Debug)]
pub struct ChartData {
    pub symbol: String,
    pub venue: String,
    pub timeframe: Timeframe,
    /// Decimal places for price labels.
    pub price_precision: u8,
    /// Bars, oldest first.
    pub candles: Vec<Candle>,
    /// Unique per data set, so the GPU re-uploads bars only when they change.
    generation: u64,
}

impl ChartData {
    #[must_use]
    pub fn new(
        symbol: impl Into<String>,
        venue: impl Into<String>,
        timeframe: Timeframe,
        price_precision: u8,
        candles: Vec<Candle>,
    ) -> Arc<Self> {
        Arc::new(Self {
            symbol: symbol.into(),
            venue: venue.into(),
            timeframe,
            price_precision,
            candles,
            generation: next_id(),
        })
    }
}

#[derive(Clone, Debug)]
pub enum ChartMessage {
    /// Drag by `dx` pixels; positive moves the bars right (into the past).
    Pan { dx: f32, plot_width: f32 },
    /// Zoom by `factor` around the point `from_right` pixels left of the plot's right edge.
    Zoom {
        factor: f32,
        from_right: f32,
        plot_width: f32,
    },
    /// Price-axis drag by `dy` pixels.
    ScalePrice { dy: f32 },
    /// Back to fitting the visible bars.
    ResetPriceScale,
    /// A left click (no drag) on the plot at `at`, in chart coordinates. The chart itself
    /// ignores it; the app opens its menu there.
    Clicked { at: Point },
    /// A left click on study `study`'s legend.
    StudyClicked { study: usize, at: Point },
}

/// One chart: its data and indicators, what part of it is on screen, and cached layers.
pub struct ChartState {
    /// Keys this chart's GPU buffers.
    id: u64,
    data: Arc<ChartData>,
    studies: Arc<Vec<Study>>,
    /// Changes whenever `studies` does, so the GPU re-uploads them.
    studies_generation: u64,
    /// A backtest's fills and holding periods, if one ran.
    trades: Option<Trades>,
    viewport: Viewport,
    grid_cache: canvas::Cache,
}

/// The price pane and one pane per indicator that wants its own, for one frame.
pub(crate) struct Panes {
    pub(crate) price: Projection,
    /// (study index, its pane), top to bottom.
    pub(crate) indicators: Vec<(usize, Projection)>,
}

impl Panes {
    /// Every pane, price first.
    pub(crate) fn all(&self) -> impl Iterator<Item = &Projection> {
        std::iter::once(&self.price).chain(self.indicators.iter().map(|(_, p)| p))
    }

    /// The pane under `y` (from the top of the plot), and the study it shows if not prices.
    pub(crate) fn at(&self, y: f32) -> Option<(Option<usize>, &Projection)> {
        if self.price.contains_y(y) {
            return Some((None, &self.price));
        }
        self.indicators
            .iter()
            .find(|(_, p)| p.contains_y(y))
            .map(|(study, p)| (Some(*study), p))
    }
}

impl ChartState {
    #[must_use]
    pub fn new(data: Arc<ChartData>) -> Self {
        Self {
            id: next_id(),
            viewport: Viewport::new(data.candles.len()),
            data,
            studies: Arc::new(Vec::new()),
            studies_generation: next_id(),
            trades: None,
            grid_cache: canvas::Cache::new(),
        }
    }

    #[must_use]
    pub fn studies(&self) -> &[Study] {
        &self.studies
    }

    #[must_use]
    pub fn trades(&self) -> Option<&Trades> {
        self.trades.as_ref()
    }

    /// Shows (or with `None`, clears) a backtest's trade markers and position shading.
    pub fn set_trades(&mut self, trades: Option<Trades>) {
        self.trades = trades;
        self.grid_cache.clear();
    }

    /// Replaces the chart's indicators. Each study's values must line up with the candles.
    pub fn set_studies(&mut self, studies: Vec<Study>) {
        self.studies = Arc::new(studies);
        self.studies_generation = next_id();
        self.grid_cache.clear();
    }

    #[must_use]
    pub fn data(&self) -> &Arc<ChartData> {
        &self.data
    }

    #[must_use]
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    pub fn update(&mut self, message: ChartMessage) {
        let len = self.data.candles.len();
        match message {
            ChartMessage::Pan { dx, plot_width } => self.viewport.pan(dx, plot_width, len),
            ChartMessage::Zoom {
                factor,
                from_right,
                plot_width,
            } => self.viewport.zoom(factor, from_right, plot_width, len),
            ChartMessage::ScalePrice { dy } => self.viewport.scale_price(dy),
            ChartMessage::ResetPriceScale => self.viewport.reset_price(),
            ChartMessage::Clicked { .. } | ChartMessage::StudyClicked { .. } => return,
        }
        self.grid_cache.clear();
    }

    pub fn view(&self) -> Element<'_, ChartMessage> {
        let candles = container(
            shader(gpu::Candles { chart: self })
                .width(Length::Fill)
                .height(Length::Fill),
        )
        .padding(Padding {
            top: 0.0,
            right: PRICE_AXIS_WIDTH,
            bottom: TIME_AXIS_HEIGHT,
            left: 0.0,
        });

        stack![
            canvas(grid::Grid { chart: self })
                .width(Length::Fill)
                .height(Length::Fill),
            candles,
            canvas(overlay::Overlay { chart: self })
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    pub(crate) fn panes(&self, plot: Size) -> Panes {
        let overlays: Vec<&[f64]> = self
            .studies
            .iter()
            .filter(|s| s.placement == Placement::Overlay)
            .flat_map(Study::series)
            .collect();
        let separate: Vec<usize> = (0..self.studies.len())
            .filter(|&i| self.studies[i].placement == Placement::Pane)
            .collect();

        let (price_height, pane_height) = viewport::pane_heights(plot.height, separate.len());
        let price = Projection::price(
            &self.viewport,
            &self.data.candles,
            &overlays,
            plot.width,
            price_height,
        );
        let indicators = separate
            .iter()
            .enumerate()
            .map(|(k, &i)| {
                let study = &self.studies[i];
                let offset = price_height + k as f32 * pane_height;
                (
                    i,
                    price.pane(
                        &study.series(),
                        study.range,
                        &study.levels,
                        offset,
                        pane_height,
                    ),
                )
            })
            .collect();
        Panes { price, indicators }
    }
}

/// The plot area inside a chart of `size`: everything left of the price axis and above the
/// time axis.
fn plot_size(size: Size) -> Size {
    Size::new(
        (size.width - PRICE_AXIS_WIDTH).max(0.0),
        (size.height - TIME_AXIS_HEIGHT).max(0.0),
    )
}
