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

pub mod drawing;
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
#[cfg(test)]
use persephone_core::Timeframe;
use persephone_core::{BarSpec, Candle, UnixNanos};

pub use drawing::{Anchor, Drawing, Tool};
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
    /// How the bars are cut: by time, ticks or volume.
    pub bars: BarSpec,
    /// A bar's usual length, for spacing time labels and placing times beyond the data.
    pub typical_nanos: UnixNanos,
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
        bars: impl Into<BarSpec>,
        price_precision: u8,
        candles: Vec<Candle>,
    ) -> Arc<Self> {
        let bars = bars.into();
        Arc::new(Self {
            symbol: symbol.into(),
            venue: venue.into(),
            typical_nanos: bars.typical_nanos(&candles),
            bars,
            price_precision,
            candles,
            generation: next_id(),
        })
    }

    /// The same market with other bars (say, one more that just closed).
    #[must_use]
    pub fn with_candles(&self, candles: Vec<Candle>) -> Arc<Self> {
        Self::new(
            self.symbol.clone(),
            self.venue.clone(),
            self.bars,
            self.price_precision,
            candles,
        )
    }
}

#[derive(Clone, Debug)]
pub enum ChartMessage {
    /// Drag by `dx` pixels; positive moves the bars right (into the past).
    Pan {
        dx: f32,
        plot_width: f32,
    },
    /// Zoom by `factor` around the point `from_right` pixels left of the plot's right edge.
    Zoom {
        factor: f32,
        from_right: f32,
        plot_width: f32,
    },
    /// Price-axis drag by `dy` pixels.
    ScalePrice {
        dy: f32,
    },
    /// Back to fitting the visible bars.
    ResetPriceScale,
    /// A right click on the plot at `at`, in chart coordinates. The chart itself ignores it;
    /// the app opens its menu there.
    RightClicked {
        at: Point,
    },
    /// A left click (no drag) on the bare plot; the app closes whatever is open.
    Clicked {
        at: Point,
    },
    /// A click on the symbol, or on the timeframe, in the chart's top-left header.
    SymbolClicked {
        at: Point,
    },
    TimeframeClicked {
        at: Point,
    },
    /// A click on one of the header's indicator tags (see [`ChartState::set_tags`]).
    TagClicked {
        tag: usize,
    },
    /// Time-axis drag by `dx` pixels: right widens the bars (fewer on screen), left narrows
    /// them. The newest bar on screen stays put.
    ScaleTime {
        dx: f32,
        plot_width: f32,
    },
    /// Back to the default bar width.
    ResetTimeScale,
    /// A left click on study `study`'s legend.
    StudyClicked {
        study: usize,
        at: Point,
    },
    /// The bar under the cursor changed (its open time), or the cursor left (`None`). The chart
    /// ignores it; the app moves the other charts' crosshairs to the same time.
    Hovered {
        ts: Option<UnixNanos>,
    },
    /// A click while drawing, at this time and price; [`ChartState::place`] takes it.
    Placed(Anchor),
    /// A left click on drawing `index`.
    DrawingClicked {
        index: usize,
        at: Point,
    },
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
    /// A time hovered on another chart, shown as a crosshair line here.
    linked: Option<UnixNanos>,
    drawings: Arc<Vec<Drawing>>,
    /// The tool being used, and its first click if it needs two.
    tool: Option<(Tool, Option<Anchor>)>,
    /// Round tags after the header's name, one per indicator.
    tags: Vec<HeaderTag>,
}

/// A round tag in the chart's header naming an indicator, lit while it's shown. Clicking one
/// publishes [`ChartMessage::TagClicked`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderTag {
    pub label: String,
    pub shown: bool,
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
            linked: None,
            drawings: Arc::new(Vec::new()),
            tool: None,
            tags: Vec::new(),
        }
    }

    #[must_use]
    pub fn drawings(&self) -> &[Drawing] {
        &self.drawings
    }

    pub fn set_drawings(&mut self, drawings: Vec<Drawing>) {
        self.drawings = Arc::new(drawings);
    }

    /// The next clicks draw with `tool` instead of opening the menu.
    pub fn start_drawing(&mut self, tool: Tool) {
        self.tool = Some((tool, None));
    }

    pub fn cancel_drawing(&mut self) {
        self.tool = None;
    }

    #[must_use]
    pub fn drawing_tool(&self) -> Option<Tool> {
        self.tool.map(|(tool, _)| tool)
    }

    /// Takes a click made while drawing; returns the finished drawing once it has all its
    /// clicks (and stops drawing).
    pub fn place(&mut self, anchor: Anchor) -> Option<Drawing> {
        let (tool, first) = self.tool?;
        match tool.complete(first, anchor) {
            Some(drawing) => {
                self.tool = None;
                Some(drawing)
            }
            None => {
                self.tool = Some((tool, Some(anchor)));
                None
            }
        }
    }

    /// Where `ts` falls on the bar axis: bar `i` opens at `i`; in a gap between bars the time
    /// spreads across it, and outside the data it's spaced by the timeframe.
    #[must_use]
    pub fn index_of_time(&self, ts: UnixNanos) -> f64 {
        let candles = &self.data.candles;
        let step = self.data.typical_nanos as f64;
        let Some(first) = candles.first() else {
            return 0.0;
        };
        let i = candles.partition_point(|c| c.ts <= ts);
        if i == 0 {
            return (ts as f64 - first.ts as f64) / step;
        }
        let i = i - 1;
        let span = candles
            .get(i + 1)
            .map_or(step, |next| (next.ts - candles[i].ts) as f64);
        i as f64 + (ts - candles[i].ts) as f64 / span
    }

    /// The time at a point on the bar axis; the inverse of [`index_of_time`](Self::index_of_time).
    #[must_use]
    pub fn time_of_index(&self, index: f64) -> UnixNanos {
        let candles = &self.data.candles;
        let step = self.data.typical_nanos as f64;
        let Some(first) = candles.first() else {
            return 0;
        };
        let last = candles.len() - 1;
        if index < 0.0 {
            return (first.ts as f64 + index * step).max(0.0) as UnixNanos;
        }
        let i = (index.floor() as usize).min(last);
        let span = candles
            .get(i + 1)
            .map_or(step, |next| (next.ts - candles[i].ts) as f64);
        candles[i].ts + ((index - i as f64) * span).round() as UnixNanos
    }

    /// Shows a crosshair at `ts` (hovered on another chart), or none.
    pub fn set_linked_time(&mut self, ts: Option<UnixNanos>) {
        self.linked = ts;
    }

    #[must_use]
    pub fn linked_time(&self) -> Option<UnixNanos> {
        self.linked
    }

    /// The bar whose period contains `ts`, if the data covers it.
    #[must_use]
    pub fn bar_at_time(&self, ts: UnixNanos) -> Option<usize> {
        let candles = &self.data.candles;
        let i = candles.partition_point(|c| c.ts <= ts).checked_sub(1)?;
        let end = match self.data.bars {
            BarSpec::Time(tf) => candles[i].ts + tf.nanos(),
            // Tick and volume bars last until the next one opens.
            BarSpec::Ticks(_) | BarSpec::Volume(_) => candles
                .get(i + 1)
                .map_or(candles[i].ts + self.data.typical_nanos, |next| next.ts),
        };
        (ts < end).then_some(i)
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
    pub fn tags(&self) -> &[HeaderTag] {
        &self.tags
    }

    /// The header's indicator tags, in order.
    pub fn set_tags(&mut self, tags: Vec<HeaderTag>) {
        self.tags = tags;
        self.grid_cache.clear();
    }

    #[must_use]
    pub fn data(&self) -> &Arc<ChartData> {
        &self.data
    }

    /// Swaps in new bars for the same market, keeping the view where it is, or, if it showed
    /// the latest bar, moving along with the bars added after it.
    pub fn set_data(&mut self, data: Arc<ChartData>) {
        let old = self.data.candles.len() as f64;
        if self.viewport.right >= old - 1.0 {
            self.viewport.right += data.candles.len() as f64 - old;
        }
        self.data = data;
        self.grid_cache.clear();
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
            ChartMessage::ScaleTime { dx, plot_width } => {
                self.viewport.zoom((dx * 0.005).exp(), 0.0, plot_width, len);
            }
            ChartMessage::ResetTimeScale => self.viewport.reset_spacing(),
            ChartMessage::RightClicked { .. }
            | ChartMessage::Clicked { .. }
            | ChartMessage::SymbolClicked { .. }
            | ChartMessage::TagClicked { .. }
            | ChartMessage::TimeframeClicked { .. }
            | ChartMessage::StudyClicked { .. }
            | ChartMessage::Hovered { .. }
            | ChartMessage::Placed(_)
            | ChartMessage::DrawingClicked { .. } => return,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_find_the_bar_that_contains_them() {
        let hour = Timeframe::H1.nanos();
        // Hourly bars with a gap: 0h, 1h, then 5h.
        let candles = [0, 1, 5]
            .iter()
            .map(|&h| Candle {
                ts: h * hour,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            })
            .collect();
        let chart = ChartState::new(ChartData::new("X", "Y", Timeframe::H1, 2, candles));
        assert_eq!(chart.bar_at_time(0), Some(0));
        assert_eq!(
            chart.bar_at_time(hour + 59 * 60_000_000_000),
            Some(1),
            "a minute inside 1h"
        );
        assert_eq!(chart.bar_at_time(3 * hour), None, "the gap has no bar");
        assert_eq!(chart.bar_at_time(5 * hour + 1), Some(2));
        assert_eq!(chart.bar_at_time(6 * hour), None, "past the last bar");
    }

    #[test]
    fn times_and_bar_positions_convert_both_ways() {
        let hour = Timeframe::H1.nanos();
        let candles = [10, 11, 15]
            .iter()
            .map(|&h| Candle {
                ts: h * hour,
                open: 1.0,
                high: 1.0,
                low: 1.0,
                close: 1.0,
                volume: 1.0,
            })
            .collect();
        let chart = ChartState::new(ChartData::new("X", "Y", Timeframe::H1, 2, candles));
        assert_eq!(chart.index_of_time(11 * hour), 1.0);
        assert_eq!(chart.index_of_time(10 * hour + hour / 2), 0.5);
        assert_eq!(
            chart.index_of_time(13 * hour),
            1.5,
            "a gap spreads across one step"
        );
        assert_eq!(
            chart.index_of_time(8 * hour),
            -2.0,
            "before the data, an hour a step"
        );
        assert_eq!(chart.index_of_time(17 * hour), 4.0, "after it too");
        for ts in [8 * hour, 10 * hour, 13 * hour, 15 * hour + 7, 20 * hour] {
            assert_eq!(chart.time_of_index(chart.index_of_time(ts)), ts);
        }
    }

    #[test]
    fn drawing_takes_its_clicks_then_stops() {
        let candles = vec![Candle {
            ts: 0,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        }];
        let mut chart = ChartState::new(ChartData::new("X", "Y", Timeframe::H1, 2, candles));
        let (a, b) = (Anchor { ts: 0, price: 1.0 }, Anchor { ts: 5, price: 2.0 });
        assert_eq!(chart.place(a), None, "not drawing");
        chart.start_drawing(Tool::TrendLine);
        assert_eq!(chart.place(a), None, "one more click");
        assert_eq!(chart.place(b), Some(Drawing::TrendLine { from: a, to: b }));
        assert_eq!(chart.drawing_tool(), None);
        chart.start_drawing(Tool::Rectangle);
        chart.cancel_drawing();
        assert_eq!(chart.place(a), None);
    }

    #[test]
    fn new_bars_keep_the_view_or_follow_the_latest() {
        let bars = |n: u64| -> Vec<Candle> {
            (0..n)
                .map(|i| Candle {
                    ts: i,
                    open: 1.0,
                    high: 1.0,
                    low: 1.0,
                    close: 1.0,
                    volume: 1.0,
                })
                .collect()
        };
        let data = ChartData::new("X", "Y", Timeframe::H1, 2, bars(100));
        let mut chart = ChartState::new(Arc::clone(&data));
        let right = chart.viewport().right;
        chart.set_data(data.with_candles(bars(101)));
        assert_eq!(
            chart.viewport().right,
            right + 1.0,
            "following the latest bar"
        );
        assert_eq!(chart.data().candles.len(), 101);

        chart.update(ChartMessage::Pan {
            dx: 400.0,
            plot_width: 800.0,
        });
        let right = chart.viewport().right;
        chart.set_data(data.with_candles(bars(102)));
        assert_eq!(chart.viewport().right, right, "looking back in time");
    }
}
