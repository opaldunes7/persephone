use persephone_core::{
    Candle, InstrumentSpec, Side, Timeframe,
    strategy::{BuiltInStrategy, StrategySpec},
};
use persephone_engine::{BacktestReport, BacktestRequest, EngineError, FeeRates, spawn_backtest};

const START: u64 = 1_735_689_600_000_000_000; // 2025-01-01T00:00:00Z
const BALANCE: f64 = 100_000.0;

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

/// A slow sine wave around 50k, so trend and reversion rules both trigger predictably.
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

fn request(candles: Vec<Candle>, strategy: StrategySpec) -> BacktestRequest {
    BacktestRequest {
        instrument: btcusdt(),
        timeframe: Timeframe::H1,
        candles,
        strategy,
        starting_balance: BALANCE,
        fees: FeeRates::ZERO,
    }
}

fn ema(fast: f64, slow: f64) -> StrategySpec {
    StrategySpec::with_params(BuiltInStrategy::EmaCross, &[fast, slow, 10.0])
}

fn run(req: BacktestRequest) -> Result<BacktestReport, EngineError> {
    spawn_backtest(req)
        .join()
        .expect("backtest thread panicked")
}

#[test]
fn ema_cross_flips_between_long_and_short() {
    let report = run(request(sine_candles(1_000), ema(5.0, 20.0))).unwrap();
    let fills = &report.fills;

    // ~1000 / (2π·40) ≈ 4 full waves → about 8 crosses.
    assert!((6..=10).contains(&fills.len()), "got {} fills", fills.len());
    // Each position is 10% of the balance: ~0.2 BTC at 50k; reversals carry two positions.
    let first_value = fills[0].qty * fills[0].price;
    assert!(
        (first_value - 10_000.0).abs() < 50.0,
        "first entry worth {first_value}"
    );
    assert!(
        fills[1..].iter().all(|f| f.qty > fills[0].qty * 1.5),
        "reversals close and reopen"
    );
    assert!(
        fills.windows(2).all(|w| w[0].side != w[1].side),
        "sides alternate"
    );
    assert!(fills.windows(2).all(|w| w[0].bar < w[1].bar));
    assert_eq!(report.total_orders, fills.len());
}

#[test]
fn every_builtin_trades_and_reports_equity() {
    for kind in BuiltInStrategy::ALL {
        let candles = sine_candles(1_200);
        let report = run(request(candles.clone(), StrategySpec::new(kind))).unwrap();
        assert!(!report.fills.is_empty(), "{kind:?} trades");
        assert_eq!(report.equity.len(), candles.len());
        assert_eq!(report.equity[0], BALANCE, "{kind:?} starts flat");
        assert!(report.max_drawdown <= 0.0);
        assert!(report.fills.iter().all(|f| f.bar < candles.len()));
    }
}

#[test]
fn equity_agrees_with_nautilus_realized_pnl_when_flat() {
    // Zero fees and a reversal strategy: the position is open at the end, so compare the
    // equity change up to the last fill with Nautilus's realized PnL plus that open trade.
    let report = run(request(sine_candles(800), ema(5.0, 20.0))).unwrap();
    let last = report.fills.last().unwrap();
    let equity_at_last_fill = report.equity[last.bar];
    let realized = report.stat("PnL (total)").unwrap();
    assert!(
        (equity_at_last_fill - BALANCE - realized).abs() < 1.0,
        "equity {equity_at_last_fill} vs realized {realized}"
    );
}

#[test]
fn fees_are_counted() {
    let mut req = request(sine_candles(600), ema(5.0, 20.0));
    req.fees = FeeRates::BINANCE_SPOT;
    let report = run(req).unwrap();
    let traded: f64 = report.fills.iter().map(|f| f.qty * f.price).sum();
    assert!(
        (report.fees - traded * 0.001).abs() < 0.05,
        "0.1% of {traded} vs {}",
        report.fees
    );
}

#[test]
fn fills_never_precede_the_signal_bar_close() {
    let candles = sine_candles(400);
    let first_bar_close = candles[0].ts + Timeframe::H1.nanos();
    let report = run(request(candles, ema(5.0, 20.0))).unwrap();
    assert!(report.fills.iter().all(|f| f.ts >= first_bar_close));
}

#[test]
fn same_input_gives_same_fills() {
    let a = run(request(sine_candles(600), ema(8.0, 21.0))).unwrap();
    let b = run(request(sine_candles(600), ema(8.0, 21.0))).unwrap();
    assert_eq!(a.fills, b.fills);
    assert_eq!(a.equity, b.equity);
}

#[test]
fn rejects_bad_requests() {
    assert!(matches!(
        run(request(Vec::new(), ema(5.0, 20.0))),
        Err(EngineError::NoData)
    ));
    assert!(matches!(
        run(request(sine_candles(100), ema(20.0, 5.0))),
        Err(EngineError::InvalidRequest(_))
    ));
    let mut tiny = request(sine_candles(100), ema(5.0, 20.0));
    tiny.starting_balance = 0.000_001;
    assert!(
        matches!(run(tiny), Err(EngineError::InvalidRequest(_))),
        "positions round to zero"
    );
}

#[test]
fn ends_long_in_an_uptrend() {
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
    let report = run(request(candles, ema(5.0, 20.0))).unwrap();
    assert_eq!(report.fills.last().unwrap().side, Side::Buy);
    assert!(report.net_pnl() > 0.0, "the trend paid");
}
