//! The panel's drawn controls: the equity curve, the span slider and the speed slider, plus the
//! handle that resizes the panel.

use iced::{
    Color, Point, Rectangle, Renderer, Size, Theme, alignment,
    mouse::{self, ScrollDelta},
    widget::canvas::{self, Event, Frame, Path, Stroke, Text},
};
use persephone_chart::theme;
use persephone_core::{UnixNanos, time::DateTime};

use super::{History, PanelMessage, SPEED_LABELS, position_of, speed_at};

const TRACK: Color = Color::from_rgb8(0x2a, 0x2e, 0x39);
const HANDLE: Color = Color::from_rgb8(0xd1, 0xd4, 0xdc);

// --- Equity ----------------------------------------------------------------------------------

/// Equity over the span, drawn as far as the playhead: a line over the starting capital, with
/// falls from the running peak shaded.
pub struct Equity<'a> {
    /// One value per bar of the span.
    pub values: &'a [f64],
    /// How many have happened; all of them without a playback.
    pub shown: usize,
    pub start: f64,
}

impl canvas::Program<PanelMessage> for Equity<'_> {
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
        let (w, h) = (bounds.width, bounds.height);
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), theme::BACKGROUND);
        let shown = &self.values[..self.shown.min(self.values.len())];
        if self.values.len() < 2 || w < 4.0 || h < 4.0 {
            return vec![frame.into_geometry()];
        }

        // The scale covers the whole run, so it doesn't jump as the playhead moves.
        let (lo, hi) = self
            .values
            .iter()
            .chain(std::iter::once(&self.start))
            .fold((f64::MAX, f64::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        let pad = ((hi - lo) * 0.08).max(hi.abs() * 1e-6).max(1e-9);
        let (lo, hi) = (lo - pad, hi + pad);
        let x = |i: usize| i as f32 / (self.values.len() - 1) as f32 * w;
        let y = |v: f64| ((hi - v) / (hi - lo)) as f32 * h;

        let base = y(self.start).round() + 0.5;
        frame.stroke(
            &Path::line(Point::new(0.0, base), Point::new(w, base)),
            Stroke::default()
                .with_color(theme::AXIS_LINE)
                .with_width(1.0),
        );
        if shown.len() >= 2 {
            // Drawdown: from the running peak down to the curve.
            let mut peak = f64::MIN;
            let peaks: Vec<f64> = shown
                .iter()
                .map(|&v| {
                    peak = peak.max(v);
                    peak
                })
                .collect();
            let shade = Path::new(|b| {
                b.move_to(Point::new(x(0), y(peaks[0])));
                for (i, &p) in peaks.iter().enumerate() {
                    b.line_to(Point::new(x(i), y(p)));
                }
                for (i, &v) in shown.iter().enumerate().rev() {
                    b.line_to(Point::new(x(i), y(v)));
                }
                b.close();
            });
            frame.fill(
                &shade,
                Color {
                    a: 0.12,
                    ..theme::DOWN
                },
            );

            let last = *shown.last().unwrap_or(&self.start);
            let color = if last >= self.start {
                theme::UP
            } else {
                theme::DOWN
            };
            let line = Path::new(|b| {
                b.move_to(Point::new(x(0), y(shown[0])));
                for (i, &v) in shown.iter().enumerate().skip(1) {
                    b.line_to(Point::new(x(i), y(v)));
                }
            });
            frame.stroke(&line, Stroke::default().with_color(color).with_width(1.5));
        }

        if let Some(p) = cursor.position_in(bounds) {
            let i = ((p.x / w) * (self.values.len() - 1) as f32).round() as usize;
            if let Some(&v) = shown.get(i) {
                frame.fill_rectangle(
                    Point::new(x(i).round(), 0.0),
                    Size::new(1.0, h),
                    theme::CROSSHAIR,
                );
                let change = (v / self.start - 1.0) * 100.0;
                frame.fill_text(Text {
                    content: format!("{v:.2}  ({change:+.2}%)"),
                    position: Point::new((x(i) + 6.0).min(w - 120.0), 6.0),
                    color: theme::TEXT_BRIGHT,
                    size: 11.0.into(),
                    ..Text::default()
                });
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

        frame.fill_rectangle(Point::ORIGIN, bounds.size(), TRACK);
        // Prices across the whole history, faint outside the span.
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
                a: 0.2,
                ..theme::ACCENT
            },
        );
        if let Some(at) = self.playhead {
            let px = self.x(at, w);
            frame.fill_rectangle(
                Point::new(fx, h - 3.0),
                Size::new((px - fx).max(0.0), 3.0),
                theme::ACCENT,
            );
            frame.fill_rectangle(
                Point::new(px - 1.0, 0.0),
                Size::new(2.0, h),
                theme::TEXT_BRIGHT,
            );
        }
        for x in [fx, tx] {
            frame.fill_rectangle(Point::new(x - 2.5, 0.0), Size::new(5.0, h), HANDLE);
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
            frame.fill_rectangle(
                Point::new(x, h - 6.0),
                Size::new(1.0, 6.0),
                theme::AXIS_LINE,
            );
            frame.fill_text(Text {
                content: year.to_string(),
                position: Point::new(x + 3.0, h - 2.0),
                color: theme::TEXT_MUTED,
                size: 9.0.into(),
                align_y: alignment::Vertical::Bottom,
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
            Size::new(w - 2.0 * SPEED_INSET, 4.0),
            TRACK,
        );
        let knob = speed_x(self.position, w);
        frame.fill_rectangle(
            Point::new(SPEED_INSET, track_y - 2.0),
            Size::new(knob - SPEED_INSET, 4.0),
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
                    color: theme::TEXT_MUTED,
                    size: 10.0.into(),
                    align_x: iced::widget::text::Alignment::Center,
                    align_y: alignment::Vertical::Bottom,
                    ..Text::default()
                });
            }
        }
        let hot = state.dragging || cursor.position_in(bounds).is_some();
        let radius = if hot { 7.0 } else { 6.0 };
        frame.fill(&Path::circle(Point::new(knob, track_y), radius), HANDLE);
        if speed_at(self.position).is_none() {
            frame.fill(&Path::circle(Point::new(knob, track_y), 2.5), theme::ACCENT);
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
