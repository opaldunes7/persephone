use persephone_core::{Candle, InstrumentSpec, Side, Timeframe};
use persephone_engine::{
    BacktestReport, BacktestRequest, EngineError, FeeRates, StrategySpec, spawn_backtest,
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

/// A slow sine wave around 50k, so the EMAs cross a predictable number of times.
fn sine_candles(n: usize) -> Vec<Candle> {
    (0..n)
        .map(|i| {
            let mid = 50_000.0 + 2_000.0 * (i as f64 / 40.0).sin();
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

fn request(candles: Vec<Candle>, fast: usize, slow: usize) -> BacktestRequest {
    BacktestRequest {
        instrument: btcusdt(),
        timeframe: Timeframe::H1,
        candles,
        strategy: StrategySpec::EmaCross {
            fast,
            slow,
            trade_size: 0.1,
        },
        starting_balance: 100_000.0,
        fees: FeeRates::ZERO,
    }
}

fn run(req: BacktestRequest) -> Result<BacktestReport, EngineError> {
    spawn_backtest(req)
        .join()
        .expect("backtest thread panicked")
}

#[test]
fn ema_cross_flips_between_long_and_short() {
    let report = run(request(sine_candles(1_000), 5, 20)).unwrap();
    let fills = &report.fills;

    // ~1000 / (2π·40) ≈ 4 full waves → about 8 crosses.
    assert!((6..=10).contains(&fills.len()), "got {} fills", fills.len());
    assert!((fills[0].qty - 0.1).abs() < 1e-9, "first entry is one unit");
    assert!(
        fills[1..].iter().all(|f| (f.qty - 0.2).abs() < 1e-9),
        "reversals are two units"
    );
    assert!(
        fills.windows(2).all(|w| w[0].side != w[1].side),
        "sides alternate"
    );
    assert!(fills.windows(2).all(|w| w[0].ts < w[1].ts));
    assert_eq!(report.total_orders, fills.len());
    assert!(
        report.pnl.contains_key("PnL (total)"),
        "pnl stats: {:?}",
        report.pnl.keys()
    );
}

#[test]
fn fills_never_precede_the_signal_bar_close() {
    let candles = sine_candles(400);
    let first_bar_close = candles[0].ts + Timeframe::H1.nanos();
    let report = run(request(candles, 5, 20)).unwrap();
    assert!(report.fills.iter().all(|f| f.ts >= first_bar_close));
}

#[test]
fn same_input_gives_same_fills() {
    let a = run(request(sine_candles(600), 8, 21)).unwrap();
    let b = run(request(sine_candles(600), 8, 21)).unwrap();
    assert_eq!(a.fills, b.fills);
    // Stats with no samples (e.g. "Avg Loser" without losers) are NaN, and NaN != NaN.
    assert_eq!(format!("{:?}", a.pnl), format!("{:?}", b.pnl));
}

#[test]
fn rejects_bad_requests() {
    assert!(matches!(
        run(request(Vec::new(), 5, 20)),
        Err(EngineError::NoData)
    ));
    assert!(matches!(
        run(request(sine_candles(100), 20, 5)),
        Err(EngineError::InvalidRequest(_))
    ));
    let mut tiny = request(sine_candles(100), 5, 20);
    tiny.strategy = StrategySpec::EmaCross {
        fast: 5,
        slow: 20,
        trade_size: 0.000_001,
    };
    assert!(matches!(run(tiny), Err(EngineError::InvalidRequest(_))));
}

#[test]
fn long_short_pnl_has_the_right_sign() {
    // Price only rises after the first cross: the strategy goes long and stays long.
    let mut candles = sine_candles(120);
    let last = candles.last().unwrap().close;
    for i in 0..200 {
        let p = last + 10.0 * i as f64;
        candles.push(Candle {
            ts: START + (120 + i) as u64 * Timeframe::H1.nanos(),
            open: p,
            high: p + 15.0,
            low: p - 5.0,
            close: p + 10.0,
            volume: 10.0,
        });
    }
    let report = run(request(candles, 5, 20)).unwrap();
    let last_fill = report.fills.last().expect("at least one fill");
    assert_eq!(last_fill.side, Side::Buy, "ends long in an uptrend");
}
