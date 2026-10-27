//! Tick-level backtests: the strategy decides on bars, orders fill on ticks.

use std::sync::{Arc, Mutex};

use persephone_core::{
    BarSpec, InstrumentSpec, Side, Timeframe, UnixNanos,
    strategy::{BuiltInStrategy, StrategySpec},
    tick::{self, Aggressor, Quote, TickFeed, TickKind, Ticks, Trade},
};
use persephone_engine::{BacktestReport, BacktestRequest, FeeRates, run_backtest};

const START: UnixNanos = 1_388_966_400_000_000_000; // 2014-01-06, a Monday
const DAY: UnixNanos = 86_400_000_000_000;
const SECOND: UnixNanos = 1_000_000_000;
const SPREAD: f64 = 0.0002;

/// Ticks every `every` nanoseconds for `days` days: a slow wave, so EMA crosses happen.
#[derive(Debug)]
struct Wave {
    kind: TickKind,
    every: UnixNanos,
    days: u64,
    /// Every window asked for.
    calls: Mutex<Vec<(UnixNanos, UnixNanos)>>,
}

impl Wave {
    fn new(kind: TickKind, every: UnixNanos, days: u64) -> Self {
        Self {
            kind,
            every,
            days,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn mid(ts: UnixNanos) -> f64 {
        let hours = (ts - START) as f64 / 3.6e12;
        let mid = 1.30 + 0.01 * (hours / 7.0).sin() + 0.002 * (hours * 1.3).sin();
        (mid * 1e5).round() / 1e5
    }

    fn quote(ts: UnixNanos) -> Quote {
        let mid = Self::mid(ts);
        Quote {
            ts,
            bid: ((mid - SPREAD / 2.0) * 1e5).round() / 1e5,
            ask: ((mid + SPREAD / 2.0) * 1e5).round() / 1e5,
            bid_size: 0.0,
            ask_size: 0.0,
        }
    }

    /// The last tick at or before `ts`.
    fn last_at(&self, ts: UnixNanos) -> UnixNanos {
        START + (ts - START) / self.every * self.every
    }

    fn end(&self) -> UnixNanos {
        START + self.days * DAY
    }
}

impl TickFeed for Wave {
    fn ticks(&self, start: UnixNanos, end: UnixNanos) -> Result<Ticks, String> {
        self.calls.lock().unwrap().push((start, end));
        let end = end.min(self.end());
        let first = if start <= START {
            START
        } else {
            START + (start - START).div_ceil(self.every) * self.every
        };
        let times = (first..end).step_by(self.every as usize);
        Ok(match self.kind {
            TickKind::Quotes => Ticks::Quotes(times.map(Self::quote).collect()),
            TickKind::Trades => Ticks::Trades(
                times
                    .map(|ts| Trade {
                        ts,
                        price: Self::mid(ts),
                        size: 0.0,
                        aggressor: Aggressor::Unknown,
                    })
                    .collect(),
            ),
        })
    }
}

fn eurusd() -> InstrumentSpec {
    InstrumentSpec {
        symbol: "EURUSD".into(),
        venue: "TEST".into(),
        base: "EUR".into(),
        quote: "USD".into(),
        price_precision: 5,
        size_precision: 0,
    }
}

fn request(feed: &Arc<Wave>, with_ticks: bool) -> BacktestRequest {
    let all = feed.ticks(START, feed.end()).unwrap();
    feed.calls.lock().unwrap().clear();
    BacktestRequest {
        instrument: eurusd(),
        bars: Timeframe::M15.into(),
        candles: tick::bars(&all, Timeframe::M15),
        strategy: StrategySpec::with_params(BuiltInStrategy::EmaCross, &[10.0, 30.0, 10.0]),
        plugin: None,
        ticks: with_ticks.then(|| Arc::clone(feed) as Arc<dyn TickFeed>),
        starting_balance: 100_000.0,
        fees: FeeRates::ZERO,
    }
}

fn run(request: &BacktestRequest) -> BacktestReport {
    run_backtest(request).unwrap_or_else(|e| panic!("{e}"))
}

#[test]
fn quotes_fill_buys_at_the_ask_and_sells_at_the_bid() {
    let feed = Arc::new(Wave::new(TickKind::Quotes, 10 * SECOND, 10));
    let request = request(&feed, true);
    let report = run(&request);
    assert!(report.fills.len() >= 6, "{} fills", report.fills.len());

    let step = Timeframe::M15.nanos();
    for fill in &report.fills {
        let quote = Wave::quote(feed.last_at(fill.ts));
        let expected = match fill.side {
            Side::Buy => quote.ask,
            Side::Sell => quote.bid,
        };
        assert!(
            (fill.price - expected).abs() < 1e-9,
            "{fill:?} vs {quote:?}"
        );
        let closed = request.candles[fill.bar].ts + step;
        assert!(fill.ts >= closed, "filled before its bar closed");
        assert!(
            fill.ts - closed <= 10 * SECOND,
            "filled on the next tick at the latest"
        );
    }
}

#[test]
fn the_spread_is_a_cost_that_bar_fills_miss() {
    let feed = Arc::new(Wave::new(TickKind::Quotes, 10 * SECOND, 10));
    let on_ticks = run(&request(&feed, true));
    let on_bars = run(&request(&feed, false));
    assert_eq!(
        on_ticks.total_orders, on_bars.total_orders,
        "same decisions"
    );
    assert_eq!(on_ticks.fills.len(), on_bars.fills.len());

    // Every fill crosses half the spread against the mid the bars were built from.
    let half_spreads: f64 = on_ticks.fills.iter().map(|f| f.qty * SPREAD / 2.0).sum();
    let cost = on_bars.net_pnl() - on_ticks.net_pnl();
    assert!(
        cost > 0.0,
        "ticks {} vs bars {}",
        on_ticks.net_pnl(),
        on_bars.net_pnl()
    );
    assert!(
        (cost - half_spreads).abs() / half_spreads < 0.5,
        "cost {cost} vs half spreads {half_spreads}"
    );
}

#[test]
fn trades_fill_at_the_latest_trade() {
    let feed = Arc::new(Wave::new(TickKind::Trades, 10 * SECOND, 10));
    let report = run(&request(&feed, true));
    assert!(!report.fills.is_empty());
    for fill in &report.fills {
        let price = Wave::mid(feed.last_at(fill.ts));
        assert!((fill.price - price).abs() < 1e-9, "{fill:?} vs {price}");
    }
}

#[test]
fn long_histories_stream_a_day_at_a_time() {
    // About 1.2 million quotes: more than one batch.
    let feed = Arc::new(Wave::new(TickKind::Quotes, 3 * SECOND, 42));
    let request = request(&feed, true);
    let report = run(&request);
    assert!(!report.fills.is_empty());
    assert_eq!(report.equity.len(), request.candles.len());

    let calls = feed.calls.lock().unwrap();
    assert_eq!(calls.len(), 42, "one window per day");
    assert!(
        calls
            .iter()
            .all(|(start, end)| end - start == DAY && start % DAY == 0)
    );
}

#[test]
fn unreadable_ticks_fail_the_run() {
    #[derive(Debug)]
    struct Broken;
    impl TickFeed for Broken {
        fn ticks(&self, _: UnixNanos, _: UnixNanos) -> Result<Ticks, String> {
            Err("disk on fire".into())
        }
    }
    let feed = Arc::new(Wave::new(TickKind::Quotes, 10 * SECOND, 2));
    let mut request = request(&feed, true);
    request.ticks = Some(Arc::new(Broken));
    let error = run_backtest(&request).unwrap_err();
    assert!(error.to_string().contains("disk on fire"), "{error}");
}

#[test]
fn tick_bars_decide_after_their_last_tick() {
    let feed = Arc::new(Wave::new(TickKind::Quotes, 10 * SECOND, 10));
    let all = feed.ticks(START, feed.end()).unwrap();
    let mut request = request(&feed, true);
    request.bars = BarSpec::Ticks(90);
    request.candles = tick::bars_by(&all, request.bars);
    let report = run(&request);
    assert!(report.fills.len() >= 4, "{} fills", report.fills.len());
    let closes = request.bars.closes(&request.candles);
    for fill in &report.fills {
        // Decided when bar `fill.bar` was over (just before the next one opens) and filled
        // then, at the bar's last quote: never a quote from a later bar.
        assert_eq!(fill.ts, closes[fill.bar], "{fill:?}");
        let quote = Wave::quote(feed.last_at(fill.ts));
        assert!(quote.ts < request.candles[fill.bar + 1].ts);
        let expected = if fill.side == Side::Buy {
            quote.ask
        } else {
            quote.bid
        };
        assert!(
            (fill.price - expected).abs() < 1e-9,
            "{fill:?} vs {quote:?}"
        );
    }
}
