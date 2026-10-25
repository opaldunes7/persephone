//! Top layer: axes, labels, last-price line, crosshair and legends, for every pane. It also
//! owns all mouse input for the chart:
//!
//! - a click (press and release without moving) publishes [`ChartMessage::Clicked`], or
//!   [`ChartMessage::StudyClicked`] on an indicator's legend;
//! - dragging the plot pans, scrolling zooms, horizontal swipes pan;
//! - dragging the price axis scales prices, and double-clicking it resets them.

use std::time::{Duration, Instant};

use iced::{
    Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme, alignment,
    mouse::{self, ScrollDelta},
    widget::{
        canvas::{self, Event, Frame, LineDash, Path, Stroke, Text},
        text::Alignment,
    },
};

use persephone_core::Side;

use crate::{
    ChartData, ChartMessage, ChartState, PRICE_AXIS_WIDTH, Panes, TIME_AXIS_HEIGHT, plot_size,
    study::Placement,
    theme,
    ticks::{self, format_price, format_value, step_decimals},
    viewport::Projection,
};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// How far the mouse may move between press and release and still count as a click.
const CLICK_SLOP: f32 = 4.0;
/// Rough advance of one monospace glyph, as a fraction of the font size.
const MONO_ADVANCE: f32 = 0.62;
const LEGEND_X: f32 = 12.0;
const LEGEND_ROW: f32 = 20.0;
const TAG_HEIGHT: f32 = 18.0;

pub(crate) struct Overlay<'a> {
    pub(crate) chart: &'a ChartState,
}

#[derive(Default)]
pub(crate) struct Interaction {
    drag: Drag,
    hovering: bool,
    last_axis_click: Option<Instant>,
}

#[derive(Clone, Copy, Default)]
enum Drag {
    #[default]
    Idle,
    /// Pressed on the plot; becomes a pan once the mouse moves past [`CLICK_SLOP`].
    Press {
        start: Point,
    },
    Pan {
        last_x: f32,
    },
    Price {
        last_y: f32,
    },
}

fn in_plot(p: Point, plot: Size) -> bool {
    p.x < plot.width && p.y < plot.height
}

fn on_price_axis(p: Point, plot: Size) -> bool {
    p.x >= plot.width && p.y < plot.height
}

impl canvas::Program<ChartMessage> for Overlay<'_> {
    type State = Interaction;

    fn update(
        &self,
        state: &mut Interaction,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<ChartMessage>> {
        let plot = plot_size(bounds.size());
        let local = cursor.position_in(bounds);

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let p = local?;
                if on_price_axis(p, plot) {
                    let now = Instant::now();
                    if state
                        .last_axis_click
                        .is_some_and(|t| now - t < DOUBLE_CLICK)
                    {
                        state.last_axis_click = None;
                        return Some(
                            canvas::Action::publish(ChartMessage::ResetPriceScale).and_capture(),
                        );
                    }
                    state.last_axis_click = Some(now);
                    state.drag = Drag::Price { last_y: p.y };
                } else if in_plot(p, plot) {
                    state.drag = Drag::Press { start: p };
                } else {
                    return None;
                }
                Some(canvas::Action::capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => match state.drag {
                Drag::Idle => None,
                Drag::Press { start } => {
                    state.drag = Drag::Idle;
                    let panes = self.chart.panes(plot);
                    let bar = panes.price.bar_at(start.x, self.chart.data.candles.len());
                    let message = match legend_rows(self.chart, &panes, bar)
                        .into_iter()
                        .find(|row| row.contains(start))
                    {
                        Some(row) => ChartMessage::StudyClicked {
                            study: row.study,
                            at: start,
                        },
                        None => ChartMessage::Clicked { at: start },
                    };
                    Some(canvas::Action::publish(message).and_capture())
                }
                Drag::Pan { .. } | Drag::Price { .. } => {
                    state.drag = Drag::Idle;
                    Some(canvas::Action::request_redraw().and_capture())
                }
            },
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                // Drags keep going outside the chart, so use the absolute position.
                let p = Point::new(position.x - bounds.x, position.y - bounds.y);
                match state.drag {
                    Drag::Press { start } => {
                        if p.distance(start) <= CLICK_SLOP {
                            return Some(canvas::Action::request_redraw());
                        }
                        state.drag = Drag::Pan { last_x: p.x };
                        let message = ChartMessage::Pan {
                            dx: p.x - start.x,
                            plot_width: plot.width,
                        };
                        Some(canvas::Action::publish(message).and_capture())
                    }
                    Drag::Pan { last_x } => {
                        state.drag = Drag::Pan { last_x: p.x };
                        let message = ChartMessage::Pan {
                            dx: p.x - last_x,
                            plot_width: plot.width,
                        };
                        Some(canvas::Action::publish(message).and_capture())
                    }
                    Drag::Price { last_y } => {
                        state.drag = Drag::Price { last_y: p.y };
                        let message = ChartMessage::ScalePrice { dy: p.y - last_y };
                        Some(canvas::Action::publish(message).and_capture())
                    }
                    Drag::Idle => {
                        // Redraw the crosshair while hovering, and once more on the way out.
                        let was_hovering = std::mem::replace(&mut state.hovering, local.is_some());
                        (local.is_some() || was_hovering).then(canvas::Action::request_redraw)
                    }
                }
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                state.hovering = false;
                Some(canvas::Action::request_redraw())
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let p = local.filter(|p| in_plot(*p, plot))?;
                let (dx, dy) = match *delta {
                    ScrollDelta::Lines { x, y } => (x * 40.0, y * 40.0),
                    ScrollDelta::Pixels { x, y } => (x, y),
                };
                // Horizontal swipes pan; vertical scrolling zooms around the cursor.
                let message = if dx.abs() > dy.abs() {
                    ChartMessage::Pan {
                        dx,
                        plot_width: plot.width,
                    }
                } else {
                    ChartMessage::Zoom {
                        factor: (dy * 0.004).exp(),
                        from_right: plot.width - p.x,
                        plot_width: plot.width,
                    }
                };
                Some(canvas::Action::publish(message).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Interaction,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let size = bounds.size();
        let plot = plot_size(size);
        let mut frame = Frame::new(renderer, size);
        if plot.width < 1.0 || plot.height < 1.0 {
            return vec![frame.into_geometry()];
        }

        let data = &self.chart.data;
        let panes = self.chart.panes(plot);
        let hovered = cursor
            .position_in(bounds)
            .filter(|c| in_plot(*c, plot) && !matches!(state.drag, Drag::Price { .. }));
        let hovered_bar = hovered.and_then(|c| panes.price.bar_at(c.x, data.candles.len()));

        // Tags are laid out before the axis labels so labels under them can make way: iced
        // draws all text above all fills, so a tag can't simply cover a label.
        let crosshair_tag = hovered.and_then(|c| {
            let (study, pane) = panes.at(c.y)?;
            let value = pane.price_at(c.y);
            Some(Tag {
                center: c.y,
                text: match study {
                    None => format_price(value, data.price_precision),
                    Some(_) => format_value(value),
                },
                background: theme::LABEL_BACKGROUND,
            })
        });
        let last_tag = last_price_tag(data, &panes.price).filter(|last| {
            crosshair_tag
                .as_ref()
                .is_none_or(|c| !c.overlaps(last.center))
        });
        let price_tags: Vec<&Tag> = [last_tag.as_ref(), crosshair_tag.as_ref()]
            .into_iter()
            .flatten()
            .collect();
        let time_tag = hovered_bar.map(|i| {
            let text = ticks::crosshair_label(data.candles[i].ts, data.timeframe);
            let half = time_tag_width(&text) / 2.0;
            let center = panes
                .price
                .x(i as f64)
                .clamp(half, (plot.width - half).max(half));
            Tag {
                center,
                text,
                background: theme::LABEL_BACKGROUND,
            }
        });

        frame.fill_rectangle(
            Point::new(plot.width, 0.0),
            Size::new(1.0, size.height),
            theme::AXIS_LINE,
        );
        frame.fill_rectangle(
            Point::new(0.0, plot.height),
            Size::new(size.width, 1.0),
            theme::AXIS_LINE,
        );
        for (_, pane) in &panes.indicators {
            frame.fill_rectangle(
                Point::new(0.0, pane.offset),
                Size::new(size.width, 1.0),
                theme::PANE_SEPARATOR,
            );
        }
        draw_value_labels(&mut frame, data, &panes, plot, &price_tags);
        draw_time_labels(&mut frame, data, &panes.price, plot, time_tag.as_ref());
        draw_last_price_line(&mut frame, data, &panes.price, plot);
        draw_trade_markers(&mut frame, self.chart, &panes.price);
        if let Some(c) = hovered {
            draw_crosshair(&mut frame, &panes, plot, c, hovered_bar);
        }
        for tag in price_tags {
            draw_price_tag(&mut frame, plot, tag);
        }
        if let Some(tag) = &time_tag {
            draw_time_tag(&mut frame, plot, tag);
        }

        let legend_bar = hovered_bar.or(data.candles.len().checked_sub(1));
        if let Some(i) = legend_bar {
            draw_price_legend(&mut frame, data, i, plot.width);
        }
        for row in legend_rows(self.chart, &panes, legend_bar) {
            row.draw(&mut frame, plot.width);
        }

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Interaction,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        let plot = plot_size(bounds.size());
        match state.drag {
            Drag::Pan { .. } => mouse::Interaction::Grabbing,
            Drag::Price { .. } => mouse::Interaction::ResizingVertically,
            Drag::Idle | Drag::Press { .. } => match cursor.position_in(bounds) {
                Some(p) if on_price_axis(p, plot) => mouse::Interaction::ResizingVertically,
                Some(p) if in_plot(p, plot) => {
                    let panes = self.chart.panes(plot);
                    let bar = panes.price.bar_at(p.x, self.chart.data.candles.len());
                    if legend_rows(self.chart, &panes, bar)
                        .iter()
                        .any(|row| row.contains(p))
                    {
                        mouse::Interaction::Pointer
                    } else {
                        mouse::Interaction::Crosshair
                    }
                }
                _ => mouse::Interaction::default(),
            },
        }
    }
}

/// A filled label on an axis: the price axis (`center` is a y) or the time axis (an x).
struct Tag {
    center: f32,
    text: String,
    background: Color,
}

impl Tag {
    fn overlaps(&self, y: f32) -> bool {
        (self.center - y).abs() < TAG_HEIGHT
    }
}

fn time_tag_width(text: &str) -> f32 {
    text.chars().count() as f32 * theme::TEXT_SIZE * MONO_ADVANCE + 16.0
}

/// One clickable legend line for a study, e.g. `EMA 21  80512.33`.
struct LegendRow {
    study: usize,
    y: f32,
    /// Text pieces and their colors, laid out left to right in a monospace font.
    pieces: Vec<(String, Color)>,
}

impl LegendRow {
    fn advance() -> f32 {
        theme::TEXT_SIZE * MONO_ADVANCE
    }

    fn width(&self) -> f32 {
        let chars: usize = self
            .pieces
            .iter()
            .map(|(text, _)| text.chars().count() + 2)
            .sum();
        chars as f32 * Self::advance()
    }

    /// The label is always clickable, even when values are cut off in a narrow chart.
    fn contains(&self, p: Point) -> bool {
        let label = self
            .pieces
            .first()
            .map_or(0, |(text, _)| text.chars().count()) as f32;
        let width = self.width().max(label * Self::advance());
        (LEGEND_X - 4.0..=LEGEND_X + width + 4.0).contains(&p.x)
            && (p.y - self.y).abs() <= LEGEND_ROW / 2.0
    }

    fn draw(&self, frame: &mut Frame, plot_width: f32) {
        let mut x = LEGEND_X;
        for (i, (text, color)) in self.pieces.iter().enumerate() {
            let width = text.chars().count() as f32 * Self::advance();
            if i > 0 && x + width > plot_width - 8.0 {
                break;
            }
            label_with(
                frame,
                text.clone(),
                Point::new(x, self.y),
                *color,
                theme::TEXT_SIZE,
                Font::MONOSPACE,
            );
            x += width + 2.0 * Self::advance();
        }
    }
}

/// Legend rows for every study: overlays stack under the OHLC line, pane studies sit at the
/// top of their pane. Values are for bar `bar` (the hovered or newest one).
fn legend_rows(chart: &ChartState, panes: &Panes, bar: Option<usize>) -> Vec<LegendRow> {
    let precision = chart.data.price_precision;
    let mut overlay_y = 60.0;
    let mut rows = Vec::new();

    for (index, study) in chart.studies.iter().enumerate() {
        let y = match study.placement {
            Placement::Overlay => {
                overlay_y += LEGEND_ROW;
                overlay_y - LEGEND_ROW
            }
            Placement::Pane => match panes.indicators.iter().find(|(s, _)| *s == index) {
                Some((_, pane)) => pane.offset + 14.0,
                None => continue,
            },
        };
        let mut pieces = vec![(study.label.clone(), theme::TEXT_BRIGHT)];
        if let Some(error) = &study.error {
            pieces.push((error.clone(), theme::DOWN));
        } else if let Some(i) = bar {
            for line in &study.lines {
                let value = line.values.get(i).copied().unwrap_or(f64::NAN);
                let text = match study.placement {
                    Placement::Overlay if value.is_finite() => format_price(value, precision),
                    _ => format_value(value),
                };
                let color =
                    if value < 0.0 && matches!(line.style, crate::study::OutputStyle::Histogram) {
                        theme::DOWN
                    } else {
                        line.color
                    };
                pieces.push((text, color));
            }
        }
        rows.push(LegendRow {
            study: index,
            y,
            pieces,
        });
    }
    rows
}

fn draw_value_labels(
    frame: &mut Frame,
    data: &ChartData,
    panes: &Panes,
    plot: Size,
    tags: &[&Tag],
) {
    let mut draw_pane = |pane: &Projection, format: &dyn Fn(f64) -> String| {
        for value in ticks::price_ticks(pane) {
            let y = pane.y(value);
            let inside = y > pane.offset + 8.0 && y < pane.offset + pane.height - 8.0;
            if inside && !tags.iter().any(|tag| tag.overlaps(y)) {
                label(
                    frame,
                    format(value),
                    Point::new(plot.width + 8.0, y),
                    theme::TEXT,
                    Alignment::Left,
                );
            }
        }
    };

    draw_pane(&panes.price, &|v| format_price(v, data.price_precision));
    for (_, pane) in &panes.indicators {
        let ticks = ticks::price_ticks(pane);
        let decimals = match ticks.as_slice() {
            [a, b, ..] => step_decimals(b - a),
            _ => 2,
        };
        draw_pane(pane, &|v| format!("{v:.decimals$}"));
    }
}

fn draw_time_labels(
    frame: &mut Frame,
    data: &ChartData,
    price: &Projection,
    plot: Size,
    time_tag: Option<&Tag>,
) {
    // Half the width of a typical "HH:MM" label, so neighbours of the time tag clear it too.
    let label_half = 3.0 * theme::TEXT_SIZE * MONO_ADVANCE;
    for tick in ticks::time_ticks(&data.candles, price, data.timeframe) {
        let x = price.x(tick.index as f64);
        let hidden = time_tag.is_some_and(|tag| {
            (tag.center - x).abs() < time_tag_width(&tag.text) / 2.0 + label_half
        });
        if !hidden {
            let color = if tick.major {
                theme::TEXT_BRIGHT
            } else {
                theme::TEXT
            };
            label(
                frame,
                tick.label,
                Point::new(x, plot.height + TIME_AXIS_HEIGHT / 2.0),
                color,
                Alignment::Center,
            );
        }
    }
}

fn last_price_tag(data: &ChartData, price: &Projection) -> Option<Tag> {
    let last = data.candles.last()?;
    let y = price.y(last.close);
    price.contains_y(y).then(|| Tag {
        center: y,
        text: format_price(last.close, data.price_precision),
        background: if last.close >= last.open {
            theme::UP
        } else {
            theme::DOWN
        },
    })
}

fn draw_last_price_line(frame: &mut Frame, data: &ChartData, price: &Projection, plot: Size) {
    let Some(last) = data.candles.last() else {
        return;
    };
    let y = price.y(last.close);
    if !price.contains_y(y) {
        return;
    }
    let color = if last.close >= last.open {
        theme::UP
    } else {
        theme::DOWN
    };
    let y = y.round() + 0.5;
    frame.stroke(
        &Path::line(Point::new(0.0, y), Point::new(plot.width, y)),
        dashed(color, &[1.0, 2.0]),
    );
}

/// An arrow under the bar for each buy and over it for each sell, labelled with the position
/// it leaves when bars are wide enough for text.
fn draw_trade_markers(frame: &mut Frame, chart: &ChartState, price: &Projection) {
    let Some(trades) = &chart.trades else { return };
    let first = trades
        .markers
        .partition_point(|m| m.bar < price.visible.start);
    let labelled = price.spacing >= 5.0;

    for marker in trades.markers[first..]
        .iter()
        .take_while(|m| m.bar < price.visible.end)
    {
        let Some(candle) = chart.data.candles.get(marker.bar) else {
            continue;
        };
        let x = price.x(marker.bar as f64);
        let (tip, base, label_y, color) = match marker.side {
            Side::Buy => {
                let tip = price.y(candle.low) + 4.0;
                (tip, tip + 7.0, tip + 17.0, theme::UP)
            }
            Side::Sell => {
                let tip = price.y(candle.high) - 4.0;
                (tip, tip - 7.0, tip - 17.0, theme::DOWN)
            }
        };
        if !price.contains_y(tip) {
            continue;
        }
        let arrow = Path::new(|b| {
            b.move_to(Point::new(x, tip));
            b.line_to(Point::new(x - 4.5, base));
            b.line_to(Point::new(x + 4.5, base));
            b.close();
        });
        frame.fill(&arrow, color);
        if labelled && price.contains_y(label_y) {
            // A filled tag, so the label reads over candles of the same color.
            let width = marker.label.len() as f32 * 10.0 * MONO_ADVANCE + 8.0;
            frame.fill_rectangle(
                Point::new(x - width / 2.0, label_y - 7.0),
                Size::new(width, 14.0),
                color,
            );
            frame.fill_text(Text {
                content: marker.label.into(),
                position: Point::new(x, label_y),
                color: theme::LABEL_TEXT,
                size: Pixels(10.0),
                align_x: Alignment::Center,
                align_y: alignment::Vertical::Center,
                ..Text::default()
            });
        }
    }
}

/// The vertical line crosses every pane; the horizontal one stays in the hovered pane.
fn draw_crosshair(frame: &mut Frame, panes: &Panes, plot: Size, cursor: Point, bar: Option<usize>) {
    // The vertical line snaps to the hovered bar's center, as TradingView does.
    let x = bar.map_or(cursor.x, |i| panes.price.x(i as f64)).round() + 0.5;
    let y = cursor.y.round() + 0.5;
    let stroke = dashed(theme::CROSSHAIR, &[4.0, 4.0]);
    frame.stroke(
        &Path::line(Point::new(x, 0.0), Point::new(x, plot.height)),
        stroke,
    );
    if panes.at(cursor.y).is_some() {
        frame.stroke(
            &Path::line(Point::new(0.0, y), Point::new(plot.width, y)),
            stroke,
        );
    }
}

fn draw_price_tag(frame: &mut Frame, plot: Size, tag: &Tag) {
    frame.fill_rectangle(
        Point::new(plot.width + 1.0, tag.center - TAG_HEIGHT / 2.0),
        Size::new(PRICE_AXIS_WIDTH - 1.0, TAG_HEIGHT),
        tag.background,
    );
    label(
        frame,
        tag.text.clone(),
        Point::new(plot.width + 8.0, tag.center),
        theme::LABEL_TEXT,
        Alignment::Left,
    );
}

fn draw_time_tag(frame: &mut Frame, plot: Size, tag: &Tag) {
    let width = time_tag_width(&tag.text);
    frame.fill_rectangle(
        Point::new(tag.center - width / 2.0, plot.height + 1.0),
        Size::new(width, TIME_AXIS_HEIGHT - 2.0),
        tag.background,
    );
    label(
        frame,
        tag.text.clone(),
        Point::new(tag.center, plot.height + TIME_AXIS_HEIGHT / 2.0),
        theme::LABEL_TEXT,
        Alignment::Center,
    );
}

/// `BTCUSDT · 1m · BINANCE` and the O/H/L/C/change line for bar `i`. Fields that would run
/// past the plot are left out, so narrow tiles show what fits.
fn draw_price_legend(frame: &mut Frame, data: &ChartData, i: usize, plot_width: f32) {
    let c = &data.candles[i];
    label_with(
        frame,
        format!("{} · {} · {}", data.symbol, data.timeframe, data.venue),
        Point::new(LEGEND_X, 16.0),
        theme::TEXT_BRIGHT,
        theme::LEGEND_SIZE,
        Font::default(),
    );

    let prev_close = i.checked_sub(1).map_or(c.open, |j| data.candles[j].close);
    let change = c.close - prev_close;
    let color = if c.close >= c.open {
        theme::UP
    } else {
        theme::DOWN
    };
    let percent = if prev_close == 0.0 {
        0.0
    } else {
        change / prev_close * 100.0
    };
    let prec = data.price_precision;

    let fields = [
        ("O", format_price(c.open, prec)),
        ("H", format_price(c.high, prec)),
        ("L", format_price(c.low, prec)),
        ("C", format_price(c.close, prec)),
        (
            "",
            format!(
                "{change:+.prec$} ({percent:+.2}%)",
                prec = usize::from(prec)
            ),
        ),
    ];
    let size = theme::TEXT_SIZE + 1.0;
    let advance = size * MONO_ADVANCE;
    let mut x = LEGEND_X;
    for (name, value) in fields {
        let label_width = if name.is_empty() { 0.0 } else { advance * 2.0 };
        let width = value.chars().count() as f32 * advance;
        if x + label_width + width > plot_width - 8.0 {
            break;
        }
        if !name.is_empty() {
            label_with(
                frame,
                name,
                Point::new(x, 38.0),
                theme::TEXT_MUTED,
                size,
                Font::MONOSPACE,
            );
            x += label_width;
        }
        label_with(
            frame,
            value,
            Point::new(x, 38.0),
            color,
            size,
            Font::MONOSPACE,
        );
        x += width + advance * 2.0;
    }
}

fn dashed(color: Color, segments: &[f32]) -> Stroke<'_> {
    Stroke {
        line_dash: LineDash {
            segments,
            offset: 0,
        },
        ..Stroke::default().with_color(color).with_width(1.0)
    }
}

fn label(
    frame: &mut Frame,
    content: impl Into<String>,
    position: Point,
    color: Color,
    align_x: Alignment,
) {
    frame.fill_text(Text {
        content: content.into(),
        position,
        color,
        size: Pixels(theme::TEXT_SIZE),
        align_x,
        align_y: alignment::Vertical::Center,
        ..Text::default()
    });
}

fn label_with(
    frame: &mut Frame,
    content: impl Into<String>,
    position: Point,
    color: Color,
    size: f32,
    font: Font,
) {
    frame.fill_text(Text {
        content: content.into(),
        position,
        color,
        size: Pixels(size),
        font,
        align_y: alignment::Vertical::Center,
        ..Text::default()
    });
}
