//! Top layer: axes, labels, last-price line, crosshair and legend. It also owns all mouse
//! input for the chart: drag pans, scroll zooms, dragging the price axis scales price, and
//! double-clicking the price axis resets it.

use std::time::{Duration, Instant};

use iced::{
    Color, Font, Pixels, Point, Rectangle, Renderer, Size, Theme, alignment,
    mouse::{self, ScrollDelta},
    widget::{
        canvas::{self, Event, Frame, LineDash, Path, Stroke, Text},
        text::Alignment,
    },
};

use crate::{
    ChartData, ChartMessage, ChartState, PRICE_AXIS_WIDTH, TIME_AXIS_HEIGHT, plot_size, theme,
    ticks::{self, format_price},
    viewport::Projection,
};

const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rough advance of one monospace glyph, as a fraction of the font size.
const MONO_ADVANCE: f32 = 0.62;

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
                    state.drag = Drag::Pan { last_x: p.x };
                } else {
                    return None;
                }
                Some(canvas::Action::capture())
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                if matches!(state.drag, Drag::Idle) {
                    return None;
                }
                state.drag = Drag::Idle;
                Some(canvas::Action::request_redraw().and_capture())
            }
            Event::Mouse(mouse::Event::CursorMoved { position }) => {
                // Drags keep going outside the chart, so use the absolute position.
                let p = Point::new(position.x - bounds.x, position.y - bounds.y);
                match state.drag {
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
        let p = self.chart.projection(plot);
        let hovered = cursor
            .position_in(bounds)
            .filter(|c| in_plot(*c, plot) && !matches!(state.drag, Drag::Price { .. }));
        let hovered_bar = hovered.and_then(|c| p.bar_at(c.x, data.candles.len()));

        // Tags are laid out before the axis labels so labels under them can make way: iced
        // draws all text above all fills, so a tag can't simply cover a label.
        let crosshair_tag = hovered.map(|c| Tag {
            center: c.y,
            text: format_price(p.price_at(c.y), data.price_precision),
            background: theme::LABEL_BACKGROUND,
        });
        let last_tag = last_price_tag(data, &p, plot).filter(|last| {
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
            let center = p.x(i as f64).clamp(half, (plot.width - half).max(half));
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
        draw_axis_labels(&mut frame, data, &p, plot, &price_tags, time_tag.as_ref());
        draw_last_price_line(&mut frame, data, &p, plot);
        if let Some(c) = hovered {
            draw_crosshair(&mut frame, &p, plot, c, hovered_bar);
        }
        for tag in price_tags {
            draw_price_tag(&mut frame, plot, tag);
        }
        if let Some(tag) = &time_tag {
            draw_time_tag(&mut frame, plot, tag);
        }
        if let Some(i) = hovered_bar.or(data.candles.len().checked_sub(1)) {
            draw_legend(&mut frame, data, i, plot.width);
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
            Drag::Idle => match cursor.position_in(bounds) {
                Some(p) if on_price_axis(p, plot) => mouse::Interaction::ResizingVertically,
                Some(p) if in_plot(p, plot) => mouse::Interaction::Crosshair,
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

const TAG_HEIGHT: f32 = 18.0;

impl Tag {
    fn overlaps(&self, y: f32) -> bool {
        (self.center - y).abs() < TAG_HEIGHT
    }
}

fn time_tag_width(text: &str) -> f32 {
    text.chars().count() as f32 * theme::TEXT_SIZE * MONO_ADVANCE + 16.0
}

fn draw_axis_labels(
    frame: &mut Frame,
    data: &ChartData,
    p: &Projection,
    plot: Size,
    price_tags: &[&Tag],
    time_tag: Option<&Tag>,
) {
    for price in ticks::price_ticks(p) {
        let y = p.y(price);
        let hidden = price_tags.iter().any(|tag| tag.overlaps(y));
        if y > 8.0 && y < plot.height - 8.0 && !hidden {
            label(
                frame,
                format_price(price, data.price_precision),
                Point::new(plot.width + 8.0, y),
                theme::TEXT,
                Alignment::Left,
            );
        }
    }

    // Half the width of a typical "HH:MM" label, so neighbours of the time tag clear it too.
    let label_half = 3.0 * theme::TEXT_SIZE * MONO_ADVANCE;
    for tick in ticks::time_ticks(&data.candles, p, data.timeframe) {
        let x = p.x(tick.index as f64);
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

fn last_price_tag(data: &ChartData, p: &Projection, plot: Size) -> Option<Tag> {
    let last = data.candles.last()?;
    let y = p.y(last.close);
    (0.0..=plot.height).contains(&y).then(|| Tag {
        center: y,
        text: format_price(last.close, data.price_precision),
        background: if last.close >= last.open {
            theme::UP
        } else {
            theme::DOWN
        },
    })
}

fn draw_last_price_line(frame: &mut Frame, data: &ChartData, p: &Projection, plot: Size) {
    let Some(last) = data.candles.last() else {
        return;
    };
    let y = p.y(last.close);
    if !(0.0..=plot.height).contains(&y) {
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

fn draw_crosshair(
    frame: &mut Frame,
    p: &Projection,
    plot: Size,
    cursor: Point,
    bar: Option<usize>,
) {
    // The vertical line snaps to the hovered bar's center, as TradingView does.
    let x = bar.map_or(cursor.x, |i| p.x(i as f64)).round() + 0.5;
    let y = cursor.y.round() + 0.5;
    let stroke = dashed(theme::CROSSHAIR, &[4.0, 4.0]);
    frame.stroke(
        &Path::line(Point::new(x, 0.0), Point::new(x, plot.height)),
        stroke,
    );
    frame.stroke(
        &Path::line(Point::new(0.0, y), Point::new(plot.width, y)),
        stroke,
    );
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
fn draw_legend(frame: &mut Frame, data: &ChartData, i: usize, plot_width: f32) {
    let c = &data.candles[i];
    label_with(
        frame,
        format!("{} · {} · {}", data.symbol, data.timeframe, data.venue),
        Point::new(12.0, 16.0),
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
    let mut x = 12.0;
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
