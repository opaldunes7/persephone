//! The team drawn: agent cards and the Lab card, and the links between them. A card drags; its
//! handle (the dot on its right edge) drags to another card to link them as the kind picked;
//! empty space pans, the wheel zooms, and a double-click there adds an agent.

use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use iced::{
    Color, Point, Rectangle, Renderer, Size, Theme, Vector, alignment,
    mouse::{self, ScrollDelta},
    widget::{
        canvas::{self, Event, Frame, LineDash, Path, Stroke, Text},
        text,
    },
};
use persephone_agents::{
    prompts::first_line,
    run::Status,
    team::{LAB, LabData, LinkKind, Team, View},
};
use persephone_chart::theme;

use super::{AgentsMessage, CardLive, Selected};
use crate::Message;

/// An agent card's size on the canvas, at zoom 1.
pub const CARD: Size = Size::new(210.0, 88.0);
pub const LAB_CARD: Size = Size::new(160.0, 58.0);
/// The link handle's radius, in pixels.
const HANDLE: f32 = 6.0;
/// How near a link a click picks it, in pixels.
const LINK_REACH: f32 = 6.0;
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
pub const ZOOM: (f32, f32) = (0.3, 2.5);
/// The grid's spacing, at zoom 1.
const GRID: f32 = 60.0;

const CARD_FILL: Color = Color::from_rgb8(0x1c, 0x20, 0x2b);
const LAB_FILL: Color = Color::from_rgb8(0x17, 0x24, 0x22);
const AMBER: Color = Color::from_rgb8(0xff, 0xa7, 0x26);

fn say(message: AgentsMessage) -> Message {
    Message::Agents(message)
}

/// What a status looks like on a card.
#[must_use]
pub fn status_color(status: &Status) -> Color {
    match status {
        Status::Waiting | Status::Skipped(_) => theme::TEXT_MUTED,
        Status::Running => theme::ACCENT,
        Status::WrappingUp => AMBER,
        Status::Done => theme::UP,
        Status::Failed(_) => theme::DOWN,
        Status::Stopped(_) => theme::TEXT,
    }
}

pub struct Board<'a> {
    pub team: &'a Team,
    pub selected: &'a Selected,
    /// Each agent's part in the run on screen, by id, while there's one.
    pub cards: Option<&'a HashMap<String, CardLive>>,
    pub lab_calls: u32,
    /// The agents Check found problems with.
    pub flagged: Vec<String>,
}

#[derive(Default)]
pub struct Drag {
    grab: Option<Grab>,
    /// The last press on empty space, for a double-click.
    last_press: Option<(Instant, Point)>,
    /// Where the cursor is while a link is drawn.
    cursor: Option<Point>,
}

enum Grab {
    /// A card, by id ([`LAB`] for Lab's), held this far from its centre.
    Card {
        id: String,
        offset: Vector,
        at: [f32; 2],
    },
    Pan {
        from: Point,
        view: View,
    },
    Link {
        from: String,
    },
}

/// Where a point of the team's plane is on screen, and back.
struct Projection {
    view: View,
    size: Size,
}

impl Projection {
    fn screen(&self, at: [f32; 2]) -> Point {
        Point::new(
            (at[0] - self.view.at[0]) * self.view.zoom + self.size.width / 2.0,
            (at[1] - self.view.at[1]) * self.view.zoom + self.size.height / 2.0,
        )
    }

    fn world(&self, point: Point) -> [f32; 2] {
        [
            (point.x - self.size.width / 2.0) / self.view.zoom + self.view.at[0],
            (point.y - self.size.height / 2.0) / self.view.zoom + self.view.at[1],
        ]
    }

    /// A card's rectangle on screen.
    fn card(&self, at: [f32; 2], size: Size) -> Rectangle {
        let centre = self.screen(at);
        let size = Size::new(size.width * self.view.zoom, size.height * self.view.zoom);
        Rectangle::new(
            Point::new(centre.x - size.width / 2.0, centre.y - size.height / 2.0),
            size,
        )
    }
}

impl Board<'_> {
    fn projection(&self, bounds: Rectangle) -> Projection {
        Projection {
            view: self.team.view,
            size: bounds.size(),
        }
    }

    /// Every card's id and rectangle on screen, Lab's last.
    fn rects(&self, p: &Projection) -> Vec<(&str, Rectangle)> {
        let mut rects: Vec<(&str, Rectangle)> = self
            .team
            .agents
            .iter()
            .map(|a| (a.id.as_str(), p.card(a.at, CARD)))
            .collect();
        rects.push((LAB, p.card(self.team.lab.at, LAB_CARD)));
        rects
    }

    fn card_at(&self, p: &Projection, point: Point) -> Option<String> {
        self.rects(p)
            .into_iter()
            .rev()
            .find(|(_, rect)| rect.contains(point))
            .map(|(id, _)| id.to_owned())
    }

    fn handle_at(&self, p: &Projection, point: Point) -> Option<String> {
        self.team
            .agents
            .iter()
            .find(|a| handle(p.card(a.at, CARD)).distance(point) <= HANDLE + 3.0)
            .map(|a| a.id.clone())
    }

    fn link_at(&self, p: &Projection, point: Point) -> Option<usize> {
        let rects = self.rects(p);
        let rect = |id: &str| rects.iter().find(|(r, _)| *r == id).map(|(_, rect)| *rect);
        self.team.links.iter().enumerate().find_map(|(i, link)| {
            let (a, b) = ends(rect(&link.from)?, rect(&link.to)?);
            (distance_to_segment(point, a, b) <= LINK_REACH).then_some(i)
        })
    }
}

/// Where a card's link handle is: on its right edge, above the middle, where links between cards
/// side by side don't end.
fn handle(rect: Rectangle) -> Point {
    Point::new(rect.x + rect.width, rect.y + rect.height * 0.22)
}

/// Where a link between two cards meets their edges, on the line between their centres.
fn ends(from: Rectangle, to: Rectangle) -> (Point, Point) {
    let (a, b) = (from.center(), to.center());
    (edge(from, b), edge(to, a))
}

/// Where the line from `rect`'s centre towards `towards` leaves it.
fn edge(rect: Rectangle, towards: Point) -> Point {
    let centre = rect.center();
    let (dx, dy) = (towards.x - centre.x, towards.y - centre.y);
    if dx.abs() < f32::EPSILON && dy.abs() < f32::EPSILON {
        return centre;
    }
    let scale_x = if dx.abs() > f32::EPSILON {
        (rect.width / 2.0) / dx.abs()
    } else {
        f32::INFINITY
    };
    let scale_y = if dy.abs() > f32::EPSILON {
        (rect.height / 2.0) / dy.abs()
    } else {
        f32::INFINITY
    };
    let scale = scale_x.min(scale_y);
    Point::new(centre.x + dx * scale, centre.y + dy * scale)
}

fn distance_to_segment(p: Point, a: Point, b: Point) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let length = dx * dx + dy * dy;
    if length <= f32::EPSILON {
        return p.distance(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / length).clamp(0.0, 1.0);
    p.distance(Point::new(a.x + t * dx, a.y + t * dy))
}

impl canvas::Program<Message> for Board<'_> {
    type State = Drag;

    fn update(
        &self,
        state: &mut Drag,
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let p = self.projection(bounds);
        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                let point = cursor.position_in(bounds)?;
                if let Some(from) = self.handle_at(&p, point) {
                    state.grab = Some(Grab::Link { from });
                    state.cursor = Some(point);
                    return Some(canvas::Action::request_redraw().and_capture());
                }
                if let Some(id) = self.card_at(&p, point) {
                    let at = if id == LAB {
                        self.team.lab.at
                    } else {
                        self.team.agent(&id).map_or([0.0, 0.0], |a| a.at)
                    };
                    let world = p.world(point);
                    state.grab = Some(Grab::Card {
                        offset: Vector::new(at[0] - world[0], at[1] - world[1]),
                        id: id.clone(),
                        at,
                    });
                    let selected = if id == LAB {
                        Selected::Lab
                    } else {
                        Selected::Agent(id)
                    };
                    return Some(
                        canvas::Action::publish(say(AgentsMessage::Select(selected))).and_capture(),
                    );
                }
                if let Some(link) = self.link_at(&p, point) {
                    return Some(
                        canvas::Action::publish(say(AgentsMessage::Select(Selected::Link(link))))
                            .and_capture(),
                    );
                }
                let now = Instant::now();
                if let Some((then, at)) = state.last_press.take()
                    && now.duration_since(then) < DOUBLE_CLICK
                    && at.distance(point) < 6.0
                {
                    return Some(
                        canvas::Action::publish(say(AgentsMessage::Add(p.world(point))))
                            .and_capture(),
                    );
                }
                state.last_press = Some((now, point));
                state.grab = Some(Grab::Pan {
                    from: point,
                    view: self.team.view,
                });
                Some(
                    canvas::Action::publish(say(AgentsMessage::Select(Selected::Team)))
                        .and_capture(),
                )
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let point = cursor.position_from(bounds.position())?;
                match state.grab.as_mut()? {
                    Grab::Card { id, offset, at } => {
                        let world = p.world(point);
                        *at = [world[0] + offset.x, world[1] + offset.y];
                        Some(
                            canvas::Action::publish(say(AgentsMessage::Moved {
                                id: id.clone(),
                                at: *at,
                                done: false,
                            }))
                            .and_capture(),
                        )
                    }
                    Grab::Pan { from, view } => {
                        let shift = (point - *from) * (1.0 / view.zoom);
                        let moved = View {
                            at: [view.at[0] - shift.x, view.at[1] - shift.y],
                            zoom: view.zoom,
                        };
                        Some(
                            canvas::Action::publish(say(AgentsMessage::View(moved, false)))
                                .and_capture(),
                        )
                    }
                    Grab::Link { .. } => {
                        state.cursor = Some(point);
                        Some(canvas::Action::request_redraw().and_capture())
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
                let grab = state.grab.take()?;
                state.cursor = None;
                match grab {
                    Grab::Card { id, at, .. } => Some(
                        canvas::Action::publish(say(AgentsMessage::Moved { id, at, done: true }))
                            .and_capture(),
                    ),
                    Grab::Pan { .. } => Some(
                        canvas::Action::publish(say(AgentsMessage::View(self.team.view, true)))
                            .and_capture(),
                    ),
                    Grab::Link { from } => {
                        let point = cursor.position_in(bounds);
                        let to = point.and_then(|point| self.card_at(&p, point));
                        match to {
                            Some(to) if to != from => Some(
                                canvas::Action::publish(say(AgentsMessage::Connect(from, to)))
                                    .and_capture(),
                            ),
                            _ => Some(canvas::Action::request_redraw().and_capture()),
                        }
                    }
                }
            }
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let point = cursor.position_in(bounds)?;
                let lines = match delta {
                    ScrollDelta::Lines { y, .. } => *y,
                    ScrollDelta::Pixels { y, .. } => *y / 40.0,
                };
                let view = self.team.view;
                let zoom = (view.zoom * 1.1_f32.powf(lines)).clamp(ZOOM.0, ZOOM.1);
                // The point under the cursor stays there.
                let world = p.world(point);
                let at = [
                    world[0] - (point.x - bounds.width / 2.0) / zoom,
                    world[1] - (point.y - bounds.height / 2.0) / zoom,
                ];
                Some(
                    canvas::Action::publish(say(AgentsMessage::View(View { at, zoom }, true)))
                        .and_capture(),
                )
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Drag,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let p = self.projection(bounds);
        let zoom = p.view.zoom;
        grid(&mut frame, &p);

        let rects = self.rects(&p);
        let rect = |id: &str| rects.iter().find(|(r, _)| *r == id).map(|(_, rect)| *rect);
        for (i, link) in self.team.links.iter().enumerate() {
            let (Some(from), Some(to)) = (rect(&link.from), rect(&link.to)) else {
                continue;
            };
            let selected = *self.selected == Selected::Link(i);
            draw_link(&mut frame, from, to, link.kind, selected, zoom);
        }
        for agent in &self.team.agents {
            let rect = p.card(agent.at, CARD);
            let live = self.cards.and_then(|cards| cards.get(&agent.id));
            let selected = *self.selected == Selected::Agent(agent.id.clone());
            let flagged = self.flagged.contains(&agent.id);
            let lab = self.team.uses_lab(&agent.id);
            draw_card(&mut frame, rect, agent, live, selected, flagged, lab, zoom);
        }
        draw_lab(
            &mut frame,
            p.card(self.team.lab.at, LAB_CARD),
            &self.team.lab.data,
            self.lab_calls,
            *self.selected == Selected::Lab,
            zoom,
        );
        if let (Some(Grab::Link { from }), Some(cursor)) = (&state.grab, state.cursor)
            && let Some(from) = rect(from)
        {
            frame.stroke(
                &Path::line(handle(from), cursor),
                Stroke::default().with_color(theme::ACCENT).with_width(1.5),
            );
        }
        if self.team.agents.is_empty() {
            label(
                &mut frame,
                "Double-click to add an agent, or start from a starter team on the left.",
                Point::new(bounds.width / 2.0, bounds.height / 2.0 - 60.0),
                13.0,
                theme::TEXT_MUTED,
                text::Alignment::Center,
            );
        }
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Drag,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.grab.is_some() {
            return mouse::Interaction::Grabbing;
        }
        let Some(point) = cursor.position_in(bounds) else {
            return mouse::Interaction::default();
        };
        let p = self.projection(bounds);
        if self.handle_at(&p, point).is_some() {
            mouse::Interaction::Crosshair
        } else if self.card_at(&p, point).is_some() || self.link_at(&p, point).is_some() {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::Grab
        }
    }
}

fn grid(frame: &mut Frame, p: &Projection) {
    let step = GRID * p.view.zoom;
    if step < 12.0 {
        return;
    }
    let origin = p.screen([0.0, 0.0]);
    let first_x = origin.x.rem_euclid(step);
    let first_y = origin.y.rem_euclid(step);
    let stroke = Stroke::default().with_color(theme::GRID).with_width(1.0);
    let mut x = first_x;
    while x < p.size.width {
        frame.stroke(
            &Path::line(Point::new(x, 0.0), Point::new(x, p.size.height)),
            stroke,
        );
        x += step;
    }
    let mut y = first_y;
    while y < p.size.height {
        frame.stroke(
            &Path::line(Point::new(0.0, y), Point::new(p.size.width, y)),
            stroke,
        );
        y += step;
    }
}

fn draw_link(
    frame: &mut Frame,
    from: Rectangle,
    to: Rectangle,
    kind: LinkKind,
    selected: bool,
    zoom: f32,
) {
    let (a, b) = ends(from, to);
    let (color, width, dash): (Color, f32, &[f32]) = match kind {
        LinkKind::ReportsTo => (theme::TEXT, 1.6, &[]),
        LinkKind::HandsTo => (theme::TEXT, 1.6, &[7.0, 5.0]),
        LinkKind::Reviews { .. } => (AMBER, 1.6, &[2.0, 4.0]),
        LinkKind::Uses => (theme::TEXT_MUTED, 1.0, &[]),
    };
    let color = if selected { theme::ACCENT } else { color };
    let width = if selected { width + 1.2 } else { width };
    frame.stroke(
        &Path::line(a, b),
        Stroke {
            line_dash: LineDash {
                segments: dash,
                offset: 0,
            },
            ..Stroke::default().with_color(color).with_width(width)
        },
    );
    if kind != LinkKind::Uses {
        arrow(frame, a, b, color, 9.0 * zoom.clamp(0.6, 1.4));
    }
    if let LinkKind::Reviews { rounds } = kind {
        let middle = Point::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        label(
            frame,
            &format!("×{rounds}"),
            Point::new(middle.x + 6.0, middle.y - 8.0),
            11.0 * zoom.clamp(0.8, 1.3),
            color,
            text::Alignment::Left,
        );
    }
}

fn arrow(frame: &mut Frame, from: Point, to: Point, color: Color, size: f32) {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let length = (dx * dx + dy * dy).sqrt();
    if length < 1.0 {
        return;
    }
    let (ux, uy) = (dx / length, dy / length);
    let back = Point::new(to.x - ux * size, to.y - uy * size);
    let left = Point::new(back.x - uy * size * 0.5, back.y + ux * size * 0.5);
    let right = Point::new(back.x + uy * size * 0.5, back.y - ux * size * 0.5);
    let head = Path::new(|path| {
        path.move_to(to);
        path.line_to(left);
        path.line_to(right);
        path.close();
    });
    frame.fill(&head, color);
}

#[allow(clippy::too_many_arguments)]
fn draw_card(
    frame: &mut Frame,
    rect: Rectangle,
    agent: &persephone_agents::team::Agent,
    live: Option<&CardLive>,
    selected: bool,
    flagged: bool,
    lab: bool,
    zoom: f32,
) {
    let radius = 6.0 * zoom;
    let body = Path::rounded_rectangle(rect.position(), rect.size(), radius.into());
    frame.fill(&body, CARD_FILL);
    let border = if selected {
        theme::ACCENT
    } else if flagged {
        theme::DOWN
    } else {
        theme::AXIS_LINE
    };
    frame.stroke(
        &body,
        Stroke::default()
            .with_color(border)
            .with_width(if selected { 2.0 } else { 1.0 }),
    );
    let status = live.and_then(|l| l.status.as_ref());
    if let Some(status) = status {
        let bar = Path::rounded_rectangle(
            rect.position(),
            Size::new(4.0 * zoom, rect.height),
            radius.into(),
        );
        frame.fill(&bar, status_color(status));
    }
    let size = |s: f32| s * zoom;
    let left = rect.x + 12.0 * zoom;
    let room = rect.width - 24.0 * zoom;
    let line = |n: f32| rect.y + (14.0 + n * 18.0) * zoom;
    label(
        frame,
        &fit(&agent.name, room, size(13.0)),
        Point::new(left, line(0.0)),
        size(13.0),
        theme::TEXT_BRIGHT,
        text::Alignment::Left,
    );
    let mut what = format!("{} · {}", agent.model.label(), agent.effort.label());
    if lab {
        what.push_str(" · Lab");
    }
    label(
        frame,
        &fit(&what, room, size(11.0)),
        Point::new(left, line(1.0)),
        size(11.0),
        theme::TEXT_MUTED,
        text::Alignment::Left,
    );
    if zoom >= 0.55 {
        let role = if agent.role.trim().is_empty() {
            "No role yet".to_owned()
        } else {
            first_line(&agent.role).to_owned()
        };
        label(
            frame,
            &fit(&role, room, size(11.0)),
            Point::new(left, line(2.0)),
            size(11.0),
            theme::TEXT,
            text::Alignment::Left,
        );
    }
    let (said, color) = match (status, live) {
        (Some(status), Some(live)) => {
            let mut said = match status {
                Status::Running | Status::WrappingUp if !live.doing.is_empty() => {
                    live.doing.clone()
                }
                _ => status.label(),
            };
            said = format!("{said} · {}/{}", live.calls, agent.budget);
            if live.spend > 0.0 {
                said.push_str(&format!(" · ${:.2}", live.spend));
            }
            (said, status_color(status))
        }
        _ => (format!("{} calls", agent.budget), theme::TEXT_MUTED),
    };
    label(
        frame,
        &fit(&said, room, size(11.0)),
        Point::new(left, line(3.0)),
        size(11.0),
        color,
        text::Alignment::Left,
    );
    // The link handle.
    let dot = Path::circle(handle(rect), HANDLE * zoom.clamp(0.7, 1.2));
    frame.fill(
        &dot,
        if selected {
            theme::ACCENT
        } else {
            theme::LABEL_BACKGROUND
        },
    );
    frame.stroke(
        &dot,
        Stroke::default()
            .with_color(theme::TEXT_MUTED)
            .with_width(1.0),
    );
}

fn draw_lab(
    frame: &mut Frame,
    rect: Rectangle,
    data: &LabData,
    calls: u32,
    selected: bool,
    zoom: f32,
) {
    let body = Path::rounded_rectangle(rect.position(), rect.size(), (rect.height / 2.0).into());
    frame.fill(&body, LAB_FILL);
    frame.stroke(
        &body,
        Stroke::default()
            .with_color(if selected { theme::ACCENT } else { theme::UP })
            .with_width(if selected { 2.0 } else { 1.0 }),
    );
    let centre = rect.center();
    label(
        frame,
        "Lab",
        Point::new(centre.x, centre.y - 9.0 * zoom),
        13.0 * zoom,
        theme::TEXT_BRIGHT,
        text::Alignment::Center,
    );
    let data = match data {
        LabData::Own(_) => "its own data",
        LabData::App => "your app's data",
    };
    let said = if calls > 0 {
        format!("{data} · {calls} calls")
    } else {
        data.to_owned()
    };
    label(
        frame,
        &said,
        Point::new(centre.x, centre.y + 9.0 * zoom),
        11.0 * zoom,
        theme::TEXT_MUTED,
        text::Alignment::Center,
    );
}

fn label(
    frame: &mut Frame,
    said: &str,
    at: Point,
    size: f32,
    color: Color,
    align: text::Alignment,
) {
    if size < 5.0 {
        return;
    }
    frame.fill_text(Text {
        content: said.to_owned(),
        position: at,
        color,
        size: size.into(),
        align_x: align,
        align_y: alignment::Vertical::Center,
        ..Text::default()
    });
}

/// As much of `said` as fits in `width` at text `size`, with an ellipsis if cut.
fn fit(said: &str, width: f32, size: f32) -> String {
    let most = (width / (size * 0.56)).max(1.0) as usize;
    if said.chars().count() <= most {
        return said.to_owned();
    }
    let mut cut: String = said.chars().take(most.saturating_sub(1)).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_meet_cards_at_their_edges() {
        let a = Rectangle::new(Point::new(0.0, 0.0), Size::new(100.0, 50.0));
        let b = Rectangle::new(Point::new(300.0, 0.0), Size::new(100.0, 50.0));
        let (from, to) = ends(a, b);
        assert_eq!(from, Point::new(100.0, 25.0));
        assert_eq!(to, Point::new(300.0, 25.0));
        let below = Rectangle::new(Point::new(0.0, 200.0), Size::new(100.0, 50.0));
        let (from, to) = ends(a, below);
        assert_eq!(from, Point::new(50.0, 50.0));
        assert_eq!(to, Point::new(50.0, 200.0));
    }

    #[test]
    fn a_click_near_a_line_finds_it() {
        let (a, b) = (Point::new(0.0, 0.0), Point::new(100.0, 0.0));
        assert!(distance_to_segment(Point::new(50.0, 4.0), a, b) <= LINK_REACH);
        assert!(distance_to_segment(Point::new(150.0, 0.0), a, b) > LINK_REACH);
    }

    #[test]
    fn the_projection_goes_both_ways() {
        let p = Projection {
            view: View {
                at: [100.0, -50.0],
                zoom: 1.5,
            },
            size: Size::new(800.0, 600.0),
        };
        let at = [30.0, 70.0];
        let back = p.world(p.screen(at));
        assert!((back[0] - at[0]).abs() < 1e-3 && (back[1] - at[1]).abs() < 1e-3);
        assert_eq!(p.screen([100.0, -50.0]), Point::new(400.0, 300.0));
    }

    #[test]
    fn long_text_is_cut_to_fit() {
        assert_eq!(fit("short", 200.0, 11.0), "short");
        let cut = fit(&"x".repeat(100), 60.0, 10.0);
        assert!(cut.ends_with('…') && cut.chars().count() <= 11, "{cut}");
    }
}
