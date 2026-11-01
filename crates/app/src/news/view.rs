//! How the news sidebar looks: a heading saying whether it's live or as of the playhead, filter
//! chips, and the headlines newest first, each a click from its time on the chart.

use iced::{
    Alignment, Background, Border, Color, Element, Fill, Point, Rectangle, Renderer, Size, Theme,
    mouse,
    widget::{Space, button, canvas, column, container, row, scrollable, text},
};
use persephone_chart::theme;
use persephone_core::{UnixNanos, time::DateTime};
use persephone_data::news::{Headline, Importance};

use super::{Filter, History, News, NewsMessage};
use crate::Message;

/// The list's id, for scrolling it to a headline.
#[must_use]
pub fn list_id() -> iced::widget::Id {
    iced::widget::Id::new("news-list")
}

/// The most headlines listed at once.
const MOST_ROWS: usize = 300;
const SMALL: f32 = 12.0;
const HINT: f32 = 11.0;
const NANOS_PER_MINUTE: u64 = 60_000_000_000;
const HOVER: Color = Color::from_rgb8(0x2a, 0x2e, 0x39);
const BORDER: Color = Color::from_rgb8(0x36, 0x3a, 0x45);
/// Medium importance: amber.
const AMBER: Color = Color::from_rgb8(0xff, 0xa7, 0x26);

fn say(message: NewsMessage) -> Message {
    Message::News(message)
}

fn chip(label: &str, on: bool, message: NewsMessage) -> Element<'static, Message> {
    button(text(label.to_owned()).size(HINT).center())
        .padding([2, 7])
        .on_press(say(message))
        .style(move |_, status| {
            let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(Background::Color(if on {
                    theme::ACCENT
                } else if hot {
                    HOVER
                } else {
                    Color::TRANSPARENT
                })),
                text_color: if on { Color::WHITE } else { theme::TEXT_BRIGHT },
                border: Border {
                    color: if on { theme::ACCENT } else { BORDER },
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..button::Style::default()
            }
        })
        .into()
}

/// How long ago `at` was, from `now`: `just now`, `12 min ago`, `3 h ago`, else the date.
fn ago(at: UnixNanos, now: UnixNanos) -> String {
    let minutes = now.saturating_sub(at) / NANOS_PER_MINUTE;
    match minutes {
        0 => "just now".into(),
        1..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", minutes / 60),
        _ => DateTime::from_nanos(at).date(),
    }
}

fn when(at: UnixNanos) -> String {
    let at = DateTime::from_nanos(at);
    format!("{} {}", at.date(), &at.time()[..5])
}

/// The headlines shown: up to `at` while replaying, else all; filtered; newest first.
#[must_use]
pub fn shown<'a>(news: &'a News, at: Option<UnixNanos>, words: &[&str]) -> Vec<&'a Headline> {
    news.store
        .until(at.unwrap_or(UnixNanos::MAX), usize::MAX)
        .into_iter()
        .filter(|headline| match news.saved.filter {
            Filter::All => true,
            Filter::Market => headline.names_market(words),
            Filter::Macro => headline.is_macro(),
        })
        .take(MOST_ROWS)
        .collect()
}

/// The sidebar. `replay` is the playhead's time while a backtest plays back, `words` what the
/// focused chart's market is called, and `now` the time on the clock.
pub fn sidebar<'a>(
    news: &'a News,
    replay: Option<UnixNanos>,
    words: &[&str],
    market: &str,
    now: UnixNanos,
    span: Option<(UnixNanos, UnixNanos)>,
) -> Element<'a, Message> {
    let filter = news.saved.filter;
    let status = match (replay, news.fetched_at) {
        (Some(at), _) => format!("As of {} (the playhead)", when(at)),
        (None, Some(fetched)) => format!("Latest, read {}", ago(fetched, now)),
        (None, None) => "Latest".into(),
    };
    let heading = row![
        text("News").size(SMALL).color(theme::TEXT_BRIGHT),
        Space::new().width(Fill),
        chip("⟳", false, NewsMessage::Refresh),
        chip("✕", false, NewsMessage::Toggle),
    ]
    .spacing(4)
    .align_y(Alignment::Center);
    let filters = row![
        chip(
            "All",
            filter == Filter::All,
            NewsMessage::Filter(Filter::All)
        ),
        chip(
            market,
            filter == Filter::Market,
            NewsMessage::Filter(Filter::Market)
        ),
        chip(
            "Macro",
            filter == Filter::Macro,
            NewsMessage::Filter(Filter::Macro)
        ),
    ]
    .spacing(3);
    let mut list = column![].spacing(2);
    let headlines = shown(news, replay, words);
    for headline in &headlines {
        let importance = headline.importance(words);
        let mark = match importance {
            Importance::High => theme::DOWN,
            Importance::Medium => AMBER,
            Importance::Low => BORDER,
        };
        let time = if replay.is_some() {
            when(headline.published)
        } else {
            ago(headline.published, now)
        };
        let lit = news.lit.as_deref() == Some(headline.id.as_str());
        // Off the focused chart's bars: a click can't show it there, so it's said and dimmed.
        let off_chart =
            span.is_some_and(|(from, to)| headline.published < from || headline.published >= to);
        let off: Element<'a, Message> = if off_chart {
            text("not on this chart")
                .size(HINT)
                .color(theme::TEXT_MUTED)
                .into()
        } else {
            Space::new().width(0).into()
        };
        let body = column![
            row![
                text(time).size(HINT).color(theme::TEXT_MUTED),
                off,
                text(headline.source.clone())
                    .size(HINT)
                    .color(theme::TEXT_MUTED)
                    .width(Fill),
                button(text("↗").size(HINT))
                    .padding([0, 4])
                    .on_press(say(NewsMessage::Open(headline.link.clone())))
                    .style(|_, status| button::Style {
                        background: matches!(status, button::Status::Hovered)
                            .then_some(Background::Color(HOVER)),
                        text_color: theme::TEXT_MUTED,
                        ..button::Style::default()
                    }),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
            text(headline.title.clone())
                .size(SMALL)
                .color(if off_chart {
                    theme::TEXT_MUTED
                } else if importance == Importance::High {
                    theme::TEXT_BRIGHT
                } else {
                    theme::TEXT
                })
                .width(Fill),
        ]
        .spacing(1);
        // A row's height follows its title, so the bar's is set: two lines, the most titles take.
        let row = row![
            container(Space::new().width(3).height(32)).style(move |_| container::background(mark)),
            body
        ]
        .spacing(6)
        .align_y(Alignment::Start);
        list = list.push(
            button(row)
                .padding([4, 6])
                .width(Fill)
                .on_press(say(NewsMessage::Pick(headline.id.clone())))
                .style(move |_, status| button::Style {
                    background: (lit
                        || matches!(status, button::Status::Hovered | button::Status::Pressed))
                    .then_some(Background::Color(HOVER)),
                    text_color: theme::TEXT,
                    border: Border {
                        color: if lit {
                            theme::ACCENT
                        } else {
                            Color::TRANSPARENT
                        },
                        width: 1.0,
                        radius: 3.0.into(),
                    },
                    ..button::Style::default()
                }),
        );
    }
    // Older headlines, from GDELT's archive, for the focused chart's bars.
    let history: Element<'a, Message> = match &news.history {
        History::Running {
            done, total, added, ..
        } => row![
            text(format!(
                "Reading older headlines: {done} of {total} searches, {added} found…"
            ))
            .size(HINT)
            .color(theme::TEXT_MUTED)
            .width(Fill),
            chip("Stop", false, NewsMessage::StopHistory),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
        History::Failed(error) => row![
            text(error.clone())
                .size(HINT)
                .color(theme::DOWN)
                .width(Fill),
            chip("Try again", false, NewsMessage::FetchHistory),
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into(),
        idle => {
            let said = match idle {
                History::Finished {
                    nothing_left: true, ..
                } => "This chart's span has been searched already.".to_owned(),
                History::Finished { added, .. } => {
                    format!("Added {added} older headlines from GDELT.")
                }
                _ => "Older headlines for this chart's bars, back to 2017, from GDELT.".to_owned(),
            };
            row![
                chip("Older headlines", false, NewsMessage::FetchHistory),
                text(said).size(HINT).color(theme::TEXT_MUTED).width(Fill),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
            .into()
        }
    };
    // Why the list is short or empty, plainly.
    let mut notes = column![].spacing(2);
    if let Some(note) = &news.note {
        notes = notes.push(text(note.clone()).size(HINT).color(theme::TEXT_MUTED));
    }
    if headlines.is_empty() {
        let said = match (news.store.since(), replay) {
            (None, _) if news.root.is_none() => "Headlines aren't kept on this system.".to_owned(),
            (None, _) => "Nothing yet: the feeds are read every 5 minutes.".to_owned(),
            (Some(since), Some(_)) => format!(
                "Nothing by the playhead. Headlines are only known from {}: feeds carry their last few dozen items, and Older headlines fetches earlier ones.",
                when(since)
            ),
            (Some(_), None) => "Nothing matches this filter.".to_owned(),
        };
        notes = notes.push(text(said).size(HINT).color(theme::TEXT_MUTED));
    }
    if !news.failures.is_empty() {
        notes = notes.push(
            text(format!(
                "{} feed{} couldn't be read: {}",
                news.failures.len(),
                if news.failures.len() == 1 { "" } else { "s" },
                news.failures.join("; ")
            ))
            .size(HINT)
            .color(theme::DOWN),
        );
    }
    let collecting = news.saved.collecting;
    let footer = row![
        chip(
            "Keep collecting",
            collecting,
            NewsMessage::Collecting(!collecting)
        ),
        text("Times are when each was published.")
            .size(HINT)
            .color(theme::TEXT_MUTED),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    let content = column![
        heading,
        text(status).size(HINT).color(theme::TEXT_MUTED),
        filters,
        history,
        notes,
        scrollable(list).id(list_id()).height(Fill),
        footer,
    ]
    .spacing(6)
    .padding([8, 10]);
    row![
        canvas(WidthEdge).width(5).height(Fill),
        container(content)
            .width(Fill)
            .height(Fill)
            .style(|_| container::background(theme::BACKGROUND)),
    ]
    .into()
}

/// The sidebar's left edge, dragged to make it wider or narrower.
struct WidthEdge;

#[derive(Default)]
struct Dragging(bool);

impl canvas::Program<Message> for WidthEdge {
    type State = Dragging;

    fn update(
        &self,
        state: &mut Dragging,
        event: &canvas::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        match event {
            canvas::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
                cursor.position_in(bounds)?;
                state.0 = true;
                Some(canvas::Action::capture())
            }
            // The window's own x, as the cursor's moved in it.
            canvas::Event::Mouse(mouse::Event::CursorMoved { position }) if state.0 => {
                Some(canvas::Action::publish(say(NewsMessage::Width(position.x))).and_capture())
            }
            canvas::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) if state.0 => {
                state.0 = false;
                Some(canvas::Action::capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Dragging,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let hot = state.0 || cursor.position_in(bounds).is_some();
        frame.fill_rectangle(
            Point::ORIGIN,
            Size::new(bounds.width, bounds.height),
            theme::BACKGROUND,
        );
        frame.fill_rectangle(
            Point::new(0.0, 0.0),
            Size::new(1.0, bounds.height),
            if hot { theme::ACCENT } else { theme::AXIS_LINE },
        );
        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        state: &Dragging,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if state.0 || cursor.position_in(bounds).is_some() {
            mouse::Interaction::ResizingHorizontally
        } else {
            mouse::Interaction::default()
        }
    }
}
