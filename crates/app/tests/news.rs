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
    // The chart is a pane of its own now, 6 px from the window's edge and from the sidebar.
    assert!(
        (full.width - narrower.width - width - 12.0).abs() < 1.0,
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
    // And the chart being played back shows that bar in its middle, its mark lit, even once
    // time has moved on.
    settle(&mut app);
    let chart = app.tile_chart(0).unwrap();
    assert_eq!(chart.lit_event(), Some("h300"));
    let width = app.tile_rect(0).unwrap().width - persephone_chart::PRICE_AXIS_WIDTH;
    assert!(
        (chart.middle_bar(width) - 300.0).abs() < 1.5,
        "{}",
        chart.middle_bar(width)
    );

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

#[test]
fn charts_mark_the_headlines_that_matter_and_a_click_on_one_lights_it() {
    let mut app = app();
    let root = dir("marks");
    with_headlines(&mut app, root.clone());
    let marks = |app: &Persephone| app.tile_chart(0).unwrap().events().len();
    assert_eq!(marks(&app), 0, "none while the sidebar's closed");
    news(&mut app, NewsMessage::Toggle);
    // Every Fed headline is High, and every Bitcoin one names this market.
    assert_eq!(marks(&app), 13);
    let chart = app.tile_chart(0).unwrap();
    let first = &chart.events()[0];
    assert!(first.high && first.title.starts_with("Fed"));
    assert_eq!(
        chart.bar_of_time(first.ts),
        Some(100),
        "on the bar it came out in"
    );
    news(&mut app, NewsMessage::Filter(Filter::Market));
    assert_eq!(marks(&app), 6);
    assert!(app.tile_chart(0).unwrap().events().iter().all(|e| !e.high));

    let _ = app.update(Message::Chart(
        0,
        ChartMessage::EventClicked { id: "h400".into() },
    ));
    assert_eq!(app.news().lit.as_deref(), Some("h400"));

    news(&mut app, NewsMessage::Toggle);
    assert_eq!(marks(&app), 0, "and gone when it closes");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn nothing_is_fetched_until_the_sidebar_is_first_opened() {
    let mut app = app();
    let root = dir("unused");
    app.use_news_root(root.clone());
    assert!(!app.news().polling(), "never opened: no feeds read");
    news(&mut app, NewsMessage::Toggle);
    assert!(app.news().polling());
    news(&mut app, NewsMessage::Toggle);
    assert!(app.news().polling(), "collecting on once it's been used");
    news(&mut app, NewsMessage::Collecting(false));
    assert!(!app.news().polling());
    news(&mut app, NewsMessage::Toggle);
    news(&mut app, NewsMessage::Toggle);
    assert!(!app.news().polling(), "and left off as set");
    std::fs::remove_dir_all(root).unwrap();
}

/// 500 hourly bars from 2024-01-01, so GDELT's archive covers them.
fn app_in_2024() -> Persephone {
    const START: u64 = 1_704_067_200_000_000_000;
    let candles = (0..500u64)
        .map(|i| {
            let p = 40_000.0 + 500.0 * (i as f64 / 30.0).sin();
            Candle {
                ts: START + i * HOUR,
                open: p,
                high: p + 20.0,
                low: p - 20.0,
                close: p,
                volume: 1.0,
            }
        })
        .collect();
    Persephone::preloaded(
        ChartData::new("BTCUSDT", "BINANCE", Timeframe::H1, 2, candles),
        Size::new(1400.0, 900.0),
    )
}

#[test]
fn picking_any_headline_centres_its_mark_and_lights_it() {
    let mut app = app();
    let root = dir("pick");
    app.use_news_root(root.clone());
    // One the chart wouldn't mark on its own: neither market-moving nor naming the market.
    let quiet = headline(700, "A gallery opens downtown");
    news(
        &mut app,
        NewsMessage::Fetched(Arc::new(vec![quiet.clone()]), Vec::new()),
    );
    news(&mut app, NewsMessage::Toggle);
    settle(&mut app);
    assert!(app.tile_chart(0).unwrap().events().is_empty());
    news(&mut app, NewsMessage::Pick(quiet.id.clone()));
    let chart = app.tile_chart(0).unwrap();
    assert_eq!(
        chart.events().len(),
        1,
        "picked, it's marked whatever the filter"
    );
    assert_eq!(chart.lit_event(), Some(quiet.id.as_str()));
    let width = app.tile_rect(0).unwrap().width - persephone_chart::PRICE_AXIS_WIDTH;
    let bar = chart.bar_of_time(quiet.published).unwrap();
    assert_eq!(bar, 700);
    assert!(
        (chart.middle_bar(width) - bar as f64).abs() < 1.0,
        "in the middle: {} against {bar}",
        chart.middle_bar(width)
    );
    assert_eq!(app.news().note, None);

    // A click on a mark lights it on the chart too.
    let other = headline(300, "Fed holds rates");
    news(
        &mut app,
        NewsMessage::Fetched(Arc::new(vec![other.clone()]), Vec::new()),
    );
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::EventClicked {
            id: other.id.clone(),
        },
    ));
    assert_eq!(app.tile_chart(0).unwrap().lit_event(), Some("h300"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_headline_from_before_the_chart_says_so() {
    let mut app = app_in_2024();
    let root = dir("before");
    app.use_news_root(root.clone());
    let early = Headline {
        published: 1_600_000_000_000_000_000,
        ..headline(0, "Bitcoin in 2020")
    };
    news(
        &mut app,
        NewsMessage::Fetched(Arc::new(vec![early.clone()]), Vec::new()),
    );
    news(&mut app, NewsMessage::Toggle);
    news(&mut app, NewsMessage::Pick(early.id));
    let note = app.news().note.clone().unwrap();
    assert!(
        note.contains("before this chart's first bar, 2024-01-01"),
        "{note}"
    );
    assert!(shows(&app, &note));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn older_headlines_are_searched_for_over_the_charts_bars_once() {
    use persephone::news::History;
    let mut app = app_in_2024();
    let root = dir("history");
    app.use_news_root(root.clone());
    news(&mut app, NewsMessage::Toggle);
    assert!(shows(&app, "Older headlines"));
    news(&mut app, NewsMessage::FetchHistory);
    // 500 hours is three fortnights on GDELT's grid here: the market's and the economy's each.
    let History::Running { total, .. } = app.news().history else {
        panic!("{:?}", app.news().history);
    };
    assert_eq!(total, 6);
    assert!(shows(
        &app,
        "Reading older headlines: 0 of 6 searches, 0 found…"
    ));
    // What a search would bring back, as the reader hands it over.
    let found = Headline {
        published: 1_704_067_200_000_000_000 + 30 * HOUR,
        ..headline(0, "Bitcoin ETF approval nears")
    };
    let key = persephone_data::news::NewsStore::search_key("(btc OR bitcoin)", 0);
    news(
        &mut app,
        NewsMessage::HistoryFound(Arc::new(vec![found.clone()]), key.clone(), 1, 1),
    );
    news(&mut app, NewsMessage::HistoryEnded);
    assert!(matches!(
        app.news().history,
        History::Finished {
            added: 1,
            nothing_left: false
        }
    ));
    assert!(
        app.news().store.searched(&key),
        "a stretch long over isn't searched again"
    );
    assert!(
        app.tile_chart(0)
            .unwrap()
            .events()
            .iter()
            .any(|mark| mark.id == found.id),
        "and it's marked"
    );
    assert!(shows(&app, "Added 1 older headlines from GDELT."));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_click_brings_every_chart_on_screen_to_the_headline() {
    let mut app = app();
    let root = dir("every");
    app.use_news_root(root.clone());
    let _ = app.update(Message::Key(Action::NewTile));
    settle(&mut app);
    let tiles = app.layout().tiles();
    assert_eq!(tiles.len(), 2);
    let picked = headline(700, "Bitcoin breaks out");
    news(
        &mut app,
        NewsMessage::Fetched(Arc::new(vec![picked.clone()]), Vec::new()),
    );
    news(&mut app, NewsMessage::Toggle);
    settle(&mut app);
    news(&mut app, NewsMessage::Pick(picked.id.clone()));
    for tile in tiles {
        let chart = app.tile_chart(tile).unwrap();
        let width = app.tile_rect(tile).unwrap().width - persephone_chart::PRICE_AXIS_WIDTH;
        assert!(
            (chart.middle_bar(width) - 700.0).abs() < 1.0,
            "tile {tile}: {}",
            chart.middle_bar(width)
        );
        assert_eq!(chart.lit_event(), Some(picked.id.as_str()));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_headline_after_the_charts_last_bar_isnt_marked_on_it() {
    const START: u64 = 1_704_067_200_000_000_000;
    let mut app = app_in_2024();
    let root = dir("after");
    app.use_news_root(root.clone());
    // Long after the chart's 500 bars, as a 2026 headline is on a chart ending in 2022.
    let later = Headline {
        published: START + 5_000 * HOUR,
        ..headline(0, "Bitcoin much later")
    };
    // In the last bar, half an hour after it opened: on the chart.
    let last_bar = Headline {
        published: START + 499 * HOUR + HOUR / 2,
        ..headline(1, "Bitcoin in the last hour")
    };
    news(
        &mut app,
        NewsMessage::Fetched(Arc::new(vec![later.clone(), last_bar.clone()]), Vec::new()),
    );
    news(&mut app, NewsMessage::Toggle);
    settle(&mut app);
    let chart = app.tile_chart(0).unwrap();
    let marked: Vec<&str> = chart.events().iter().map(|e| e.id.as_str()).collect();
    assert!(
        marked.contains(&"h1") && marked.contains(&"h0"),
        "{marked:?}"
    );
    assert_eq!(chart.bar_within(later.published), None);
    assert_eq!(chart.bar_within(last_bar.published), Some(499));
    assert!(
        shows(&app, "not on this chart"),
        "the list says so before a click"
    );

    let width = app.tile_rect(0).unwrap().width - persephone_chart::PRICE_AXIS_WIDTH;
    let before = app.tile_chart(0).unwrap().middle_bar(width);
    news(&mut app, NewsMessage::Pick(later.id.clone()));
    let note = app.news().note.clone().unwrap();
    assert!(
        note.contains("after this chart's last bar, 2024-01-21"),
        "{note}"
    );
    assert_eq!(
        app.tile_chart(0).unwrap().middle_bar(width),
        before,
        "the chart stays where it was"
    );
    news(&mut app, NewsMessage::Pick(last_bar.id.clone()));
    assert_eq!(app.news().note, None);
    std::fs::remove_dir_all(root).unwrap();
}
