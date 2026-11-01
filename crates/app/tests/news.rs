//! The news sidebar, driven through the messages the UI sends. Headlines are handed in as the feed
//! reader would hand them over; nothing here touches the network.

use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

use iced::{Point, Size};
use persephone::{
    Message, Persephone,
    backtest::PanelMessage,
    keymap::Action,
    menu::{MenuMessage, Page},
    news::{Filter, NewsMessage, view::shown},
};
use persephone_chart::{ChartData, ChartMessage};
use persephone_core::{Candle, Timeframe, UnixNanos, strategy::BuiltInStrategy};
use persephone_data::news::Headline;

const HOUR: u64 = 3_600_000_000_000;

fn app() -> Persephone {
    let candles = (0..1_500u64)
        .map(|i| {
            let p = 50_000.0 + 1_500.0 * (i as f64 / 45.0).sin();
            Candle {
                ts: i * Timeframe::H1.nanos(),
                open: p - 5.0,
                high: p + 25.0,
                low: p - 25.0,
                close: p + 5.0,
                volume: 3.0,
            }
        })
        .collect();
    Persephone::preloaded(
        ChartData::new("BTCUSDT", "BINANCE", Timeframe::H1, 2, candles),
        Size::new(1400.0, 900.0),
    )
}

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("persephone-news-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn headline(hours: u64, title: &str) -> Headline {
    Headline {
        published: hours * HOUR + HOUR / 2,
        seen: hours * HOUR + HOUR / 2,
        source: "Wire".into(),
        title: title.into(),
        link: format!("https://example.com/{hours}"),
        id: format!("h{hours}"),
        official: false,
    }
}

fn news(app: &mut Persephone, message: NewsMessage) {
    let _ = app.update(Message::News(message));
}

fn settle(app: &mut Persephone) {
    let _ = app.update(Message::Tick(Instant::now() + Duration::from_secs(3)));
}

/// Headlines every 100 hours along the chart, the market's and the Fed's by turns.
fn with_headlines(app: &mut Persephone, root: PathBuf) {
    app.use_news_root(root);
    let found: Vec<Headline> = (1..14)
        .map(|n| {
            if n % 2 == 0 {
                headline(n * 100, &format!("Bitcoin moves, part {n}"))
            } else {
                headline(n * 100, &format!("Fed holds rates, part {n}"))
            }
        })
        .collect();
    news(app, NewsMessage::Fetched(Arc::new(found), Vec::new()));
}

fn shows(app: &Persephone, words: &str) -> bool {
    let mut ui = iced_test::Simulator::with_size(
        iced::Settings::default(),
        Size::new(1400.0, 900.0),
        app.view(),
    );
    ui.find(words).is_ok()
}

#[test]
fn the_menu_opens_a_sidebar_that_the_charts_make_room_for() {
    let mut app = app();
    settle(&mut app);
    let full = app.tile_rect(0).unwrap();
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::RightClicked {
            at: Point::new(200.0, 200.0),
        },
    ));
    assert!(shows(&app, "News"), "in the right-click menu");
    let _ = app.update(Message::Menu(MenuMessage::News));
    assert!(app.news().saved.open);
    settle(&mut app);
    let narrower = app.tile_rect(0).unwrap();
    let width = app.news().width(1400.0);
    assert_eq!(width, 300.0);
    assert!(
        (full.width - narrower.width - width).abs() < 1.0,
        "{full:?} {narrower:?}"
    );
    assert!(shows(&app, "Keep collecting"));

    // Its edge drags, within limits, and Ctrl+N closes it.
    news(&mut app, NewsMessage::Width(1400.0 - 450.0));
    assert_eq!(app.news().width(1400.0), 450.0);
    news(&mut app, NewsMessage::Width(1390.0));
    assert_eq!(app.news().width(1400.0), 220.0);
    news(&mut app, NewsMessage::Width(0.0));
    assert_eq!(app.news().width(1400.0), 520.0);
    let _ = app.update(Message::Key(Action::News));
    assert!(!app.news().saved.open);
    settle(&mut app);
    assert_eq!(app.tile_rect(0).unwrap().width, full.width);
}

#[test]
fn it_is_saved_with_the_layout() {
    let dir = dir("saved");
    let path = dir.join("layout.ron");
    let (mut first, _) = Persephone::start(&[], Some(path.clone()));
    let _ = first.update(Message::News(NewsMessage::Toggle));
    let _ = first.update(Message::News(NewsMessage::Width(1400.0 - 360.0)));
    let _ = first.update(Message::News(NewsMessage::Filter(Filter::Macro)));
    let (restored, _) = Persephone::start(&[], Some(path));
    let saved = &restored.news().saved;
    assert!(saved.open);
    assert_eq!(saved.filter, Filter::Macro);
    assert_eq!(saved, &first.news().saved, "as it was, width and all");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn replaying_shows_only_what_was_out_by_the_playhead_and_a_click_moves_it() {
    let mut app = app();
    let root = dir("replay");
    with_headlines(&mut app, root.clone());
    // Kept on disk, so a history builds.
    assert!(root.join("news").join("headlines.jsonl").exists());
    news(&mut app, NewsMessage::Toggle);

    // Live: everything, newest first.
    let live = shown(app.news(), None, &["BTC", "Bitcoin"]);
    assert_eq!(live.len(), 13);
    assert_eq!(live[0].id, "h1300");

    // Replaying a backtest: nothing from after the playhead.
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::RightClicked {
            at: Point::new(200.0, 200.0),
        },
    ));
    let _ = app.update(Message::Menu(MenuMessage::Open(Page::Strategies)));
    let _ = app.update(Message::Menu(MenuMessage::SetStrategy(
        BuiltInStrategy::EmaCross,
    )));
    let (generation, request) = app.backtest_request(0).unwrap();
    let report = persephone_engine::run_backtest(&request)
        .map(Arc::new)
        .map_err(|e| e.to_string());
    let _ = app.update(Message::BacktestDone(0, generation, report));
    let _ = app.update(Message::Key(Action::Backtest));
    let panel = |app: &mut Persephone, message| {
        let _ = app.update(Message::Backtest(message));
    };
    panel(&mut app, PanelMessage::Seek(650 * HOUR));
    let at: UnixNanos = app.panel().unwrap().playback.at.unwrap();
    let replay = shown(app.news(), Some(at), &["BTC", "Bitcoin"]);
    assert!(
        replay.iter().all(|h| h.published <= at),
        "nothing from later"
    );
    assert_eq!(replay[0].id, "h600");
    let at_time = persephone_core::time::DateTime::from_nanos(at);
    assert!(shows(
        &app,
        &format!(
            "As of {} {} (the playhead)",
            at_time.date(),
            &at_time.time()[..5]
        )
    ));
    // Stepping on past the next one brings it in.
    panel(&mut app, PanelMessage::Seek(705 * HOUR));
    let at = app.panel().unwrap().playback.at.unwrap();
    assert_eq!(shown(app.news(), Some(at), &[])[0].id, "h700");

    // Clicking an earlier headline takes the playhead there.
    news(&mut app, NewsMessage::Pick("h300".into()));
    let at = app.panel().unwrap().playback.at.unwrap();
    assert!(at.abs_diff(300 * HOUR + HOUR / 2) <= HOUR, "{at}");
    assert_eq!(app.news().lit.as_deref(), Some("h300"));

    // The chips filter: the market's, or the economy's.
    news(&mut app, NewsMessage::Filter(Filter::Market));
    let market = shown(app.news(), None, &["BTC", "Bitcoin"]);
    assert!(market.iter().all(|h| h.title.starts_with("Bitcoin")) && market.len() == 6);
    news(&mut app, NewsMessage::Filter(Filter::Macro));
    let macro_ = shown(app.news(), None, &["BTC", "Bitcoin"]);
    assert!(macro_.iter().all(|h| h.title.starts_with("Fed")) && macro_.len() == 7);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_empty_history_says_why() {
    let mut app = app();
    let root = dir("empty");
    app.use_news_root(root.clone());
    news(&mut app, NewsMessage::Toggle);
    assert!(shows(
        &app,
        "Nothing yet: the feeds are read every 5 minutes."
    ));
    news(
        &mut app,
        NewsMessage::Fetched(
            Arc::new(Vec::new()),
            vec!["https://example.com/rss: timed out".into()],
        ),
    );
    assert!(shows(
        &app,
        "1 feed couldn't be read: https://example.com/rss: timed out"
    ));
    std::fs::remove_dir_all(root).unwrap();
}
