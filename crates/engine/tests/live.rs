//! Paper trading through Nautilus's live node, on a replay so it runs offline.
#![cfg(feature = "live")]

use std::{sync::mpsc, time::Duration};

use persephone_core::{
    Candle, InstrumentSpec, Side, Timeframe,
    strategy::{BuiltInStrategy, StrategySpec},
};
use persephone_engine::{
    BacktestRequest, FeeRates,
    live::{Book, LiveEvent, LiveFeed, LiveFill, LiveMode, LiveRequest, start_live},
    spawn_backtest,
};

const START: u64 = 1_735_689_600_000_000_000; // 2025-01-01T00:00:00Z

fn btcusdt() -> InstrumentSpec {
    InstrumentSpec {
        symbol: "BTCUSDT".into(),
        venue: "BINANCE".into(),
        base: "BTC".into(),
        quote: "USDT".into(),
        price_precision: 2,
        size_precision: 5,
    }
}

fn sine_candles(n: usize) -> Vec<Candle> {
    (0..n)
        .map(|i| {
            let mid = 50_000.0 + 2_000.0 * (i as f64 / 15.0).sin();
            Candle {
                ts: START + i as u64 * Timeframe::H1.nanos(),
                open: mid - 5.0,
                high: mid + 20.0,
                low: mid - 20.0,
                close: mid + 5.0,
                volume: 10.0,
            }
        })
        .collect()
}

fn ema() -> StrategySpec {
    StrategySpec::with_params(BuiltInStrategy::EmaCross, &[5.0, 20.0, 10.0])
}

fn request(history: Vec<Candle>, replay: Vec<Candle>, mode: LiveMode) -> LiveRequest {
    LiveRequest {
        mode,
        feed: LiveFeed::Replay {
            bars: replay,
            every: Duration::from_millis(5),
        },
        instrument: btcusdt(),
        timeframe: Timeframe::H1,
        history,
        strategy: ema(),
        plugin: None,
        balance: 100_000.0,
        fees: FeeRates::ZERO,
        owns_signals: false,
    }
}

#[test]
fn paper_trading_fills_like_a_backtest_from_where_it_starts() {
    let candles = sine_candles(320);
    let (history, replay) = candles.split_at(200);
    let (tx, rx) = mpsc::channel();
    let session = start_live(
        request(history.to_vec(), replay.to_vec(), LiveMode::Paper),
        move |event| {
            let _ = tx.send(event);
        },
    )
    .unwrap();

    let mut bars = Vec::new();
    let mut fills = Vec::new();
    let mut started = false;
    let stopped = loop {
        match rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the session went quiet")
        {
            LiveEvent::Started => started = true,
            LiveEvent::Bar(bar) => {
                bars.push(bar);
                if bars.len() == replay.len() {
                    session.stop();
                }
            }
            LiveEvent::Fill(fill) => fills.push(fill),
            LiveEvent::Problem(message) => panic!("{message}"),
            LiveEvent::Stopped(outcome) => break outcome,
        }
    };
    session.join();
    assert!(started);
    assert_eq!(stopped, None);
    assert_eq!(
        bars.first().map(|b| b.ts),
        Some(replay[0].ts),
        "bars carry open times"
    );
    assert_eq!(bars.len(), replay.len());

    // The backtest over all of it trades on the same bars at the same prices once the live
    // session has started (the first live entry is smaller: it starts flat).
    let report = spawn_backtest(BacktestRequest {
        instrument: btcusdt(),
        bars: Timeframe::H1.into(),
        candles: candles.clone(),
        strategy: ema(),
        plugin: None,
        ticks: None,
        starting_balance: 100_000.0,
        fees: FeeRates::ZERO,
    })
    .join()
    .unwrap()
    .unwrap();
    let expected: Vec<_> = report
        .fills
        .iter()
        .filter(|f| f.bar >= history.len())
        .map(|f| (candles[f.bar].ts, f.side, f.price))
        .collect();
    // Stopping closes the position: one more fill, the size of a single position.
    let (exit, fills) = fills.split_last().expect("fills");
    let got: Vec<_> = fills.iter().map(|f| (f.bar, f.side, f.price)).collect();
    assert!(expected.len() >= 3, "the replay crosses a few times");
    assert_eq!(got, expected);
    assert!(
        fills[1..].iter().all(|f| f.qty > fills[0].qty * 1.5),
        "reversals after the first entry"
    );
    assert_ne!(exit.side, fills.last().unwrap().side);
    assert!(
        (exit.qty - fills[0].qty).abs() < fills[0].qty * 0.2,
        "{} vs {}",
        exit.qty,
        fills[0].qty
    );
}

#[test]
fn the_book_marks_the_position_to_the_latest_close() {
    let fill = |side, qty, price| {
        LiveEvent::Fill(LiveFill {
            ts: 0,
            bar: 0,
            side,
            price,
            qty,
            fee: 0.5,
        })
    };
    let bar = |close| {
        LiveEvent::Bar(Candle {
            ts: 0,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
        })
    };
    let mut book = Book::default();
    for event in [fill(Side::Buy, 2.0, 100.0), bar(110.0)] {
        book.apply(&event);
    }
    assert!((book.pnl() - 19.5).abs() < 1e-9);
    book.apply(&fill(Side::Sell, 2.0, 105.0));
    assert_eq!((book.position, book.fills), (0.0, 2));
    assert!((book.pnl() - 9.0).abs() < 1e-9);
    assert!((book.fees - 1.0).abs() < 1e-9);
}

#[test]
fn replays_only_paper_trade() {
    let candles = sine_candles(10);
    let refused = start_live(request(Vec::new(), candles, LiveMode::Testnet), |_| {});
    assert!(refused.unwrap_err().to_string().contains("paper"));
}
