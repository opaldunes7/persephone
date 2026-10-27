//! Markets from the local store inside the app.

use std::sync::Arc;

use iced::{Point, Size};
use persephone::{
    Message, Persephone, Run,
    menu::{MenuMessage, Page},
    persist::{self, Saved, SavedTile},
};
use persephone_chart::{ChartData, ChartMessage};
use persephone_core::{
    BarSpec, Candle, Side, Timeframe, UnixNanos,
    strategy::BuiltInStrategy,
    tick::{Quote, TickKind, Ticks},
};
use persephone_data::store::{BarWriter, MarketInfo, MarketKind, MarketWriter, Spacing, Store};
use persephone_tiling::Layout;

const START: UnixNanos = 1_388_966_400_000_000_000; // 2014-01-06
const DAY: UnixNanos = 86_400_000_000_000;
const EVERY: UnixNanos = 20_000_000_000; // a quote every 20 s

fn quote(ts: UnixNanos) -> Quote {
    let hours = (ts - START) as f64 / 3.6e12;
    let mid = ((1.30 + 0.01 * (hours / 5.0).sin()) * 1e5).round() / 1e5;
    Quote {
        ts,
        bid: ((mid - 0.0001) * 1e5).round() / 1e5,
        ask: ((mid + 0.0001) * 1e5).round() / 1e5,
        bid_size: 0.0,
        ask_size: 0.0,
    }
}

/// A store holding EURUSD.TEST: four days of quotes.
fn store(name: &str) -> Store {
    let root = std::env::temp_dir().join(format!(
        "persephone-app-local-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root);
    let market = store
        .create(MarketInfo {
            symbol: "EURUSD".into(),
            venue: "TEST".into(),
            base: "EUR".into(),
            quote: "USD".into(),
            price_precision: 5,
            size_precision: 0,
            kind: MarketKind::Ticks(TickKind::Quotes),
            fee: 0.0,
            first: 0,
            last: 0,
            count: 0,
        })
        .unwrap();
    let mut writer = MarketWriter::new(market);
    for day in 0..4 {
        let from = START + day * DAY;
        let quotes = (from..from + DAY)
            .step_by(EVERY as usize)
            .map(quote)
            .collect();
        writer
            .write_day((from / DAY) as i64, Ticks::Quotes(quotes))
            .unwrap();
    }
    writer.finish().unwrap();
    store
}

pub(crate) fn binance_app() -> Persephone {
    let candles = (0..500u64)
        .map(|i| Candle {
            ts: START + i * Timeframe::H1.nanos(),
            open: 50_000.0,
            high: 50_010.0,
            low: 49_990.0,
            close: 50_000.0,
            volume: 1.0,
        })
        .collect();
    Persephone::preloaded(
        ChartData::new("BTCUSDT", "BINANCE", Timeframe::H1, 2, candles),
        Size::new(1400.0, 900.0),
    )
}

fn menu(app: &mut Persephone, message: MenuMessage) {
    let _ = app.update(Message::Chart(
        0,
        ChartMessage::RightClicked {
            at: Point::new(100.0, 100.0),
        },
    ));
    let _ = app.update(Message::Menu(message));
}

#[test]
fn a_local_market_opens_from_the_menu_and_backtests_on_its_ticks() {
    let store = store("menu");
    let mut app = binance_app();
    app.set_store(store.clone());

    menu(
        &mut app,
        MenuMessage::Open(Page::Symbol {
            query: String::new(),
            removing: None,
        }),
    );
    let _ = app.update(Message::Menu(MenuMessage::SetLocal {
        venue: "TEST".into(),
        symbol: "EURUSD".into(),
    }));
    app.load_now();
    assert_eq!(app.tile_venue(0), Some("TEST"));
    assert_eq!(app.tile_market(0), Some(("EURUSD", Timeframe::H1.into())));
    let chart = app.tile_chart(0).expect("the bars loaded");
    assert_eq!(chart.data().candles.len(), 4 * 24);

    menu(&mut app, MenuMessage::SetTimeframe(Timeframe::M15));
    app.load_now();
    assert_eq!(
        app.tile_chart(0).unwrap().data().candles.len(),
        4 * 96,
        "timeframes roll up from the store"
    );
    assert_eq!(
        app.tile_venue(0),
        Some("TEST"),
        "a timeframe change keeps the market"
    );

    menu(
        &mut app,
        MenuMessage::SetStrategy(BuiltInStrategy::EmaCross),
    );
    let (generation, request) = app.backtest_request(0).expect("a backtest to run");
    assert!(
        request.ticks.is_some(),
        "a few hundred thousand quotes: fills on ticks"
    );
    assert_eq!(request.fees.taker, 0.0, "the market's own fee");
    let report = persephone_engine::run_backtest(&request).unwrap();
    for fill in &report.fills {
        let last = quote(START + (fill.ts - START) / EVERY * EVERY);
        let expected = if fill.side == Side::Buy {
            last.ask
        } else {
            last.bid
        };
        assert!((fill.price - expected).abs() < 1e-9, "{fill:?} vs {last:?}");
    }
    let _ = app.update(Message::BacktestDone(0, generation, Ok(Arc::new(report))));
    assert!(matches!(app.tile_strategy(0).unwrap().1, Run::Done(_)));

    // Back to Binance from the same page.
    menu(&mut app, MenuMessage::SetSymbol("ETHUSDT".into()));
    assert_eq!(app.tile_venue(0), None);
    std::fs::remove_dir_all(store.root()).unwrap();
}

#[test]
fn local_markets_can_use_tick_bars() {
    let store = store("tick-bars");
    let mut app = binance_app();
    app.set_store(store.clone());
    menu(
        &mut app,
        MenuMessage::SetLocal {
            venue: "TEST".into(),
            symbol: "EURUSD".into(),
        },
    );
    app.load_now();

    menu(
        &mut app,
        MenuMessage::Open(Page::Bars {
            input: String::new(),
            error: None,
        }),
    );
    let _ = app.update(Message::Menu(MenuMessage::BarsInput("2.5v".into())));
    let _ = app.update(Message::Menu(MenuMessage::SubmitBars));
    let Some(persephone::menu::Popup::Menu {
        page: Page::Bars { error, .. },
        ..
    }) = app.popup()
    else {
        panic!("the bars page stays open")
    };
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("no traded size")),
        "{error:?}"
    );

    let _ = app.update(Message::Menu(MenuMessage::SetBars(BarSpec::Ticks(500))));
    app.load_now();
    assert_eq!(app.tile_market(0), Some(("EURUSD", BarSpec::Ticks(500))));
    let chart = app.tile_chart(0).expect("the tick bars loaded");
    // Four days of a quote every 20 s: 17,280 quotes, so 34 full bars and the rest.
    assert_eq!(chart.data().candles.len(), 35);
    assert_eq!(chart.data().bars, BarSpec::Ticks(500));

    menu(
        &mut app,
        MenuMessage::SetStrategy(BuiltInStrategy::EmaCross),
    );
    let (_, request) = app.backtest_request(0).unwrap();
    assert_eq!(request.bars, BarSpec::Ticks(500));
    assert!(request.ticks.is_some());
    persephone_engine::run_backtest(&request).unwrap();
    std::fs::remove_dir_all(store.root()).unwrap();
}

#[test]
fn saved_layouts_remember_local_markets() {
    let store = store("saved");
    let dir = store.root().join("layout");
    let path = dir.join("layout.ron");
    persist::save(
        &path,
        &Saved {
            workspace: 1,
            others: Vec::new(),
            layout: Layout::new(),
            tiles: vec![SavedTile {
                id: 0,
                local: Some("TEST".into()),
                symbol: "EURUSD".into(),
                timeframe: "1h".into(),
                indicators: Vec::new(),
                strategy: None,
                drawings_hidden: false,
                volume_hidden: false,
            }],
        },
    )
    .unwrap();

    let (mut app, _loads) = Persephone::start(&[], Some(path.clone()));
    app.set_store(store.clone());
    app.load_now();
    assert_eq!(app.tile_venue(0), Some("TEST"));
    assert_eq!(app.tile_chart(0).unwrap().data().candles.len(), 4 * 24);

    // Moving away and back saves the venue each time.
    menu(&mut app, MenuMessage::SetSymbol("BTCUSDT".into()));
    assert_eq!(persist::load(&path).unwrap().tiles[0].local, None);
    menu(
        &mut app,
        MenuMessage::SetLocal {
            venue: "TEST".into(),
            symbol: "EURUSD".into(),
        },
    );
    assert_eq!(
        persist::load(&path).unwrap().tiles[0].local.as_deref(),
        Some("TEST")
    );
    std::fs::remove_dir_all(store.root()).unwrap();
}

#[test]
fn a_missing_local_market_says_so() {
    let store = store("missing");
    let mut app = binance_app();
    app.set_store(store.clone());
    menu(
        &mut app,
        MenuMessage::SetLocal {
            venue: "TEST".into(),
            symbol: "GBPUSD".into(),
        },
    );
    app.load_now();
    assert!(app.tile_chart(0).is_none());
    assert!(
        app.tile_error(0).is_some_and(|e| e.contains("GBPUSD")),
        "{:?}",
        app.tile_error(0)
    );
    std::fs::remove_dir_all(store.root()).unwrap();
}

/// Adds GOLD.TEST to `store`: 60 daily bars from Monday 2014-01-06.
fn add_daily_bars(store: &Store) {
    let market = store
        .create(MarketInfo {
            symbol: "GOLD".into(),
            venue: "TEST".into(),
            base: "GOLD".into(),
            quote: "USD".into(),
            price_precision: 2,
            size_precision: 0,
            kind: MarketKind::Bars(Spacing::Minutes(1_440)),
            fee: 0.001,
            first: 0,
            last: 0,
            count: 0,
        })
        .unwrap();
    let bars = (0..60u64)
        .map(|i| Candle {
            ts: START + i * DAY,
            open: 1_200.0 + i as f64,
            high: 1_210.0 + i as f64,
            low: 1_190.0 + i as f64,
            close: 1_205.0 + i as f64,
            volume: 100.0,
        })
        .collect();
    let mut writer = BarWriter::new(market).unwrap();
    writer.write_year(2014, bars).unwrap();
    writer.finish().unwrap();
}

#[test]
fn a_market_of_daily_bars_shows_daily_and_longer() {
    let store = store("daily-bars");
    add_daily_bars(&store);
    let mut app = binance_app();
    app.set_store(store.clone());

    // An hourly chart moves up to daily bars, the shortest these make.
    menu(
        &mut app,
        MenuMessage::SetLocal {
            venue: "TEST".into(),
            symbol: "GOLD".into(),
        },
    );
    assert_eq!(app.tile_market(0), Some(("GOLD", Timeframe::D1.into())));
    app.load_now();
    assert_eq!(app.tile_chart(0).unwrap().data().candles.len(), 60);

    menu(&mut app, MenuMessage::SetTimeframe(Timeframe::W1));
    app.load_now();
    assert_eq!(
        app.tile_chart(0).unwrap().data().candles.len(),
        9,
        "60 days from a Monday"
    );
    menu(&mut app, MenuMessage::SetTimeframe(Timeframe::M15));
    assert_eq!(
        app.tile_market(0),
        Some(("GOLD", Timeframe::D1.into())),
        "too short for daily bars"
    );

    menu(
        &mut app,
        MenuMessage::SetStrategy(BuiltInStrategy::EmaCross),
    );
    let (_, request) = app.backtest_request(0).unwrap();
    assert!(request.ticks.is_none(), "no ticks: fills on bars");
    persephone_engine::run_backtest(&request).unwrap();
    std::fs::remove_dir_all(store.root()).unwrap();
}

#[test]
fn removing_a_local_market_deletes_it_and_moves_its_charts() {
    let store = store("remove");
    add_daily_bars(&store);
    let mut app = binance_app();
    app.set_store(store.clone());
    menu(
        &mut app,
        MenuMessage::SetLocal {
            venue: "TEST".into(),
            symbol: "EURUSD".into(),
        },
    );
    menu(&mut app, MenuMessage::SetBars(BarSpec::Ticks(500)));
    assert_eq!(app.tile_market(0), Some(("EURUSD", BarSpec::Ticks(500))));

    let _ = app.update(Message::Chart(
        0,
        ChartMessage::SymbolClicked {
            at: Point::new(40.0, 16.0),
        },
    ));
    let _ = app.update(Message::Menu(MenuMessage::AskRemoveLocal {
        venue: "TEST".into(),
        symbol: "EURUSD".into(),
    }));
    let removal = match app.popup() {
        Some(persephone::menu::Popup::SymbolSearch {
            removing: Some(removal),
            ..
        }) => removal.clone(),
        other => panic!("expected the question, got {other:?}"),
    };
    assert!(removal.bytes > 0 && removal.error.is_none());
    let _ = app.update(Message::Menu(MenuMessage::KeepLocal));
    assert!(matches!(
        app.popup(),
        Some(persephone::menu::Popup::SymbolSearch { removing: None, .. })
    ));

    let _ = app.update(Message::Menu(MenuMessage::AskRemoveLocal {
        venue: "TEST".into(),
        symbol: "EURUSD".into(),
    }));
    let _ = app.update(Message::Menu(MenuMessage::RemoveLocal));
    assert!(matches!(
        app.popup(),
        Some(persephone::menu::Popup::SymbolSearch {
            removing: Some(persephone::menu::Removal { deleting: true, .. }),
            ..
        })
    ));
    // What the deleting task does and sends back.
    store.remove("TEST", "EURUSD").unwrap();
    let _ = app.update(Message::LocalRemoved(
        "TEST".into(),
        "EURUSD".into(),
        Ok(()),
    ));
    assert!(store.market("TEST", "EURUSD").is_err());
    assert!(matches!(
        app.popup(),
        Some(persephone::menu::Popup::SymbolSearch { removing: None, .. })
    ));
    assert_eq!(
        app.tile_market(0),
        Some(("BTCUSDT", Timeframe::M1.into())),
        "the chart moved to Binance, on time bars"
    );
    assert_eq!(app.tile_venue(0), None);
    assert!(store.market("TEST", "GOLD").is_ok(), "the others stay");
    std::fs::remove_dir_all(store.root()).unwrap();
}

mod dropping {
    use std::{path::PathBuf, sync::Arc};

    use persephone::{
        Message,
        menu::{ImportState, MenuMessage, Popup},
    };
    use persephone_data::{
        import::{self, MarketOptions, Overrides},
        store::Store,
    };

    use super::binance_app;

    fn truefx_file(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("EURUSD-2014-01.csv");
        let mut csv = String::new();
        for i in 0..20_000u64 {
            let ms = i * 8_000;
            let (secs, millis) = (ms / 1_000, ms % 1_000);
            let (h, m, s) = (secs / 3_600 % 24, secs / 60 % 60, secs % 60);
            let day = 6 + secs / 86_400;
            let mid = 1.36 + (i as f64 / 300.0).sin() * 0.002;
            csv.push_str(&format!(
                "EUR/USD,201401{day:02} {h:02}:{m:02}:{s:02}.{millis:03},{:.5},{:.5}\n",
                mid,
                mid + 0.0001
            ));
        }
        std::fs::write(&path, csv).unwrap();
        path
    }

    fn import_state(app: &persephone::Persephone) -> &ImportState {
        match app.popup() {
            Some(Popup::Import { state, .. }) => state,
            other => panic!("expected the import panel, got {other:?}"),
        }
    }

    #[test]
    fn a_dropped_csv_imports_into_the_chart_it_landed_on() {
        let root = std::env::temp_dir().join(format!("persephone-app-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let store = Store::new(root.join("store"));
        let file = truefx_file(&root);

        let mut app = binance_app();
        app.set_store(store.clone());
        let _ = app.update(Message::FilesHovered(true));
        let _ = app.update(Message::FileDropped(file.clone()));
        let Some(Popup::Import { symbol, venue, .. }) = app.popup() else {
            panic!("no import panel")
        };
        assert_eq!(
            (symbol.as_str(), venue.as_str()),
            ("EURUSD", "LOCAL"),
            "guessed from the file name"
        );
        assert!(matches!(import_state(&app), ImportState::Detecting));

        // What the detection task sends back.
        let detection =
            import::detect(&import::sample(&file).unwrap(), &Overrides::default()).unwrap();
        let format = detection.format.clone();
        let _ = app.update(Message::Detected(file.clone(), Ok(Arc::new(detection))));
        assert!(matches!(import_state(&app), ImportState::Ready(_)));

        let _ = app.update(Message::Menu(MenuMessage::ImportVenueChanged(
            "true fx".into(),
        )));
        let _ = app.update(Message::Menu(MenuMessage::StartImport));
        let Some(Popup::Import { error, .. }) = app.popup() else {
            panic!()
        };
        assert!(
            error.as_deref().is_some_and(|e| e.contains("venue")),
            "{error:?}"
        );

        let _ = app.update(Message::Menu(MenuMessage::ImportVenueChanged(
            "truefx".into(),
        )));
        let _ = app.update(Message::Menu(MenuMessage::StartImport));
        assert!(matches!(import_state(&app), ImportState::Importing(_)));

        // What the import task does and sends back.
        let options = MarketOptions {
            symbol: "EURUSD".into(),
            venue: "TRUEFX".into(),
            base: None,
            quote: None,
            fee: None,
        };
        let summary = import::import(&store, &[file], &format, &options, &mut |_| true).unwrap();
        assert_eq!(summary.imported, 20_000);
        let _ = app.update(Message::Imported(Ok(Arc::new(summary))));
        assert!(matches!(import_state(&app), ImportState::Done(_)));
        assert_eq!(
            app.tile_venue(0),
            Some("TRUEFX"),
            "the chart switched to the new market"
        );
        app.load_now();
        assert!(
            app.tile_chart(0)
                .is_some_and(|c| !c.data().candles.is_empty())
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn picked_files_open_the_import_panel_on_their_chart() {
        let mut app = binance_app();
        app.set_store(Store::new(
            std::env::temp_dir().join("persephone-unused-store"),
        ));
        let _ = app.update(Message::FilesPicked(0, Vec::new()));
        assert!(app.popup().is_none(), "cancelled");
        let _ = app.update(Message::FilesPicked(
            0,
            vec!["/data/XAUUSD_D1.csv".into(), "/data/XAUUSD_D1-2.csv".into()],
        ));
        let Some(Popup::Import {
            files,
            symbol,
            tile,
            ..
        }) = app.popup()
        else {
            panic!("no import panel")
        };
        assert_eq!((files.len(), symbol.as_str(), *tile), (2, "XAUUSD", 0));
        assert!(matches!(import_state(&app), ImportState::Detecting));
    }

    #[test]
    fn files_dropped_together_join_one_import() {
        let mut app = binance_app();
        app.set_store(Store::new(
            std::env::temp_dir().join("persephone-unused-store"),
        ));
        let _ = app.update(Message::FileDropped("/data/a.csv".into()));
        let _ = app.update(Message::FileDropped("/data/b.csv".into()));
        let _ = app.update(Message::FileDropped("/data/a.csv".into()));
        let Some(Popup::Import { files, .. }) = app.popup() else {
            panic!()
        };
        assert_eq!(files.len(), 2);
        let _ = app.update(Message::Detected(
            "/data/a.csv".into(),
            Err("not a CSV".into()),
        ));
        assert!(matches!(import_state(&app), ImportState::Failed(e) if e == "not a CSV"));
    }
}
