use persephone_core::{
    Candle, InstrumentSpec, Side, Timeframe,
    strategy::{BuiltInStrategy, StrategySpec},
};
use persephone_engine::{
    BacktestReport, BacktestRequest, Direction, EngineError, FeeRates, Session, SimSettings,
    TradingRules, spawn_backtest,
};

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
        bars: Timeframe::H1.into(),
        candles,
        strategy,
        plugin: None,
        ticks: None,
        starting_balance: BALANCE,
        fees: FeeRates::ZERO,
        sim: SimSettings::default(),
        rules: TradingRules::default(),
        trade_from: 0,
        fill_bars: None,
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
fn unknown_currencies_are_fine() {
    // An index or an old coin from imported data: Nautilus doesn't know these codes.
    let mut req = request(sine_candles(300), ema(5.0, 20.0));
    req.instrument = InstrumentSpec {
        symbol: "SPX500".into(),
        venue: "LOCAL".into(),
        base: "SPX500".into(),
        quote: "XUSD".into(),
        ..btcusdt()
    };
    let report = run(req).unwrap();
    assert!(!report.fills.is_empty());
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

// --- Settings and rules ---------------------------------------------------------------------

/// The position after each fill, in BTC, starting flat.
fn positions(report: &BacktestReport) -> Vec<f64> {
    let mut held = 0.0;
    report
        .fills
        .iter()
        .map(|f| {
            held += if f.side == Side::Buy { f.qty } else { -f.qty };
            held
        })
        .collect()
}

#[test]
fn slippage_and_spread_move_every_fill_against_it() {
    let ideal = run(request(sine_candles(600), ema(5.0, 20.0))).unwrap();
    let mut costly = request(sine_candles(600), ema(5.0, 20.0));
    // 2 ticks of slippage and a 3-tick spread: buys 2 + 2 ticks above, sells 2 + 1 below.
    costly.sim = SimSettings {
        slippage_ticks: 2,
        spread_ticks: 3,
        ..SimSettings::default()
    };
    let costly = run(costly).unwrap();
    assert_eq!(costly.fills.len(), ideal.fills.len());
    for (a, b) in ideal.fills.iter().zip(&costly.fills) {
        let moved = ((b.price - a.price) * 100.0).round();
        let expected = if a.side == Side::Buy { 4.0 } else { -3.0 };
        assert_eq!(moved, expected, "{a:?} → {b:?}");
    }
    assert!(costly.net_pnl() < ideal.net_pnl());
}

#[test]
fn a_flat_fee_is_charged_once_per_order() {
    let mut req = request(sine_candles(600), ema(5.0, 20.0));
    req.fees = FeeRates::BINANCE_SPOT;
    req.sim.fee_per_order = Some(3.0);
    let report = run(req).unwrap();
    assert!(
        report.fills.iter().all(|f| (f.fee - 3.0).abs() < 1e-9),
        "{:?}",
        report.fills
    );
    assert!((report.fees - 3.0 * report.total_orders as f64).abs() < 1e-6);
}

#[test]
fn leverage_multiplies_positions() {
    let mut req = request(sine_candles(400), ema(5.0, 20.0));
    req.sim.leverage = 5.0;
    let report = run(req).unwrap();
    let first = report.fills[0].qty * report.fills[0].price;
    assert!(
        (first - 50_000.0).abs() < 250.0,
        "first entry worth {first}"
    );

    let mut bad = request(sine_candles(400), ema(5.0, 20.0));
    bad.sim.leverage = 0.5;
    assert!(matches!(run(bad), Err(EngineError::InvalidRequest(_))));
}

#[test]
fn long_only_and_short_only_never_cross_zero() {
    for (direction, sign) in [(Direction::LongOnly, 1.0), (Direction::ShortOnly, -1.0)] {
        let mut req = request(sine_candles(800), ema(5.0, 20.0));
        req.rules.direction = direction;
        let report = run(req).unwrap();
        let held = positions(&report);
        assert!(held.len() >= 4, "{direction:?} still trades");
        assert!(
            held.iter().all(|p| p * sign >= -1e-9),
            "{direction:?}: {held:?}"
        );
    }
}

#[test]
fn inverting_takes_the_opposite_side_of_every_trade_at_the_same_time() {
    let normal = run(request(sine_candles(800), ema(5.0, 20.0))).unwrap();
    let mut req = request(sine_candles(800), ema(5.0, 20.0));
    req.rules.invert = true;
    let inverted = run(req).unwrap();

    assert!(normal.fills.len() >= 4, "it trades");
    assert_eq!(normal.fills.len(), inverted.fills.len());
    for (a, b) in normal.fills.iter().zip(&inverted.fills) {
        assert_eq!(a.bar, b.bar);
        assert_ne!(a.side, b.side, "bar {}", a.bar);
    }
    assert!(
        positions(&normal)
            .iter()
            .zip(positions(&inverted))
            .all(|(a, b)| (a + b).abs() < 1e-9),
        "every position is its opposite"
    );
}

#[test]
fn sessions_stop_entries_and_can_close_positions() {
    let candles = sine_candles(800);
    let cut = candles[400].ts;
    let session = |close_after| {
        let mut req = request(candles.clone(), ema(5.0, 20.0));
        req.rules.session = Some(Session {
            contains: std::sync::Arc::new(move |ts| ts < cut),
            close_after,
        });
        run(req).unwrap()
    };
    let open = session(false);
    let held = positions(&open);
    for (fill, after) in open.fills.iter().zip(&held) {
        if fill.ts >= cut {
            assert!(
                after.abs() < 1e-9,
                "after the session, fills only close: {fill:?}"
            );
        }
    }
    let closing = session(true);
    let last = closing.fills.last().unwrap();
    assert!(last.ts >= cut && positions(&closing).last().unwrap().abs() < 1e-9);
    assert_eq!(
        closing.fills.iter().filter(|f| f.ts >= cut).count(),
        1,
        "closed once"
    );
}

#[test]
fn warm_up_bars_never_trade() {
    let candles = sine_candles(800);
    let from = candles[300].ts;
    let mut req = request(candles, ema(5.0, 20.0));
    req.trade_from = from;
    let report = run(req).unwrap();
    assert!(!report.fills.is_empty());
    assert!(report.fills.iter().all(|f| f.ts > from));
    assert_eq!(report.equity[299], BALANCE, "flat while warming up");
}

#[test]
fn latency_delays_fills() {
    let ideal = run(request(sine_candles(600), ema(5.0, 20.0))).unwrap();
    let mut slow = request(sine_candles(600), ema(5.0, 20.0));
    slow.sim.latency_ms = 250;
    let slow = run(slow).unwrap();
    assert_eq!(slow.fills.len(), ideal.fills.len());
    for (a, b) in ideal.fills.iter().zip(&slow.fills) {
        assert!(b.ts > a.ts, "{a:?} vs {b:?}");
    }
}

#[test]
fn orders_fill_on_finer_bars_when_given() {
    let candles = sine_candles(400);
    // Each hour's minutes drift up a little from its open, so a fill on a minute bar is never
    // at the hour's close.
    let minutes: Vec<Candle> = candles
        .iter()
        .flat_map(|hour| {
            (0..60).map(move |m| {
                let p = hour.open + m as f64 * 0.25;
                Candle {
                    ts: hour.ts + m * Timeframe::M1.nanos(),
                    open: p,
                    high: p + 0.5,
                    low: p - 0.5,
                    close: p + 0.25,
                    volume: 1.0,
                }
            })
        })
        .collect();
    let on_bars = run(request(candles.clone(), ema(5.0, 20.0))).unwrap();
    let on_minutes = |latency_ms| {
        let mut req = request(candles.clone(), ema(5.0, 20.0));
        req.fill_bars = Some(std::sync::Arc::new(minutes.clone()));
        req.sim.latency_ms = latency_ms;
        run(req).unwrap()
    };
    // A market order fills at the last price when it's placed: the close of the minute that
    // ends with the hour, not the hour's own close.
    let prompt = on_minutes(0);
    assert_eq!(prompt.fills.len(), on_bars.fills.len());
    let minute_closing_at = |ts| {
        minutes
            .iter()
            .find(|m| m.ts + Timeframe::M1.nanos() == ts)
            .unwrap_or_else(|| panic!("no minute closes at {ts}"))
    };
    for (fill, on_bar) in prompt.fills.iter().zip(&on_bars.fills) {
        assert_eq!(fill.ts, on_bar.ts);
        assert!(
            (fill.price - minute_closing_at(fill.ts).close).abs() < 0.006,
            "{fill:?}"
        );
        assert!(
            (fill.price - on_bar.price).abs() > 0.5,
            "not the hour's close: {fill:?}"
        );
    }
    // Late orders fill on the next minute bar that arrives after them.
    for fill in &on_minutes(250).fills {
        let minute = minute_closing_at(fill.ts);
        assert!(
            fill.ts % Timeframe::H1.nanos() != 0,
            "a minute into the hour: {fill:?}"
        );
        assert!(
            fill.price >= minute.low - 0.006 && fill.price <= minute.high + 0.006,
            "{fill:?} vs {minute:?}"
        );
    }
}

#[test]
fn tick_bars_can_fill_on_themselves() {
    // Nautilus ranks bar types by duration, which tick bars don't have.
    let mut req = request(sine_candles(400), ema(5.0, 20.0));
    req.bars = persephone_core::BarSpec::Ticks(100);
    let report = run(req).unwrap();
    assert!(!report.fills.is_empty());
}
