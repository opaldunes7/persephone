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

pub mod theme;
pub mod ticks;
pub mod viewport;

mod gpu;
mod grid;
mod overlay;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use iced::{
    Element, Length, Padding, Size,
    widget::{canvas, container, shader, stack},
};
use persephone_core::{Candle, Timeframe};

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
}

/// One chart: its data, what part of it is on screen, and cached layers.
pub struct ChartState {
    /// Keys this chart's GPU buffers.
    id: u64,
    data: Arc<ChartData>,
    viewport: Viewport,
    grid_cache: canvas::Cache,
}

impl ChartState {
    #[must_use]
    pub fn new(data: Arc<ChartData>) -> Self {
        Self {
            id: next_id(),
            viewport: Viewport::new(data.candles.len()),
            data,
            grid_cache: canvas::Cache::new(),
        }
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

    fn projection(&self, plot: Size) -> Projection {
        Projection::new(&self.viewport, &self.data.candles, plot.width, plot.height)
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
