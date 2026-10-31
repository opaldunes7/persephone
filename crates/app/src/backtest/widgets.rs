//! The panel's drawn controls: the equity curve and the tests' fan of lines, the span slider and
//! the speed slider, plus the handle that resizes the panel.

use std::cell::Cell;

use iced::{
    Color, Point, Radians, Rectangle, Renderer, Size, Theme, alignment,
    mouse::{self, ScrollDelta},
    widget::{
        canvas::{self, Event, Frame, LineDash, Path, Stroke, Text},
        text,
    },
};
use persephone_chart::{PRICE_AXIS_WIDTH, Projection, theme, ticks};
use persephone_core::{BarSpec, Candle, UnixNanos, time::DateTime};

use super::{
    History, PanelMessage, RunColumn, SPEED_LABELS,
    optimize::{OptimizeResult, Outcome},
    position_of,
    robustness::{LineStats, RunRow},
    speed_at,
};

const HANDLE: Color = Color::from_rgb8(0xd1, 0xd4, 0xdc);

// --- Equity ----------------------------------------------------------------------------------

/// Room along the bottom of the equity pane for its time labels.
const TIME_STRIP: f32 = 20.0;
/// Room kept above the curve for the legend's two lines.
const LEGEND_ROOM: f32 = 44.0;
/// The equity line, the same color as a chart's equity pane.
const EQUITY: Color = Color::from_rgb8(0x42, 0xa5, 0xf5);

/// Equity over the span, drawn like a chart pane as far as the playhead: grid, price axis and
/// time labels, a dashed line at the starting capital, and falls from the running peak shaded.
pub struct Equity<'a> {
    /// One value per bar of the span.
    pub values: &'a [f64],
    /// The span's bars, for the time labels.
    pub candles: &'a [Candle],
    pub bars: BarSpec,
    /// A bar's usual length.
    pub bar_nanos: UnixNanos,
    /// How many have happened; all of them without a playback.
    pub shown: usize,
    pub start: f64,
}

/// The grid stays drawn between frames, as finding its labels means going through every bar.
#[derive(Default)]
pub struct EquityGrid {
    cache: canvas::Cache,
    drawn_for: Cell<(usize, u32, u32, u64)>,
}

impl Equity<'_> {
    /// The pane's projection: the scale covers the whole run, so it doesn't jump as the
    /// playhead moves.
    fn projection(&self, size: Size) -> Projection {
        let count = self.values.len().min(self.candles.len());
        pane_for(
            self.values[..count].iter().copied(),
            self.start,
            count,
            size,
        )
    }

    fn draw_grid(&self, frame: &mut Frame, pane: &Projection) {
        price_grid(frame, pane);
        let count = pane.visible.end;
        for tick in ticks::time_ticks(&self.candles[..count], pane, self.bar_nanos) {
            let x = pane.x(tick.index as f64).round();
            let color = if tick.major {
                theme::GRID_MAJOR
            } else {
                theme::GRID
            };
            frame.fill_rectangle(Point::new(x, 0.0), Size::new(1.0, pane.height), color);
            let y = pane.height + TIME_STRIP / 2.0;
            grid_label(frame, tick.label, x, y, text::Alignment::Center);
        }
        axes_and_start(frame, pane, self.start);
    }
}

/// A pane's projection for `values` over `count` points: the scale covers them and `start`, with
/// room above for the legend.
fn pane_for(values: impl Iterator<Item = f64>, start: f64, count: usize, size: Size) -> Projection {
    let (lo, hi) = values
        .chain(std::iter::once(start))
        .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(v), hi.max(v)));
    let pad = ((hi - lo) * 0.08).max(hi.abs() * 1e-6).max(1e-9);
    let width = (size.width - PRICE_AXIS_WIDTH).max(1.0);
    let height = (size.height - TIME_STRIP).max(1.0);
    // Room above the curve, so the legend stays readable.
    let room = LEGEND_ROOM.min(height * 0.4);
    let headroom = (hi - lo + 2.0 * pad) * f64::from(room / (height - room));
    Projection {
        left: 0.0,
        spacing: f64::from(width) / (count.max(2) - 1) as f64,
        top: hi + pad + headroom,
        bottom: lo - pad,
        width,
        height,
        offset: 0.0,
        visible: 0..count,
        max_volume: 0.0,
    }
}

fn grid_label(frame: &mut Frame, content: String, x: f32, y: f32, align: text::Alignment) {
    frame.fill_text(Text {
        content,
        position: Point::new(x, y),
        color: theme::TEXT,
        size: theme::TEXT_SIZE.into(),
        align_x: align,
        align_y: alignment::Vertical::Center,
        ..Text::default()
    });
}

/// The pane's background, and its price lines with their labels on the axis.
fn price_grid(frame: &mut Frame, pane: &Projection) {
    frame.fill_rectangle(Point::ORIGIN, frame.size(), theme::BACKGROUND);
    let ticks = ticks::price_ticks(pane);
    let decimals = match ticks.as_slice() {
        [a, b, ..] => ticks::step_decimals(b - a),
        _ => 2,
    };
    for value in ticks {
        let y = pane.y(value).round();
        if pane.contains_y(y) {
            frame.fill_rectangle(Point::new(0.0, y), Size::new(pane.width, 1.0), theme::GRID);
            let text = format!("{value:.decimals$}");
            grid_label(frame, text, pane.width + 8.0, y, text::Alignment::Left);
        }
    }
}

/// The axes, as on a chart, and a dashed line at the starting capital.
fn axes_and_start(frame: &mut Frame, pane: &Projection, start: f64) {
    let size = frame.size();
    frame.fill_rectangle(
        Point::new(pane.width, 0.0),
        Size::new(1.0, size.height),
        theme::AXIS_LINE,
    );
    frame.fill_rectangle(
        Point::new(0.0, pane.height),
        Size::new(size.width, 1.0),
        theme::AXIS_LINE,
    );
    let base = pane.y(start).round() + 0.5;
    frame.stroke(
        &Path::line(Point::new(0.0, base), Point::new(pane.width, base)),
        dashed(theme::LEVEL),
    );
}

fn dashed(color: Color) -> Stroke<'static> {
    Stroke {
        line_dash: LineDash {
            segments: &[4.0, 4.0],
            offset: 0,
        },
        ..Stroke::default().with_color(color).with_width(1.0)
    }
}

/// A value's tag on the price axis, as on a chart.
fn axis_tag(frame: &mut Frame, pane: &Projection, value: f64, background: Color) {
    let y = pane.y(value);
    if !pane.contains_y(y) {
        return;
    }
    frame.fill_rectangle(
        Point::new(pane.width + 1.0, y - 9.0),
        Size::new(PRICE_AXIS_WIDTH - 1.0, 18.0),
        background,
    );
    frame.fill_text(Text {
        content: format!("{value:.2}"),
        position: Point::new(pane.width + 8.0, y),
        color: theme::LABEL_TEXT,
        size: theme::TEXT_SIZE.into(),
        align_y: alignment::Vertical::Center,
        ..Text::default()
    });
}

impl canvas::Program<PanelMessage> for Equity<'_> {
    type State = EquityGrid;

    fn update(
        &self,
        _state: &mut EquityGrid,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<PanelMessage>> {
        matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. }))
            .then(|| cursor.position_in(bounds))
            .flatten()
            .map(|_| canvas::Action::request_redraw())
    }

    fn draw(
        &self,
        state: &EquityGrid,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let size = bounds.size();
        let pane = self.projection(size);
        let key = (
            pane.visible.end,
            size.width.to_bits(),
            size.height.to_bits(),
            pane.top.to_bits() ^ pane.bottom.to_bits().rotate_left(1),
        );
        if state.drawn_for.replace(key) != key {
            state.cache.clear();
        }
        let grid = state
            .cache
            .draw(renderer, size, |frame| self.draw_grid(frame, &pane));

        let mut frame = Frame::new(renderer, size);
        let count = pane.visible.end;
        let shown = &self.values[..self.shown.min(count)];
        let Some(&last) = shown.last() else {
            return vec![grid, frame.into_geometry()];
        };
        let point = |i: usize, v: f64| Point::new(pane.x(i as f64), pane.y(v));
        // No need for more than a couple of points a pixel.
        let stride = (shown.len() / (2 * pane.width as usize).max(1)).max(1);
        let path: Vec<(usize, f64)> = shown
            .iter()
            .copied()
            .enumerate()
            .step_by(stride)
            .chain(std::iter::once((shown.len() - 1, last)))
            .collect();

        // Drawdown: from the running peak down to the curve.
        let mut peak = f64::MIN;
        let peaks: Vec<f64> = path
            .iter()
            .map(|&(_, v)| {
                peak = peak.max(v);
                peak
            })
            .collect();
        let shade = Path::new(|b| {
            b.move_to(point(path[0].0, peaks[0]));
            for (&(i, _), &p) in path.iter().zip(&peaks) {
                b.line_to(point(i, p));
            }
            for &(i, v) in path.iter().rev() {
                b.line_to(point(i, v));
            }
            b.close();
        });
        frame.fill(
            &shade,
            Color {
                a: 0.1,
                ..theme::DOWN
            },
        );
        let line = Path::new(|b| {
            b.move_to(point(path[0].0, path[0].1));
            for &(i, v) in &path[1..] {
                b.line_to(point(i, v));
            }
        });
        frame.stroke(&line, Stroke::default().with_color(EQUITY).with_width(1.5));
        axis_tag(&mut frame, &pane, last, EQUITY);

        let hovered = cursor
            .position_in(bounds)
            .filter(|p| p.x < pane.width && p.y < pane.height)
            .and_then(|p| pane.bar_at(p.x, shown.len()));
        if let Some(i) = hovered {
            let x = pane.x(i as f64).round();
            frame.stroke(
                &Path::line(Point::new(x, 0.0), Point::new(x, pane.height)),
                dashed(theme::CROSSHAIR),
            );
            axis_tag(&mut frame, &pane, shown[i], theme::LABEL_BACKGROUND);
            let label = ticks::crosshair_label(self.candles[i].ts, self.bars);
            let half = label.chars().count() as f32 * 3.4 + 7.0;
            let center = x.clamp(half, (pane.width - half).max(half));
            frame.fill_rectangle(
                Point::new(center - half, pane.height + 1.0),
                Size::new(half * 2.0, TIME_STRIP - 2.0),
                theme::LABEL_BACKGROUND,
            );
            frame.fill_text(Text {
                content: label,
                position: Point::new(center, pane.height + TIME_STRIP / 2.0),
                color: theme::LABEL_TEXT,
                size: theme::TEXT_SIZE.into(),
                align_x: text::Alignment::Center,
                align_y: alignment::Vertical::Center,
                ..Text::default()
            });
        }
        vec![grid, frame.into_geometry()]
    }
}

// --- The tests' fan of equity lines ----------------------------------------------------------

/// A picked line, in a color nothing else on the fan uses.
const PICKED: Color = Color::from_rgb8(0xff, 0xd5, 0x4f);
/// How close the mouse must be to a line to hover or pick it, in pixels.
const LINE_REACH: f32 = 8.0;
/// A drag shorter than this is a click.
const DRAG_FROM: f32 = 3.0;

/// Many equity lines from the same capital, faint over each other with their median bright on
/// top: how Monte Carlo's runs and Random blocks' blocks spread out. Any line can be hovered and
/// picked; the wheel zooms and dragging moves along, as on a chart.
pub struct Fan<'a> {
    pub paths: &'a [Vec<f64>],
    pub median: &'a [f64],
    /// The backtest's own line, counted as the line after `paths`.
    pub actual: Option<&'a [f64]>,
    /// How each line went, the backtest's last.
    pub stats: Vec<LineStats>,
    /// Lines to pick out without being asked, with their colors.
    pub marks: Vec<(usize, Color)>,
    pub picked: Option<usize>,
    /// How much of it shows, from 0 to 1.
    pub zoom: (f32, f32),
    pub start: f64,
    /// What's between a line's points, how many of them, and what a line is.
    pub unit: &'static str,
    pub stride: usize,
    pub line_name: &'static str,
}

/// The lines stay drawn between frames, as there can be many.
#[derive(Default)]
pub struct FanLines {
    /// Apart, as a clipped drawing would land under the grid's background in the same frame.
    grid: canvas::Cache,
    cache: canvas::Cache,
    drawn_for: Cell<(u32, u32, usize, u32, u32, usize)>,
    /// Where a press began and what was showing, until it's let go.
    press: Option<(Point, (f32, f32))>,
    dragged: bool,
}

impl Fan<'_> {
    fn line(&self, line: usize) -> Option<&[f64]> {
        match self.paths.get(line) {
            Some(path) => Some(path),
            None if line == self.paths.len() => self.actual,
            None => None,
        }
    }

    fn lines(&self) -> impl Iterator<Item = (usize, &[f64])> {
        let count = self.paths.len() + usize::from(self.actual.is_some());
        (0..count).filter_map(|line| Some((line, self.line(line)?)))
    }

    /// The most points any line has.
    fn points(&self) -> usize {
        self.lines().map(|(_, line)| line.len()).max().unwrap_or(0)
    }

    /// The points showing, as a range of whole indices.
    fn showing(&self) -> (usize, usize) {
        let last = self.points().saturating_sub(1) as f32;
        let from = (self.zoom.0 * last).floor() as usize;
        let to = (self.zoom.1 * last).ceil() as usize;
        (from, to.max(from + 1))
    }

    /// The pane: its scale fits what's showing, and its bars run from `showing`'s start.
    fn projection(&self, size: Size) -> Projection {
        let (from, to) = self.showing();
        let values = self
            .lines()
            .flat_map(|(_, line)| line.get(from..=to.min(line.len().saturating_sub(1))))
            .flatten()
            .copied();
        let mut pane = pane_for(values, self.start, to - from + 1, size);
        pane.left = from as f64;
        pane
    }

    fn stroke_line(frame: &mut Frame, pane: &Projection, values: &[f64], stroke: Stroke<'_>) {
        let from = (pane.left as usize).saturating_sub(1);
        let to = (pane.left as usize + pane.visible.end + 1).min(values.len());
        if from >= to {
            return;
        }
        let path = Path::new(|b| {
            for (i, &value) in values[from..to].iter().enumerate() {
                let point = Point::new(pane.x((from + i) as f64), pane.y(value));
                if i == 0 {
                    b.move_to(point);
                } else {
                    b.line_to(point);
                }
            }
        });
        frame.stroke(&path, stroke);
    }

    fn draw_grid(&self, frame: &mut Frame, pane: &Projection) {
        price_grid(frame, pane);
        let (from, to) = self.showing();
        let every = nice_step(((to - from) * self.stride / 6).max(1));
        let mut at = (from * self.stride).div_ceil(every) * every;
        while at <= to * self.stride {
            let x = pane.x((at / self.stride) as f64).round();
            frame.fill_rectangle(Point::new(x, 0.0), Size::new(1.0, pane.height), theme::GRID);
            let y = pane.height + TIME_STRIP / 2.0;
            grid_label(frame, at.to_string(), x, y, text::Alignment::Center);
            at += every;
        }
        axes_and_start(frame, pane, self.start);
    }

    fn draw_lines(&self, frame: &mut Frame, pane: &Projection) {
        let clip = Rectangle::new(Point::ORIGIN, Size::new(pane.width, pane.height));
        frame.with_clip(clip, |frame| {
            // Fewer lines are each stronger, so that a few still show.
            let alpha = (8.0 / self.paths.len().max(1) as f32).clamp(0.14, 0.6);
            let faint = Stroke::default()
                .with_color(Color { a: alpha, ..EQUITY })
                .with_width(1.0);
            for path in self.paths {
                Self::stroke_line(frame, pane, path, faint);
            }
            Self::stroke_line(
                frame,
                pane,
                self.median,
                Stroke::default().with_color(HANDLE).with_width(1.5),
            );
            if let Some(actual) = self.actual {
                Self::stroke_line(
                    frame,
                    pane,
                    actual,
                    Stroke::default().with_color(EQUITY).with_width(1.5),
                );
            }
            for &(line, color) in &self.marks {
                if let Some(values) = self.line(line) {
                    Self::stroke_line(
                        frame,
                        pane,
                        values,
                        Stroke::default().with_color(color).with_width(1.5),
                    );
                }
            }
        });
    }

    /// The line nearest the mouse, if one is close enough.
    fn line_at(&self, point: Point, pane: &Projection) -> Option<usize> {
        if point.x >= pane.width || point.y >= pane.height {
            return None;
        }
        let index = pane.index_at(point.x).round().max(0.0) as usize;
        self.lines()
            .filter_map(|(line, values)| {
                let value = values.get(index)?;
                Some((line, (pane.y(*value) - point.y).abs()))
            })
            .filter(|(_, away)| *away <= LINE_REACH)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(line, _)| line)
    }

    /// Zooms by `factor` around the fraction `around` of the way across what's showing.
    fn zoomed(&self, factor: f32, around: f32) -> Option<(f32, f32)> {
        let (from, to) = self.zoom;
        let least = (4.0 / self.points().max(2) as f32).min(1.0);
        let width = ((to - from) * factor).clamp(least, 1.0);
        if width >= 1.0 {
            return None;
        }
        let start = (from + (to - from) * around - width * around).clamp(0.0, 1.0 - width);
        Some((start, start + width))
    }
}

/// What's under the mouse, in a box just above it: among the lines, as text on an axis would
/// show through a box drawn over it.
fn hover_label(frame: &mut Frame, content: String, x: f32, y: f32, plot_width: f32) {
    let half = content.chars().count() as f32 * 3.6 + 7.0;
    let center = x.clamp(half, (plot_width - half).max(half));
    let top = (y - 28.0).max(LEGEND_ROOM + 2.0);
    frame.fill_rectangle(
        Point::new(center - half, top),
        Size::new(half * 2.0, TIME_STRIP - 2.0),
        theme::LABEL_BACKGROUND,
    );
    frame.fill_text(Text {
        content,
        position: Point::new(center, top + (TIME_STRIP - 2.0) / 2.0),
        color: theme::LABEL_TEXT,
        size: theme::TEXT_SIZE.into(),
        align_x: text::Alignment::Center,
        align_y: alignment::Vertical::Center,
        ..Text::default()
    });
}

/// A round number near `rough`: 1, 2 or 5 times a power of ten.
fn nice_step(rough: usize) -> usize {
    let power = 10_usize.pow(rough.max(1).ilog10());
    let lead = rough / power;
    power
        * match lead {
            0 | 1 => 1,
            2..=4 => 2,
            5..=9 => 5,
            _ => 10,
        }
}

impl canvas::Program<PanelMessage> for Fan<'_> {
    type State = FanLines;

    fn update(
        &self,
        state: &mut FanLines,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<PanelMessage>> {
        let Event::Mouse(event) = event else {
            return None;
        };
        let pane = self.projection(bounds.size());
        match event {
            mouse::Event::WheelScrolled { delta } => {
                let point = cursor.position_in(bounds)?;
                let lines = match delta {
                    ScrollDelta::Lines { y, .. } => *y,
                    ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                let around = (point.x / pane.width.max(1.0)).clamp(0.0, 1.0);
                let zoom = self.zoomed(0.85_f32.powf(lines), around);
                Some(canvas::Action::publish(PanelMessage::ZoomFan(zoom)).and_capture())
            }
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                let point = cursor.position_in(bounds)?;
                state.press = Some((point, self.zoom));
                state.dragged = false;
                Some(canvas::Action::request_redraw().and_capture())
            }
            mouse::Event::CursorMoved { .. } => {
                let Some((began, from)) = state.press else {
                    return cursor
                        .position_in(bounds)
                        .map(|_| canvas::Action::request_redraw());
                };
                let point = cursor.position_from(bounds.position())?;
                let moved = point.x - began.x;
                if moved.abs() < DRAG_FROM && !state.dragged {
                    return None;
                }
                state.dragged = true;
                let width = from.1 - from.0;
                if width >= 1.0 {
                    return None;
                }
                let start = (from.0 - moved / pane.width.max(1.0) * width).clamp(0.0, 1.0 - width);
                Some(canvas::Action::publish(PanelMessage::ZoomFan(Some((
                    start,
                    start + width,
                )))))
            }
            mouse::Event::ButtonReleased(mouse::Button::Left) => {
                let (began, _) = state.press.take()?;
                if state.dragged {
                    return Some(canvas::Action::request_redraw());
                }
                let line = self.line_at(began, &pane);
                Some(canvas::Action::publish(PanelMessage::SelectLine(line)))
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        state: &FanLines,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.dragged && state.press.is_some() {
            return mouse::Interaction::Grabbing;
        }
        let over = cursor
            .position_in(bounds)
            .and_then(|point| self.line_at(point, &self.projection(bounds.size())));
        if over.is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        state: &FanLines,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let size = bounds.size();
        let pane = self.projection(size);
        let key = (
            size.width.to_bits(),
            size.height.to_bits(),
            self.paths.as_ptr() as usize,
            self.zoom.0.to_bits(),
            self.zoom.1.to_bits(),
            self.marks.len(),
        );
        if state.drawn_for.replace(key) != key {
            state.grid.clear();
            state.cache.clear();
        }
        let grid = state
            .grid
            .draw(renderer, size, |frame| self.draw_grid(frame, &pane));
        let lines = state
            .cache
            .draw(renderer, size, |frame| self.draw_lines(frame, &pane));

        let mut frame = Frame::new(renderer, size);
        let clip = Rectangle::new(Point::ORIGIN, Size::new(pane.width, pane.height));
        let point = cursor.position_in(bounds);
        let hovered = point.and_then(|point| self.line_at(point, &pane));
        for (line, color, width) in [(hovered, HANDLE, 1.5), (self.picked, PICKED, 2.5)] {
            if let Some(values) = line.and_then(|line| self.line(line)) {
                frame.with_clip(clip, |frame| {
                    Self::stroke_line(
                        frame,
                        &pane,
                        values,
                        Stroke::default().with_color(color).with_width(width),
                    );
                });
            }
        }
        let tagged = self
            .picked
            .and_then(|line| self.line(line))
            .unwrap_or(self.median);
        if let Some(&last) = tagged.last() {
            let color = if self.picked.is_some() {
                PICKED
            } else {
                EQUITY
            };
            axis_tag(&mut frame, &pane, last, color);
        }
        let index = point
            .filter(|p| p.x < pane.width && p.y < pane.height)
            .map(|p| pane.index_at(p.x).round().max(0.0) as usize)
            .filter(|&i| i < self.points());
        if let Some(i) = index {
            let x = pane.x(i as f64).round();
            frame.stroke(
                &Path::line(Point::new(x, 0.0), Point::new(x, pane.height)),
                dashed(theme::CROSSHAIR),
            );
            let mut label = format!("{} {}", self.unit, i * self.stride);
            let shown = hovered.or(self.picked);
            if let Some((line, stats)) = shown.and_then(|line| Some((line, self.stats.get(line)?)))
            {
                let name = if line == self.paths.len() {
                    "The backtest"
                } else {
                    self.line_name
                };
                label = format!(
                    "{label}  ·  {name}: {:+.2}%, fell {:.2}% at worst",
                    stats.return_ * 100.0,
                    stats.drawdown * 100.0
                );
            }
            if let Some(value) = hovered
                .or(self.picked)
                .and_then(|line| self.line(line)?.get(i))
                .or_else(|| self.median.get(i))
            {
                axis_tag(&mut frame, &pane, *value, theme::LABEL_BACKGROUND);
            }
            let y = point.map_or(0.0, |p| p.y);
            hover_label(&mut frame, label, x, y, pane.width);
        }
        vec![grid, lines, frame.into_geometry()]
    }
}

// --- Monte Carlo's table of runs -------------------------------------------------------------

const ROW_HEIGHT: f32 = 20.0;
const RUN_COLUMNS: [&str; 5] = ["Rank", "Run", "Return", "Final equity", "Fell"];
/// Each column's right edge, as a share of the width.
const RUN_EDGES: [f32; 5] = [0.09, 0.42, 0.6, 0.82, 0.98];

/// Every run, drawn rather than built from widgets so that only the rows showing cost anything,
/// however many runs there are.
pub struct RunsTable {
    /// Sorted already, and whether there's an arrow on a column.
    pub rows: Vec<RunRow>,
    /// What to call the second column, and what each row is called there; its number if nothing.
    pub noun: &'static str,
    pub names: Vec<String>,
    pub sort: (RunColumn, bool),
    pub capital: f64,
    pub ruin_at: f64,
    pub scroll: usize,
    /// The run whose trades show beside it.
    pub picked: Option<usize>,
    /// Room above for the legend.
    pub top: f32,
}

impl RunsTable {
    fn visible_rows(&self, height: f32) -> usize {
        (((height - self.top - ROW_HEIGHT) / ROW_HEIGHT).floor() as usize).max(1)
    }

    /// The first row showing: where it's scrolled to, unless a resize left that past the end.
    fn first_row(&self, height: f32) -> usize {
        self.scroll
            .min(self.rows.len().saturating_sub(self.visible_rows(height)))
    }

    fn row_at(&self, point: Point, size: Size) -> Option<&RunRow> {
        let y = point.y - self.top - ROW_HEIGHT;
        if y < 0.0 || point.x > size.width {
            return None;
        }
        let shown = (y / ROW_HEIGHT) as usize;
        if shown >= self.visible_rows(size.height) {
            return None;
        }
        self.rows.get(self.first_row(size.height) + shown)
    }

    fn column_at(&self, x: f32, width: f32) -> usize {
        RUN_EDGES
            .iter()
            .position(|edge| x <= edge * width)
            .unwrap_or(RUN_EDGES.len() - 1)
    }
}

impl canvas::Program<PanelMessage> for RunsTable {
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<PanelMessage>> {
        let Event::Mouse(event) = event else {
            return None;
        };
        let point = cursor.position_in(bounds)?;
        let size = bounds.size();
        match event {
            mouse::Event::CursorMoved { .. } => Some(canvas::Action::request_redraw()),
            mouse::Event::WheelScrolled { delta } => {
                let lines = match delta {
                    ScrollDelta::Lines { y, .. } => *y * 3.0,
                    ScrollDelta::Pixels { y, .. } => *y / ROW_HEIGHT,
                };
                let most = self
                    .rows
                    .len()
                    .saturating_sub(self.visible_rows(size.height));
                let from = self.first_row(size.height) as f32;
                let row = (from - lines).clamp(0.0, most as f32).round();
                Some(canvas::Action::publish(PanelMessage::ScrollRuns(row as usize)).and_capture())
            }
            mouse::Event::ButtonPressed(mouse::Button::Left) => {
                if (self.top..self.top + ROW_HEIGHT).contains(&point.y) {
                    let column = match self.column_at(point.x, size.width) {
                        1 => RunColumn::Run,
                        4 => RunColumn::Fell,
                        _ => RunColumn::Return,
                    };
                    return Some(
                        canvas::Action::publish(PanelMessage::SortRuns(column)).and_capture(),
                    );
                }
                let row = self.row_at(point, size)?;
                Some(canvas::Action::publish(PanelMessage::SelectRun(Some(row.run))).and_capture())
            }
            _ => None,
        }
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let over = cursor.position_in(bounds).is_some_and(|point| {
            (self.top..self.top + ROW_HEIGHT).contains(&point.y)
                || self.row_at(point, bounds.size()).is_some()
        });
        if over {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let size = bounds.size();
        let mut frame = Frame::new(renderer, size);
        let width = size.width - 12.0;
        let cell = |frame: &mut Frame, content: String, column: usize, y: f32, color: Color| {
            frame.fill_text(Text {
                content,
                position: Point::new(RUN_EDGES[column] * width, y + ROW_HEIGHT / 2.0),
                color,
                size: 11.0.into(),
                font: iced::Font::MONOSPACE,
                align_x: text::Alignment::Right,
                align_y: alignment::Vertical::Center,
                ..Text::default()
            });
        };
        let (sorted, descending) = self.sort;
        let arrow = if descending { " ↓" } else { " ↑" };
        for (column, name) in RUN_COLUMNS.iter().enumerate() {
            let this = match column {
                1 => Some(RunColumn::Run),
                4 => Some(RunColumn::Fell),
                2 => Some(RunColumn::Return),
                _ => None,
            };
            let name = if column == 1 { self.noun } else { *name };
            let content = if this == Some(sorted) {
                format!("{name}{arrow}")
            } else {
                name.to_owned()
            };
            cell(&mut frame, content, column, self.top, theme::TEXT_MUTED);
        }
        frame.fill_rectangle(
            Point::new(0.0, self.top + ROW_HEIGHT - 1.0),
            Size::new(size.width, 1.0),
            theme::AXIS_LINE,
        );
        let visible = self.visible_rows(size.height);
        let hovered = cursor
            .position_in(bounds)
            .and_then(|point| self.row_at(point, size))
            .map(|row| row.run);
        let first = self.first_row(size.height);
        for (shown, row) in self.rows.iter().skip(first).take(visible).enumerate() {
            let y = self.top + ROW_HEIGHT * (shown + 1) as f32;
            if self.picked == Some(row.run) {
                frame.fill_rectangle(
                    Point::new(0.0, y),
                    Size::new(size.width, ROW_HEIGHT),
                    Color {
                        a: 0.3,
                        ..theme::ACCENT
                    },
                );
            } else if hovered == Some(row.run) {
                frame.fill_rectangle(
                    Point::new(0.0, y),
                    Size::new(size.width, ROW_HEIGHT),
                    theme::GRID,
                );
            }
            let change = if row.stats.return_ >= 0.0 {
                theme::UP
            } else {
                theme::DOWN
            };
            let ruined = row.stats.drawdown >= self.ruin_at;
            let plain = if row.backtest { EQUITY } else { theme::TEXT };
            cell(&mut frame, row.rank.to_string(), 0, y, plain);
            let name = if row.backtest {
                "The backtest".to_owned()
            } else {
                self.names
                    .get(row.run)
                    .cloned()
                    .unwrap_or_else(|| (row.run + 1).to_string())
            };
            cell(&mut frame, name, 1, y, plain);
            cell(&mut frame, percent(row.stats.return_), 2, y, change);
            let equity = self.capital * (1.0 + row.stats.return_);
            cell(&mut frame, format!("{equity:.2}"), 3, y, plain);
            cell(
                &mut frame,
                format!("{:.2}%", row.stats.drawdown * 100.0),
                4,
                y,
                if ruined { theme::DOWN } else { plain },
            );
        }
        // Where it's scrolled to, as there are usually far more rows than fit.
        if self.rows.len() > visible {
            let track = visible as f32 * ROW_HEIGHT;
            let thumb = (track * visible as f32 / self.rows.len() as f32).max(12.0);
            let most = (self.rows.len() - visible).max(1) as f32;
            let y = self.top + ROW_HEIGHT + (track - thumb) * first as f32 / most;
            frame.fill_rectangle(
                Point::new(size.width - 5.0, y),
                Size::new(3.0, thumb),
                theme::TEXT_MUTED,
            );
        }
        vec![frame.into_geometry()]
    }
}

// --- What an optimization found --------------------------------------------------------------

/// A combination without a score: it made no trades, or failed.
const NO_SCORE: Color = Color::from_rgb8(0x3a, 0x3e, 0x48);
/// A score of zero on the heatmap.
const HEAT_ZERO: Color = Color::from_rgb8(0x1e, 0x22, 0x2d);

/// The score at each value: bars for one variable, a heatmap for two. Clicking a combination
/// applies it.
pub struct Landscape<'a> {
    pub found: &'a OptimizeResult,
    /// The combination the strategy is on, if it's one of these.
    pub current: Option<usize>,
}

/// Where the heatmap's cells go: the first variable's values across, the second's up.
struct HeatArea {
    x: f32,
    y: f32,
    cell_width: f32,
    cell_height: f32,
    rows: usize,
}

impl HeatArea {
    fn top_left(&self, column: usize, row: usize) -> Point {
        Point::new(
            self.x + column as f32 * self.cell_width,
            self.y + (self.rows - 1 - row) as f32 * self.cell_height,
        )
    }
}

fn mix(a: Color, b: Color, share: f32) -> Color {
    let share = share.clamp(0.0, 1.0);
    Color::from_rgb(
        a.r + (b.r - a.r) * share,
        a.g + (b.g - a.g) * share,
        a.b + (b.b - a.b) * share,
    )
}

/// `score`'s color on a scale reaching `reach` either side of zero.
pub(super) fn heat(score: f64, reach: f64) -> Color {
    let share = (score.abs() / reach.max(1e-12)) as f32;
    mix(
        HEAT_ZERO,
        if score >= 0.0 { theme::UP } else { theme::DOWN },
        share,
    )
}

pub(super) fn percent(score: f64) -> String {
    format!("{:+.2}%", score * 100.0)
}

impl Landscape<'_> {
    fn scores(&self) -> impl Iterator<Item = f64> + '_ {
        self.found.outcomes.iter().filter_map(Outcome::score)
    }

    fn is_heatmap(&self) -> bool {
        self.found.grid.second.is_some()
    }

    fn bars_pane(&self, size: Size) -> Projection {
        let values = self.scores().map(|score| score * 100.0);
        pane_for(values, 0.0, self.found.outcomes.len(), size)
    }

    fn heat_area(&self, size: Size) -> HeatArea {
        let grid = &self.found.grid;
        let room = LEGEND_ROOM.min(size.height * 0.3);
        HeatArea {
            x: 0.0,
            y: room,
            cell_width: (size.width - PRICE_AXIS_WIDTH).max(1.0) / grid.width() as f32,
            cell_height: (size.height - room - TIME_STRIP).max(1.0) / grid.height() as f32,
            rows: grid.height(),
        }
    }

    /// The combination under the mouse.
    fn cell_at(&self, point: Point, size: Size) -> Option<usize> {
        let width = self.found.grid.width();
        if point.x >= size.width - PRICE_AXIS_WIDTH || point.y >= size.height - TIME_STRIP {
            return None;
        }
        if self.is_heatmap() {
            let area = self.heat_area(size);
            if point.y < area.y {
                return None;
            }
            let column = (point.x / area.cell_width) as usize;
            let down = ((point.y - area.y) / area.cell_height) as usize;
            let row = area.rows.checked_sub(down + 1)?;
            Some(row * width + column.min(width - 1))
        } else {
            let pane = self.bars_pane(size);
            Some(((point.x / (pane.width / width as f32)) as usize).min(width - 1))
        }
    }

    /// What the bottom strip says about a combination.
    fn describe(&self, cell: usize) -> String {
        let grid = &self.found.grid;
        let mut names = vec![format!(
            "{} {}",
            grid.first.name,
            crate::sweep::label(grid.first.values[cell % grid.width()])
        )];
        if let Some(second) = &grid.second {
            names.push(format!(
                "{} {}",
                second.name,
                crate::sweep::label(second.values[cell / grid.width()])
            ));
        }
        let outcome = match &self.found.outcomes[cell] {
            Outcome::Scored { score, .. } => percent(*score),
            Outcome::TooFewTrades => "too few trades".to_owned(),
            Outcome::Failed(error) => format!("failed: {error}"),
        };
        format!("{}: {outcome}", names.join(" · "))
    }

    fn draw_bars(&self, frame: &mut Frame, hovered: Option<usize>) {
        let size = frame.size();
        let pane = self.bars_pane(size);
        price_grid(frame, &pane);
        let count = self.found.outcomes.len();
        let slot = pane.width / count as f32;
        let every = ((44.0 / slot).ceil() as usize).max(1);
        for (i, outcome) in self.found.outcomes.iter().enumerate() {
            let x = slot * i as f32;
            let bar_x = x + slot * 0.15;
            let width = (slot * 0.7).max(1.0);
            match outcome.score() {
                Some(score) => {
                    let (zero, y) = (pane.y(0.0), pane.y(score * 100.0));
                    let color = if score >= 0.0 { theme::UP } else { theme::DOWN };
                    frame.fill_rectangle(
                        Point::new(bar_x, y.min(zero)),
                        Size::new(width, (y - zero).abs().max(1.0)),
                        color,
                    );
                }
                None => frame.fill_rectangle(
                    Point::new(bar_x, 0.0),
                    Size::new(width, pane.height),
                    Color {
                        a: 0.35,
                        ..NO_SCORE
                    },
                ),
            }
            if self.current == Some(i) || hovered == Some(i) {
                let color = if self.current == Some(i) {
                    HANDLE
                } else {
                    theme::CROSSHAIR
                };
                frame.stroke(
                    &Path::rectangle(Point::new(bar_x, 0.5), Size::new(width, pane.height - 1.0)),
                    Stroke::default().with_color(color).with_width(1.0),
                );
            }
            if i % every == 0 {
                grid_label(
                    frame,
                    crate::sweep::label(self.found.grid.first.values[i]),
                    x + slot / 2.0,
                    pane.height + TIME_STRIP / 2.0,
                    text::Alignment::Center,
                );
            }
        }
        axes_and_start(frame, &pane, 0.0);
        if let Some(score) = hovered.and_then(|i| self.found.outcomes[i].score()) {
            axis_tag(frame, &pane, score * 100.0, theme::LABEL_BACKGROUND);
        }
    }

    fn draw_heat(&self, frame: &mut Frame, hovered: Option<usize>) {
        let size = frame.size();
        frame.fill_rectangle(Point::ORIGIN, size, theme::BACKGROUND);
        let grid = &self.found.grid;
        let area = self.heat_area(size);
        let reach = self.scores().fold(0.0, |most: f64, s| most.max(s.abs()));
        let cell_size = Size::new(area.cell_width, area.cell_height);
        for (cell, outcome) in self.found.outcomes.iter().enumerate() {
            let (column, row) = (cell % grid.width(), cell / grid.width());
            let at = area.top_left(column, row);
            let color = outcome.score().map_or(NO_SCORE, |score| heat(score, reach));
            frame.fill_rectangle(at, cell_size, color);
            if let Some(score) = outcome.score()
                && area.cell_width > 52.0
                && area.cell_height > 16.0
            {
                frame.fill_text(Text {
                    content: percent(score),
                    position: Point::new(
                        at.x + area.cell_width / 2.0,
                        at.y + area.cell_height / 2.0,
                    ),
                    color: theme::TEXT_BRIGHT,
                    size: 11.0.into(),
                    align_x: text::Alignment::Center,
                    align_y: alignment::Vertical::Center,
                    ..Text::default()
                });
            }
        }
        // The current combination stands out from one that's only under the mouse.
        for (cell, color, width) in [
            (hovered, theme::CROSSHAIR, 1.0),
            (self.current, HANDLE, 1.5),
        ] {
            if let Some(cell) = cell {
                let at = area.top_left(cell % grid.width(), cell / grid.width());
                frame.stroke(
                    &Path::rectangle(
                        Point::new(at.x + 0.75, at.y + 0.75),
                        Size::new(area.cell_width - 1.5, area.cell_height - 1.5),
                    ),
                    Stroke::default().with_color(color).with_width(width),
                );
            }
        }

        let plot_width = area.cell_width * grid.width() as f32;
        let bottom = area.y + area.cell_height * area.rows as f32;
        let every = ((44.0 / area.cell_width).ceil() as usize).max(1);
        for (i, value) in grid.first.values.iter().enumerate().step_by(every) {
            grid_label(
                frame,
                crate::sweep::label(*value),
                area.cell_width * (i as f32 + 0.5),
                bottom + TIME_STRIP / 2.0,
                text::Alignment::Center,
            );
        }
        if let Some(second) = &grid.second {
            let every = ((16.0 / area.cell_height).ceil() as usize).max(1);
            for (row, value) in second.values.iter().enumerate().step_by(every) {
                grid_label(
                    frame,
                    crate::sweep::label(*value),
                    plot_width + 8.0,
                    area.top_left(0, row).y + area.cell_height / 2.0,
                    text::Alignment::Left,
                );
            }
        }
        frame.fill_rectangle(
            Point::new(plot_width, 0.0),
            Size::new(1.0, size.height),
            theme::AXIS_LINE,
        );
        frame.fill_rectangle(
            Point::new(0.0, bottom),
            Size::new(size.width, 1.0),
            theme::AXIS_LINE,
        );
    }
}

impl canvas::Program<PanelMessage> for Landscape<'_> {
    type State = ();

    fn update(
        &self,
        _state: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<PanelMessage>> {
        if matches!(event, Event::Mouse(mouse::Event::CursorMoved { .. })) {
            return cursor
                .position_in(bounds)
                .map(|_| canvas::Action::request_redraw());
        }
        let Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) = event else {
            return None;
        };
        let cell = self.cell_at(cursor.position_in(bounds)?, bounds.size())?;
        self.found.outcomes[cell].score()?;
        Some(canvas::Action::publish(PanelMessage::Apply(cell)).and_capture())
    }

    fn mouse_interaction(
        &self,
        _state: &(),
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let scored = cursor
            .position_in(bounds)
            .and_then(|point| self.cell_at(point, bounds.size()))
            .is_some_and(|cell| self.found.outcomes[cell].score().is_some());
        if scored {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let size = bounds.size();
        let mut frame = Frame::new(renderer, size);
        let hovered = cursor
            .position_in(bounds)
            .and_then(|point| self.cell_at(point, size));
        if self.is_heatmap() {
            self.draw_heat(&mut frame, hovered);
        } else {
            self.draw_bars(&mut frame, hovered);
        }
        if let Some(cell) = hovered {
            let plot = (size.width - PRICE_AXIS_WIDTH).max(1.0);
            let point = cursor.position_in(bounds).unwrap_or(Point::ORIGIN);
            hover_label(&mut frame, self.describe(cell), point.x, point.y, plot);
        }
        vec![frame.into_geometry()]
    }
}

/// The heatmap's colors, with its lowest and highest scores and where zero is.
pub struct HeatScale {
    pub low: f64,
    pub high: f64,
}

impl canvas::Program<PanelMessage> for HeatScale {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let reach = self.low.abs().max(self.high.abs());
        // Zero is always at the middle, with the same color for the same distance either side.
        let width = bounds.width;
        let steps = 48;
        let step = width / steps as f32;
        for i in 0..steps {
            let share = (f64::from(i) + 0.5) / f64::from(steps) * 2.0 - 1.0;
            frame.fill_rectangle(
                Point::new(step * i as f32, 0.0),
                Size::new(step + 0.5, 8.0),
                heat(share * reach, reach),
            );
        }
        let label = |frame: &mut Frame, content: String, x: f32, align| {
            frame.fill_text(Text {
                content,
                position: Point::new(x, 11.0),
                color: theme::TEXT,
                size: 11.0.into(),
                align_x: align,
                ..Text::default()
            });
        };
        label(
            &mut frame,
            format!("Min {}", percent(self.low)),
            0.0,
            text::Alignment::Left,
        );
        label(
            &mut frame,
            "0".to_owned(),
            width / 2.0,
            text::Alignment::Center,
        );
        label(
            &mut frame,
            format!("Max {}", percent(self.high)),
            width,
            text::Alignment::Right,
        );
        vec![frame.into_geometry()]
    }
}

// --- Icons -----------------------------------------------------------------------------------

/// A control's picture. Drawn rather than taken from a font, so they're all one weight, and
/// never turn into emoji.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Play,
    Pause,
    Restart,
    Step,
}

/// Goes inside a button: muted, brighter under the mouse, like a chart's corner buttons.
pub struct Icon {
    pub glyph: Glyph,
    pub enabled: bool,
    /// Lit, for what's switched on or the main control.
    pub lit: bool,
}

impl canvas::Program<PanelMessage> for Icon {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let hot = self.enabled && cursor.position_in(bounds).is_some();
        let color = match (self.enabled, self.lit, hot) {
            (false, _, _) => theme::LABEL_BACKGROUND,
            (true, _, true) => theme::TEXT_BRIGHT,
            (true, true, false) => theme::ACCENT,
            (true, false, false) => theme::TEXT_MUTED,
        };
        let c = frame.center();
        let at = |x: f32, y: f32| Point::new(c.x + x, c.y + y);
        let stroke = Stroke::default().with_color(color).with_width(1.4);
        match self.glyph {
            Glyph::Play => {
                let shape = Path::new(|b| {
                    b.move_to(at(-4.0, -6.0));
                    b.line_to(at(6.0, 0.0));
                    b.line_to(at(-4.0, 6.0));
                    b.close();
                });
                frame.fill(&shape, color);
            }
            Glyph::Pause => {
                frame.fill_rectangle(at(-5.0, -6.0), Size::new(3.5, 12.0), color);
                frame.fill_rectangle(at(1.5, -6.0), Size::new(3.5, 12.0), color);
            }
            Glyph::Step => {
                let shape = Path::new(|b| {
                    b.move_to(at(-5.0, -5.5));
                    b.line_to(at(3.0, 0.0));
                    b.line_to(at(-5.0, 5.5));
                    b.close();
                });
                frame.fill(&shape, color);
                frame.fill_rectangle(at(3.5, -5.5), Size::new(1.8, 11.0), color);
            }
            Glyph::Restart => {
                // Most of a circle, with an arrowhead where it starts.
                let arc = Path::new(|b| {
                    b.arc(canvas::path::Arc {
                        center: c,
                        radius: 5.5,
                        start_angle: Radians(-1.2),
                        end_angle: Radians(-1.2 + 5.2),
                    });
                });
                frame.stroke(&arc, stroke);
                let head = Path::new(|b| {
                    b.move_to(at(-4.2, -8.0));
                    b.line_to(at(-3.2, -3.0));
                    b.line_to(at(1.6, -5.0));
                    b.close();
                });
                frame.fill(&head, color);
            }
        }
        vec![frame.into_geometry()]
    }
}

// --- Span ------------------------------------------------------------------------------------

/// The span tested, as two handles over the market's whole history, with the playhead between
/// them. Dragging a handle moves it (the bar between them moves both); clicking inside seeks.
pub struct Span<'a> {
    pub history: &'a History,
    pub from: UnixNanos,
    pub to: UnixNanos,
    pub playhead: Option<UnixNanos>,
}

#[derive(Default)]
pub struct SpanDrag {
    grabbed: Option<Grab>,
}

#[derive(Clone, Copy)]
enum Grab {
    From,
    To,
    /// Both, from where the bar was taken: (cursor time, from, to).
    Both(UnixNanos, UnixNanos, UnixNanos),
}

/// Pixels a handle can be grabbed from.
const HANDLE_REACH: f32 = 8.0;

impl Span<'_> {
    fn x(&self, ts: UnixNanos, width: f32) -> f32 {
        let span = self.history.last.saturating_sub(self.history.first).max(1) as f64;
        (ts.saturating_sub(self.history.first) as f64 / span) as f32 * width
    }

    fn time(&self, x: f32, width: f32) -> UnixNanos {
        let span = self.history.last.saturating_sub(self.history.first) as f64;
        let share = f64::from((x / width.max(1.0)).clamp(0.0, 1.0));
        self.history.first + (share * span) as UnixNanos
    }
}

impl canvas::Program<PanelMessage> for Span<'_> {
    type State = SpanDrag;

    fn update(
        &self,
        state: &mut SpanDrag,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<PanelMessage>> {
        let w = bounds.width;
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                let (fx, tx) = (self.x(self.from, w), self.x(self.to, w));
                state.grabbed = Some(if (p.x - fx).abs() <= HANDLE_REACH {
                    Grab::From
                } else if (p.x - tx).abs() <= HANDLE_REACH {
                    Grab::To
                } else if p.x > fx && p.x < tx && p.y < bounds.height * 0.35 {
                    Grab::Both(self.time(p.x, w), self.from, self.to)
                } else if p.x > fx && p.x < tx {
                    return Some(
                        canvas::Action::publish(PanelMessage::Seek(self.time(p.x, w)))
                            .and_capture(),
                    );
                } else {
                    return Some(canvas::Action::capture());
                });
                Some(canvas::Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let grab = state.grabbed?;
                let x = cursor.position_from(bounds.position())?.x;
                let at = self.time(x, w);
                let (from, to) = match grab {
                    Grab::From => (at.min(self.to.saturating_sub(1)), self.to),
                    Grab::To => (self.from, at.max(self.from + 1)),
                    Grab::Both(taken, from, to) => {
                        let width = to - from;
                        let shift = at as i128 - taken as i128;
                        let first = self.history.first as i128;
                        let last = self.history.last as i128 - width as i128;
                        let from = (from as i128 + shift).clamp(first, last.max(first));
                        (from as UnixNanos, from as UnixNanos + width)
                    }
                };
                Some(canvas::Action::publish(PanelMessage::SpanDragged(from, to)).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                state.grabbed.take()?;
                Some(canvas::Action::publish(PanelMessage::SpanReleased).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &SpanDrag,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let (fx, tx) = (self.x(self.from, w), self.x(self.to, w));

        // Prices across the whole history.
        let closes = &self.history.closes;
        if closes.len() >= 2 {
            let (lo, hi) = closes
                .iter()
                .fold((f64::MAX, f64::MIN), |(lo, hi), &(_, c)| {
                    (lo.min(c), hi.max(c))
                });
            // On a log scale, so early years of a market that grew a lot still show.
            let (lo, hi) = (lo.max(1e-12).ln(), hi.max(1e-12).ln());
            let y = |c: f64| {
                let share = (c.max(1e-12).ln() - lo) / (hi - lo).max(1e-12);
                h - 3.0 - share as f32 * (h - 6.0)
            };
            let line = Path::new(|b| {
                b.move_to(Point::new(self.x(closes[0].0, w), y(closes[0].1)));
                for &(ts, c) in &closes[1..] {
                    b.line_to(Point::new(self.x(ts, w), y(c)));
                }
            });
            frame.stroke(
                &line,
                Stroke::default()
                    .with_color(Color {
                        a: 0.55,
                        ..theme::TEXT_MUTED
                    })
                    .with_width(1.0),
            );
        }
        frame.fill_rectangle(
            Point::new(fx, 0.0),
            Size::new((tx - fx).max(1.0), h),
            Color {
                a: 0.14,
                ..theme::ACCENT
            },
        );
        if let Some(at) = self.playhead {
            let px = self.x(at, w);
            frame.fill_rectangle(
                Point::new(fx, h - 2.0),
                Size::new((px - fx).max(0.0), 2.0),
                theme::ACCENT,
            );
            frame.fill_rectangle(Point::new(px, 0.0), Size::new(1.0, h), theme::TEXT_BRIGHT);
        }
        // Thin lines with a small grip, like the chart's own crosshair and tags.
        for x in [fx, tx] {
            frame.fill_rectangle(Point::new(x, 0.0), Size::new(1.0, h), theme::ACCENT);
            frame.fill_rectangle(
                Point::new(x - 2.0, h / 2.0 - 6.0),
                Size::new(5.0, 12.0),
                HANDLE,
            );
        }
        // The years along the bottom, where there's room.
        let first_year = DateTime::from_nanos(self.history.first).year;
        let last_year = DateTime::from_nanos(self.history.last).year;
        let years = (last_year - first_year).max(1);
        let every = ((years as f32 * 34.0 / w.max(1.0)).ceil() as i64).max(1);
        for year in (first_year + 1..=last_year).filter(|y| (y - first_year) % every == 0) {
            let ts = persephone_core::time::days_from_civil(year, 1, 1) as UnixNanos
                * persephone_core::time::NANOS_PER_DAY;
            let x = self.x(ts, w);
            frame.fill_rectangle(Point::new(x, 0.0), Size::new(1.0, h), theme::GRID_MAJOR);
            frame.fill_text(Text {
                content: year.to_string(),
                position: Point::new(x + 4.0, h / 2.0),
                color: theme::TEXT_MUTED,
                size: theme::TEXT_SIZE.into(),
                align_y: alignment::Vertical::Center,
                ..Text::default()
            });
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &SpanDrag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.grabbed.is_some() {
            return mouse::Interaction::Grabbing;
        }
        let Some(p) = cursor.position_in(bounds) else {
            return mouse::Interaction::default();
        };
        let (fx, tx) = (
            self.x(self.from, bounds.width),
            self.x(self.to, bounds.width),
        );
        if (p.x - fx).abs() <= HANDLE_REACH || (p.x - tx).abs() <= HANDLE_REACH {
            mouse::Interaction::ResizingHorizontally
        } else if p.x > fx && p.x < tx && p.y < bounds.height * 0.35 {
            mouse::Interaction::Grab
        } else if p.x > fx && p.x < tx {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

// --- Speed -----------------------------------------------------------------------------------

/// A long track from real time to Max: drag or click to set, scroll to nudge.
pub struct Speed {
    pub position: f32,
}

#[derive(Default)]
pub struct SpeedDrag {
    dragging: bool,
}

/// Room at each end of the track for the labels.
const SPEED_INSET: f32 = 12.0;

fn speed_from_x(x: f32, width: f32) -> f32 {
    ((x - SPEED_INSET) / (width - 2.0 * SPEED_INSET).max(1.0)).clamp(0.0, 1.0)
}

fn speed_x(position: f32, width: f32) -> f32 {
    SPEED_INSET + position * (width - 2.0 * SPEED_INSET)
}

impl canvas::Program<PanelMessage> for Speed {
    type State = SpeedDrag;

    fn update(
        &self,
        state: &mut SpeedDrag,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<PanelMessage>> {
        let set = |x: f32| {
            canvas::Action::publish(PanelMessage::Speed(speed_from_x(x, bounds.width)))
                .and_capture()
        };
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = cursor.position_in(bounds)?;
                state.dragging = true;
                Some(set(p.x))
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
                Some(set(cursor.position_from(bounds.position())?.x))
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.dragging => {
                state.dragging = false;
                Some(canvas::Action::capture())
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                cursor.position_in(bounds)?;
                let (dx, dy) = match *delta {
                    ScrollDelta::Lines { x, y } => (x * 20.0, y * 20.0),
                    ScrollDelta::Pixels { x, y } => (x, y),
                };
                let step = if dx.abs() > dy.abs() { dx } else { dy };
                let position = (self.position + step / 4_000.0).clamp(0.0, 1.0);
                Some(canvas::Action::publish(PanelMessage::Speed(position)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &SpeedDrag,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let (w, h) = (bounds.width, bounds.height);
        let track_y = 9.0;
        frame.fill_rectangle(
            Point::new(SPEED_INSET, track_y - 2.0),
            Size::new(w - 2.0 * SPEED_INSET, 2.0),
            theme::LABEL_BACKGROUND,
        );
        let knob = speed_x(self.position, w);
        frame.fill_rectangle(
            Point::new(SPEED_INSET, track_y - 2.0),
            Size::new(knob - SPEED_INSET, 2.0),
            theme::ACCENT,
        );
        // Labels where they fit, never closer than their own width.
        let mut last_label = f32::MIN;
        for (label, speed) in SPEED_LABELS {
            let x = speed_x(speed.map_or(1.0, position_of), w);
            frame.fill_rectangle(
                Point::new(x, track_y + 3.0),
                Size::new(1.0, 4.0),
                theme::AXIS_LINE,
            );
            if x - last_label >= 26.0 {
                last_label = x;
                frame.fill_text(Text {
                    content: label.into(),
                    position: Point::new(x, h - 1.0),
                    color: theme::TEXT,
                    size: 10.0.into(),
                    align_x: text::Alignment::Center,
                    align_y: alignment::Vertical::Bottom,
                    ..Text::default()
                });
            }
        }
        let hot = state.dragging || cursor.position_in(bounds).is_some();
        let radius = if hot { 6.0 } else { 5.0 };
        frame.fill(
            &Path::circle(Point::new(knob, track_y - 1.0), radius),
            HANDLE,
        );
        if speed_at(self.position).is_none() {
            frame.fill(
                &Path::circle(Point::new(knob, track_y - 1.0), 2.0),
                theme::ACCENT,
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &SpeedDrag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.dragging {
            mouse::Interaction::Grabbing
        } else if cursor.position_in(bounds).is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

// --- Resizing --------------------------------------------------------------------------------

/// The panel's bottom edge, dragged to make it taller or shorter.
pub struct Edge;

#[derive(Default)]
pub struct EdgeDrag {
    dragging: bool,
}

impl canvas::Program<PanelMessage> for Edge {
    type State = EdgeDrag;

    fn update(
        &self,
        state: &mut EdgeDrag,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<PanelMessage>> {
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                cursor.position_in(bounds)?;
                state.dragging = true;
                Some(canvas::Action::capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) if state.dragging => {
                // The panel starts at the window's top, so the cursor's height is its height.
                Some(canvas::Action::publish(PanelMessage::Resize(position.y)).and_capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.dragging => {
                state.dragging = false;
                Some(canvas::Action::capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &EdgeDrag,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let hot = state.dragging || cursor.position_in(bounds).is_some();
        frame.fill_rectangle(
            Point::new(0.0, bounds.height - 1.0),
            Size::new(bounds.width, 1.0),
            if hot { theme::ACCENT } else { theme::AXIS_LINE },
        );
        if hot {
            let x = bounds.width / 2.0;
            frame.fill_rectangle(
                Point::new(x - 18.0, bounds.height / 2.0 - 1.0),
                Size::new(36.0, 2.0),
                theme::TEXT_MUTED,
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &EdgeDrag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.dragging || cursor.position_in(bounds).is_some() {
            mouse::Interaction::ResizingVertically
        } else {
            mouse::Interaction::default()
        }
    }
}
