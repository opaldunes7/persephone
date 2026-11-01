//! Several markets on one account, worked out by hand. The first test is the one the rest lean on: a
//! portfolio of one market has to come out exactly as a backtest of that market does.

use std::sync::{Arc, Mutex};

use persephone_core::{
    Candle, InstrumentSpec, Side, Timeframe,
    signal::{Signal, SignalFactory},
    strategy::{BuiltInStrategy, StrategyInfo, StrategyKind, StrategySpec},
};
use persephone_engine::{
    BacktestRequest, FeeRates, PortfolioLeg, PortfolioReport, PortfolioRequest, Rebalance,
    SimSettings, TradingRules, run_backtest, run_portfolio,
};

const HOUR: u64 = 3_600_000_000_000;
const START: u64 = 1_735_689_600_000_000_000;
const VOLUME: f64 = 10_000.0;

fn spec(symbol: &str) -> InstrumentSpec {
    InstrumentSpec {
        symbol: symbol.into(),
        venue: "BINANCE".into(),
        base: symbol.trim_end_matches("USDT").into(),
        quote: "USDT".into(),
        price_precision: 2,
        size_precision: 5,
    }
}

fn candle(at: usize, close: f64) -> Candle {
    Candle {
        ts: START + at as u64 * HOUR,
        open: close,
        high: close,
        low: close,
        close,
        volume: VOLUME,
    }
}

fn candles(closes: &[f64]) -> Vec<Candle> {
    closes
        .iter()
        .enumerate()
        .map(|(at, &close)| candle(at, close))
        .collect()
}

fn holding(symbol: &str, closes: &[f64], weight: f64) -> PortfolioLeg {
    PortfolioLeg {
        instrument: spec(symbol),
        candles: candles(closes),
        strategy: None,
        plugin: None,
        weight,
        fees: FeeRates::ZERO,
        funding: None,
    }
}

fn request(legs: Vec<PortfolioLeg>) -> PortfolioRequest {
    PortfolioRequest {
        legs,
        bars: Timeframe::H1.into(),
        weights: None,
        rebalance: Rebalance::default(),
        starting_balance: 100_000.0,
        sim: SimSettings::default(),
        rules: TradingRules::default(),
        trade_from: 0,
        maintenance_percent: 0.5,
    }
}

fn run(request: &PortfolioRequest) -> PortfolioReport {
    run_portfolio(request).unwrap_or_else(|e| panic!("{e}"))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

/// A long, choppy series, so a strategy trades plenty of times.
fn wave(bars: usize) -> Vec<Candle> {
    (0..bars)
        .map(|at| {
            let close = 100.0
                + 20.0 * (at as f64 / 17.0).sin()
                + 7.0 * (at as f64 / 5.0).sin()
                + at as f64 * 0.02;
            let close = (close * 100.0).round() / 100.0;
            Candle {
                ts: START + at as u64 * HOUR,
                open: close - 0.3,
                high: close + 1.1,
                low: close - 1.2,
                close,
                volume: 500.0 + (at % 7) as f64 * 40.0,
            }
        })
        .collect()
}

fn parity(sim: SimSettings, fees: FeeRates, funding: Option<Vec<(u64, f64)>>) {
    let strategy = StrategySpec::with_params(BuiltInStrategy::EmaCross, &[5.0, 13.0, 20.0]);
    let funding = funding.map(Arc::new);
    let candles = wave(600);
    let single = run_backtest(&BacktestRequest {
        instrument: spec("BTCUSDT"),
        bars: Timeframe::H1.into(),
        candles: candles.clone(),
        strategy: strategy.clone(),
        plugin: None,
        ticks: None,
        starting_balance: 100_000.0,
        fees,
        sim,
        rules: TradingRules::default(),
        trade_from: 0,
        fill_bars: None,
        funding: funding.clone(),
    })
    .unwrap();
    let mut portfolio = request(vec![PortfolioLeg {
        instrument: spec("BTCUSDT"),
        candles,
        strategy: Some(strategy),
        plugin: None,
        weight: 1.0,
        fees,
        funding,
    }]);
    portfolio.sim = sim;
    let portfolio = run(&portfolio);
    assert!(single.fills.len() > 20, "{} fills", single.fills.len());
    assert_eq!(portfolio.legs[0].fills.len(), single.fills.len());
    for (a, b) in portfolio.legs[0].fills.iter().zip(&single.fills) {
        assert_eq!(
            (a.ts, a.bar, a.side, a.price, a.qty, a.maker, a.range_slip),
            (b.ts, b.bar, b.side, b.price, b.qty, b.maker, b.range_slip),
        );
        assert!((a.fee - b.fee).abs() < 1e-9, "{a:?} against {b:?}");
    }
    let leg = &portfolio.legs[0];
    assert_eq!(leg.orders_worked, single.orders_worked);
    assert_eq!(leg.bars_to_fill_longest, single.bars_to_fill_longest);
    assert!(close(leg.bars_to_fill_average, single.bars_to_fill_average));
    assert!(close(leg.unfilled_at_end, single.unfilled_at_end));
    assert_eq!(portfolio.equity.len(), single.equity.len());
    for (a, b) in portfolio.equity.iter().zip(&single.equity) {
        assert!((a - b).abs() < 1e-6, "{a} against {b}");
    }
    assert!(close(portfolio.fees, single.fees));
    assert!(close(portfolio.funding_paid, single.funding_paid));
    assert!(close(portfolio.borrow_paid, single.borrow_paid));
    assert!(close(portfolio.max_drawdown, single.max_drawdown));
}

#[test]
fn one_market_at_full_weight_matches_a_backtest_exactly() {
    parity(SimSettings::default(), FeeRates::ZERO, None);
}

#[test]
fn it_matches_with_every_cost_on_too() {
    let sim = SimSettings {
        slippage_ticks: 2,
        slippage_percent: 0.01,
        spread_ticks: 3,
        impact_percent: 1.0,
        range_slippage_percent: 2.0,
        borrow_percent_per_year: Some(5.0),
        ..SimSettings::default()
    };
    parity(
        sim,
        FeeRates {
            maker: 0.0002,
            taker: 0.0005,
        },
        None,
    );
}

#[test]
fn it_matches_when_compounding_when_slicing_and_with_leverage() {
    parity(
        SimSettings {
            compound: true,
            ..SimSettings::default()
        },
        FeeRates::BINANCE_SPOT,
        None,
    );
    parity(
        SimSettings {
            max_participation_percent: Some(1.0),
            ..SimSettings::default()
        },
        FeeRates::BINANCE_SPOT,
        None,
    );
    parity(
        SimSettings {
            leverage: 3.0,
            ..SimSettings::default()
        },
        FeeRates::BINANCE_SPOT,
        None,
    );
    parity(
        SimSettings {
            latency_ms: 50,
            ..SimSettings::default()
        },
        FeeRates::BINANCE_SPOT,
        None,
    );
}

#[test]
fn it_matches_filling_at_the_next_open() {
    let next_open = SimSettings {
        next_open: true,
        ..SimSettings::default()
    };
    parity(next_open, FeeRates::BINANCE_SPOT, None);
    parity(
        SimSettings {
            slippage_percent: 0.01,
            impact_percent: 1.0,
            range_slippage_percent: 2.0,
            max_participation_percent: Some(1.0),
            ..next_open
        },
        FeeRates::BINANCE_SPOT,
        None,
    );
    parity(
        SimSettings {
            compound: true,
            ..next_open
        },
        FeeRates::BINANCE_PERP,
        Some((0..75).map(|at| (START + at * 8 * HOUR, 0.0001)).collect()),
    );
}

#[test]
fn it_matches_with_funding() {
    let rates = (0..75)
        .map(|at| (START + at * 8 * HOUR, 0.0001 + at as f64 * 1e-6))
        .collect();
    parity(SimSettings::default(), FeeRates::BINANCE_PERP, Some(rates));
}

#[test]
fn two_legs_on_one_account_add_up_by_hand() {
    // 30,000 of A at 100 is 300 of it, and 20,000 short of B at 50 is 400 of it.
    let a = holding("AAAUSDT", &[100.0, 100.0, 110.0, 120.0], 0.3);
    let b = holding("BBBUSDT", &[50.0, 50.0, 45.0, 40.0], -0.2);
    let report = run(&request(vec![a, b]));
    let fills = |leg: usize| &report.legs[leg].fills;
    assert_eq!(fills(0).len(), 1);
    assert_eq!(
        (fills(0)[0].side, fills(0)[0].qty, fills(0)[0].price),
        (Side::Buy, 300.0, 100.0)
    );
    assert_eq!(
        (fills(1)[0].side, fills(1)[0].qty, fills(1)[0].price),
        (Side::Sell, 400.0, 50.0)
    );
    // At the end, A has made 300 * 20 and B 400 * 10.
    assert!(close(*report.legs[0].contribution.last().unwrap(), 6_000.0));
    assert!(close(*report.legs[1].contribution.last().unwrap(), 4_000.0));
    assert!(close(*report.equity.last().unwrap(), 110_000.0));
    // Worth 36,000 long and 16,000 short on an account of 110,000.
    assert!(close(
        *report.gross_exposure.last().unwrap(),
        52_000.0 / 110_000.0
    ));
    assert!(close(
        *report.net_exposure.last().unwrap(),
        20_000.0 / 110_000.0
    ));
    assert!(close(report.turnover, 0.5));
}

#[test]
fn each_leg_pays_its_own_fees_from_the_one_account() {
    let mut spot = holding("AAAUSDT", &[100.0, 100.0], 0.5);
    spot.fees = FeeRates {
        maker: 0.0,
        taker: 0.001,
    };
    let mut perp = holding("BBBUSDT-PERP", &[100.0, 100.0], -0.5);
    perp.fees = FeeRates {
        maker: 0.0,
        taker: 0.0005,
    };
    let report = run(&request(vec![spot, perp]));
    assert!(close(report.legs[0].fees, 50.0), "{}", report.legs[0].fees);
    assert!(close(report.legs[1].fees, 25.0), "{}", report.legs[1].fees);
    assert!(close(*report.equity.last().unwrap(), 100_000.0 - 75.0));
}

#[test]
fn one_legs_profit_keeps_anothers_losses_from_ending_it() {
    // Leverage 5. A long 250,000 falls 30%, a loss of 75,000. So does a short 250,000 of B rise, the
    // same again, which would be a loss of 150,000 on 100,000 if A hadn't made it up.
    let a = holding("AAAUSDT", &[100.0, 100.0, 70.0, 70.0], 2.5);
    let b = holding("BBBUSDT", &[100.0, 100.0, 70.0, 70.0], -2.5);
    let mut both = request(vec![a, b]);
    both.sim.leverage = 5.0;
    let report = run(&both);
    assert_eq!(report.liquidated_at, None, "{:?}", report.notes);
    assert!(close(*report.equity.last().unwrap(), 100_000.0));
    assert!(
        close(report.peak_margin_use, 1.0),
        "{}",
        report.peak_margin_use
    );
}

#[test]
fn a_position_taking_all_the_cash_doesnt_call_that_too_much_margin() {
    let mut leg = holding("AAAUSDT", &[100.0, 100.0, 110.0], 0.999);
    leg.fees = FeeRates {
        maker: 0.0,
        taker: 0.001,
    };
    let report = run(&request(vec![leg]));
    assert!(
        !report
            .notes
            .iter()
            .any(|note| note.text.contains("in margin"))
    );
}

#[test]
fn a_book_needing_more_margin_than_the_account_has_says_so() {
    let closes = [100.0, 100.0, 100.0];
    let report = run(&request(vec![
        holding("AAAUSDT", &closes, 0.8),
        holding("BBBUSDT", &closes, -0.8),
    ]));
    assert!(
        close(report.peak_margin_use, 1.6),
        "{}",
        report.peak_margin_use
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.text.contains("in margin"))
    );
}

#[test]
fn liquidation_takes_everything_at_once_whatever_the_volume_limit() {
    // Worth 400,000 at 100 with leverage 5, and each unit is 4,000 against bars trading 10,000.
    let closes = [100.0, 100.0, 100.0, 100.0, 75.2, 75.2, 90.0];
    let mut one = request(vec![holding("AAAUSDT", &closes, 4.0)]);
    one.sim.leverage = 5.0;
    one.sim.max_participation_percent = Some(20.0);
    let report = run(&one);
    let leg = &report.legs[0];
    // In over two bars at 2,000 a bar, then out in one go: all 4,000 of it, though a bar allows 2,000.
    let sizes: Vec<(usize, Side, f64)> = leg
        .fills
        .iter()
        .map(|fill| (fill.bar, fill.side, fill.qty))
        .collect();
    assert_eq!(
        sizes,
        vec![
            (0, Side::Buy, 2_000.0),
            (1, Side::Buy, 2_000.0),
            (4, Side::Sell, 4_000.0)
        ],
        "{:?}",
        report.notes
    );
    assert_eq!(report.liquidated_at, Some(report.times[4]));
    // Lost 24.80 on each of 4,000: 99,200 of 100,000, under the 1,504 needed to stay open. Flat from
    // then on, and nothing more traded.
    assert!(close(*report.equity.last().unwrap(), 800.0));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.text.contains("liquidated"))
    );
}

#[test]
fn liquidation_comes_from_the_worst_of_the_bar() {
    // It closed fine, but went low enough in the bar to take it out.
    let mut leg = holding("AAAUSDT", &[100.0, 100.0, 100.0, 100.0], 4.0);
    leg.candles[2].low = 70.0;
    let mut one = request(vec![leg]);
    one.sim.leverage = 5.0;
    let report = run(&one);
    assert_eq!(
        report.liquidated_at,
        Some(report.times[2]),
        "{:?}",
        report.notes
    );
}

#[test]
fn a_bar_missing_from_one_leg_keeps_every_moment_valued_together() {
    // B has no bar at moment 2: it's valued at its last close, while A carries on.
    let a = holding("AAAUSDT", &[100.0, 100.0, 110.0, 120.0], 0.5);
    let mut b = holding("BBBUSDT", &[50.0, 50.0, 999.0, 60.0], 0.5);
    b.candles.remove(2);
    let report = run(&request(vec![a, b]));
    assert_eq!(report.times.len(), 4);
    // 500 of A and 1,000 of B, bought at moment 0.
    assert!(close(report.equity[2], 100_000.0 + 500.0 * 10.0));
    assert!(close(
        report.equity[3],
        100_000.0 + 500.0 * 20.0 + 1_000.0 * 10.0
    ));
    assert_eq!(report.legs[1].missing_bars, 1);
    assert_eq!(report.legs[1].real, vec![true, true, false, true]);
    assert!(close(report.legs[1].closes[2], 50.0));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.text.contains("BBBUSDT had no bar"))
    );
}

#[test]
fn nothing_trades_on_a_bar_that_isnt_there() {
    let a = holding("AAAUSDT", &[100.0, 100.0, 110.0, 120.0], 0.5);
    let mut b = holding("BBBUSDT", &[50.0, 50.0, 999.0, 60.0], 0.5);
    b.candles.remove(2);
    let mut both = request(vec![a, b]);
    let rows = vec![
        (START, vec![0.0, 0.0]),
        // Falls on moment 2 of the opens, which is when B has no bar, and its close is moment 1's.
        (START + 3 * HOUR, vec![0.1, 0.1]),
    ];
    both.weights = Some(Arc::new(rows));
    let report = run(&both);
    assert_eq!(report.legs[0].fills[0].bar, 2, "A goes at once");
    assert_eq!(report.legs[1].fills[0].bar, 3, "B has to wait for a bar");
    assert!(close(report.legs[1].fills[0].price, 60.0));
}

#[test]
fn a_late_leg_starts_at_its_first_bar_and_a_table_says_it_came_early() {
    let a = holding("AAAUSDT", &[100.0, 100.0, 100.0, 100.0], 0.0);
    let mut b = holding("BBBUSDT", &[50.0, 50.0, 50.0, 60.0], 0.0);
    b.candles.drain(..2);
    let mut both = request(vec![a, b]);
    both.weights = Some(Arc::new(vec![(START, vec![0.2, 0.2])]));
    let report = run(&both);
    assert_eq!(report.legs[1].fills.len(), 1);
    assert_eq!(report.legs[1].fills[0].bar, 2);
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.text.contains("before it had any bars"))
    );
    assert_eq!(report.legs[1].missing_bars, 0);
}

#[test]
fn funding_settles_across_legs_at_the_same_moment_even_a_little_off_the_hour() {
    // Long A pays and short B receives: one's stamped a moment before the hour and the other after.
    let (hour, off) = (START + 2 * HOUR, 3_000_000);
    let mut a = holding("AAAUSDT-PERP", &[100.0, 100.0, 100.0, 100.0, 100.0], 0.5);
    a.funding = Some(Arc::new(vec![(hour - off, 0.001)]));
    let mut b = holding("BBBUSDT-PERP", &[100.0, 100.0, 100.0, 100.0, 100.0], -0.5);
    b.funding = Some(Arc::new(vec![(hour + off, 0.001)]));
    let report = run(&request(vec![a, b]));
    assert!(close(report.legs[0].funding_paid, 50.0));
    assert!(close(report.legs[1].funding_paid, -50.0));
    // They net out, on every step: there's never one settled and not the other.
    for equity in &report.equity {
        assert!(close(*equity, 100_000.0), "{:?}", report.equity);
    }
}

#[test]
fn funding_only_touches_the_leg_it_belongs_to() {
    let mut a = holding("AAAUSDT-PERP", &[100.0, 100.0, 100.0, 100.0], 0.5);
    a.funding = Some(Arc::new(vec![(START + 2 * HOUR, 0.001)]));
    let b = holding("BBBUSDT", &[100.0, 100.0, 100.0, 100.0], 0.5);
    let report = run(&request(vec![a, b]));
    assert!(close(report.legs[0].funding_paid, 50.0));
    assert_eq!(report.legs[1].funding_paid, 0.0);
    assert!(close(*report.equity.last().unwrap(), 100_000.0 - 50.0));
}

#[test]
fn a_short_on_a_perpetual_pays_no_borrowing_but_one_on_spot_does() {
    let mut perp = holding("AAAUSDT-PERP", &[100.0, 100.0, 100.0], -0.5);
    perp.funding = Some(Arc::new(vec![(START, 0.0)]));
    let spot = holding("BBBUSDT", &[100.0, 100.0, 100.0], -0.5);
    let mut both = request(vec![perp, spot]);
    both.sim.borrow_percent_per_year = Some(100.0);
    let report = run(&both);
    assert_eq!(report.legs[0].borrow_paid, 0.0);
    assert!(report.legs[1].borrow_paid > 0.0);
}

#[test]
fn a_table_changes_the_book_at_the_first_close_at_or_after_its_time() {
    let a = holding("AAAUSDT", &[100.0, 100.0, 100.0, 100.0, 100.0], 0.0);
    let b = holding("BBBUSDT", &[100.0, 100.0, 100.0, 100.0, 100.0], 0.0);
    let mut both = request(vec![a, b]);
    both.weights = Some(Arc::new(vec![
        (START + HOUR, vec![0.5, -0.5]),
        // Between closes, so it waits for the next.
        (START + 3 * HOUR + 1, vec![0.0, 0.25]),
    ]));
    let report = run(&both);
    let of = |leg: usize| -> Vec<(usize, Side, f64)> {
        report.legs[leg]
            .fills
            .iter()
            .map(|f| (f.bar, f.side, f.qty))
            .collect()
    };
    assert_eq!(of(0), vec![(0, Side::Buy, 500.0), (3, Side::Sell, 500.0)]);
    assert_eq!(of(1), vec![(0, Side::Sell, 500.0), (3, Side::Buy, 750.0)]);
}

#[test]
fn rebalancing_goes_back_to_the_weights_and_small_trades_are_skipped() {
    let closes = [100.0, 100.0, 200.0, 200.0];
    let flat = [100.0, 100.0, 100.0, 100.0];
    let legs = || {
        vec![
            holding("AAAUSDT", &closes, 0.5),
            holding("BBBUSDT", &flat, 0.5),
        ]
    };
    let mut every = request(legs());
    every.rebalance.every_bars = Some(2);
    let report = run(&every);
    // Bars 1 and 3 rebalance. A doubled at 2: 1,000 are worth 200,000 of an account of 200,000 total with
    // B's 50,000 and 50,000 of cash... at bar 3 the account is 150,000, so 75,000 in each.
    let last = |leg: usize| *report.legs[leg].values.last().unwrap();
    assert!(close(last(0), 75_000.0), "{}", last(0));
    assert!(close(last(1), 75_000.0), "{}", last(1));

    let mut skipped = request(legs());
    skipped.rebalance.every_bars = Some(2);
    skipped.rebalance.min_trade_value = 1_000_000.0;
    let report = run(&skipped);
    assert!(
        report.legs.iter().all(|leg| leg.fills.is_empty()),
        "nothing is worth a million"
    );
}

#[test]
fn rebalancing_on_drift_waits_for_the_drift() {
    let closes = [100.0, 100.0, 101.0, 200.0, 200.0];
    let mut one = request(vec![holding("AAAUSDT", &closes, 0.5)]);
    one.rebalance.drift_percent = Some(5.0);
    let report = run(&one);
    let bars: Vec<usize> = report.legs[0].fills.iter().map(|fill| fill.bar).collect();
    assert_eq!(bars, vec![0, 3], "1% is nothing, and doubling is plenty");
}

#[test]
fn one_leg_straying_takes_the_whole_book_back() {
    let a = holding("AAAUSDT", &[100.0, 100.0, 200.0, 200.0], 0.25);
    let b = holding("BBBUSDT", &[100.0, 100.0, 100.0, 100.0], 0.25);
    let mut both = request(vec![a, b]);
    both.rebalance.drift_percent = Some(5.0);
    let report = run(&both);
    // B hardly moved as a share, but goes back with A. The account's 125,000 by then: 31,250 in each.
    let bars =
        |leg: usize| -> Vec<usize> { report.legs[leg].fills.iter().map(|f| f.bar).collect() };
    assert_eq!(bars(0), vec![0, 2]);
    assert_eq!(bars(1), vec![0, 2]);
    assert!(
        close(report.legs[0].values[3], 31_250.0),
        "{}",
        report.legs[0].values[3]
    );
    assert!(
        close(report.legs[1].values[3], 31_250.0),
        "{}",
        report.legs[1].values[3]
    );
}

/// Bars with opens of their own: (open, low, close).
fn with_opens(symbol: &str, bars: &[(f64, f64, f64)], weight: f64) -> PortfolioLeg {
    let mut leg = holding(symbol, &[100.0], weight);
    leg.candles = bars
        .iter()
        .enumerate()
        .map(|(at, &(open, low, close))| Candle {
            open,
            low,
            high: open.max(close),
            close,
            ..candle(at, close)
        })
        .collect();
    leg
}

fn at_next_open(legs: Vec<PortfolioLeg>) -> PortfolioRequest {
    let mut asked = request(legs);
    asked.sim.next_open = true;
    asked
}

#[test]
fn every_leg_fills_at_its_own_next_open_at_the_same_moment() {
    // Decided at the first close, so both go at the second bar's open, however it gapped.
    let a = with_opens(
        "AAAUSDT",
        &[
            (100.0, 100.0, 100.0),
            (110.0, 110.0, 120.0),
            (120.0, 120.0, 130.0),
        ],
        0.3,
    );
    let b = with_opens(
        "BBBUSDT",
        &[(50.0, 50.0, 50.0), (40.0, 40.0, 45.0), (45.0, 45.0, 45.0)],
        -0.2,
    );
    let report = run(&at_next_open(vec![a, b]));
    let first = |leg: usize| report.legs[leg].fills[0];
    // 30,000 of A at the close of 100 is 300, and 20,000 of B at 50 is 400.
    assert_eq!(
        (first(0).bar, first(0).price, first(0).qty),
        (1, 110.0, 300.0)
    );
    assert_eq!(
        (first(1).bar, first(1).price, first(1).qty),
        (1, 40.0, 400.0)
    );
    assert_eq!(first(0).ts, first(1).ts);
    // A made 20 on 300 by the end, and B lost 5 on 400.
    assert!(close(
        *report.equity.last().unwrap(),
        100_000.0 + 6_000.0 - 2_000.0
    ));
    // At the first close nothing's held yet.
    assert!(close(report.equity[0], 100_000.0));
}

#[test]
fn a_leg_with_no_next_bar_fills_at_the_open_of_the_bar_it_next_has() {
    let a = with_opens(
        "AAAUSDT",
        &[
            (100.0, 100.0, 100.0),
            (101.0, 101.0, 101.0),
            (102.0, 102.0, 102.0),
            (103.0, 103.0, 103.0),
        ],
        0.1,
    );
    let mut b = with_opens(
        "BBBUSDT",
        &[
            (50.0, 50.0, 50.0),
            (51.0, 51.0, 51.0),
            (52.0, 52.0, 52.0),
            (53.0, 53.0, 53.0),
        ],
        0.1,
    );
    b.candles.remove(1);
    let report = run(&at_next_open(vec![a, b]));
    assert_eq!(report.legs[0].fills[0].bar, 1);
    assert_eq!(
        (report.legs[1].fills[0].bar, report.legs[1].fills[0].price),
        (2, 52.0)
    );
}

#[test]
fn slices_each_go_at_a_next_open_and_liquidation_takes_the_rest_at_once() {
    let bars = [
        (100.0, 100.0, 100.0),
        (100.0, 100.0, 100.0),
        (100.0, 100.0, 100.0),
        (100.0, 100.0, 100.0),
        // Out of margin at this close, so everything goes at the next open, lower still.
        (100.0, 75.0, 75.2),
        (70.0, 70.0, 70.0),
        (90.0, 90.0, 90.0),
    ];
    let mut asked = at_next_open(vec![with_opens("AAAUSDT", &bars, 4.0)]);
    asked.sim.leverage = 5.0;
    asked.sim.max_participation_percent = Some(20.0);
    let report = run(&asked);
    let sizes: Vec<(usize, Side, f64, f64)> = report.legs[0]
        .fills
        .iter()
        .map(|fill| (fill.bar, fill.side, fill.qty, fill.price))
        .collect();
    assert_eq!(
        sizes,
        vec![
            (1, Side::Buy, 2_000.0, 100.0),
            (2, Side::Buy, 2_000.0, 100.0),
            (5, Side::Sell, 4_000.0, 70.0),
        ],
        "{:?}",
        report.notes
    );
    assert_eq!(report.liquidated_at, Some(report.times[4]));
    // Past what it had: 30 lost on each of 4,000 from 100,000.
    assert!(close(*report.equity.last().unwrap(), -20_000.0));
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.text.contains("depleted"))
    );
}

#[test]
fn a_table_row_for_a_bar_goes_at_the_open_after_its_close() {
    let a = with_opens(
        "AAAUSDT",
        &[
            (100.0, 100.0, 100.0),
            (101.0, 101.0, 102.0),
            (103.0, 103.0, 104.0),
        ],
        0.0,
    );
    let mut asked = at_next_open(vec![a]);
    asked.weights = Some(Arc::new(vec![(START + HOUR, vec![0.5])]));
    let report = run(&asked);
    let fill = report.legs[0].fills[0];
    assert_eq!((fill.bar, fill.price), (1, 101.0));
}

#[test]
fn cash_below_zero_pays_interest_by_hand_and_none_above_it() {
    const YEAR_HOURS: f64 = 365.0 * 24.0;
    // 200,000 held on 100,000 leaves 100,000 borrowed, at 10% a year, for three hours.
    let mut borrowed = request(vec![holding("AAAUSDT", &[100.0; 4], 2.0)]);
    borrowed.sim.leverage = 3.0;
    borrowed.sim.cash_interest_percent_per_year = Some(10.0);
    let report = run(&borrowed);
    let hour = 100_000.0 * 0.1 / YEAR_HOURS;
    // Just over three hours' worth, as interest unpaid goes on the loan.
    assert!(
        report.interest_paid > 3.0 * hour,
        "{}",
        report.interest_paid
    );
    assert!(
        report.interest_paid < 3.0 * hour * 1.001,
        "{}",
        report.interest_paid
    );
    let left = report.equity[3];
    assert!(
        (left - (100_000.0 - report.interest_paid)).abs() < 1e-6,
        "{left}"
    );

    let mut funded = request(vec![holding("AAAUSDT", &[100.0; 4], 0.5)]);
    funded.sim.cash_interest_percent_per_year = Some(10.0);
    assert_eq!(run(&funded).interest_paid, 0.0);
}

#[test]
fn a_perpetual_is_no_loan_so_it_pays_funding_and_not_interest() {
    let mut perp = holding("AAAUSDT-PERP", &[100.0; 4], 2.0);
    perp.funding = Some(Arc::new(vec![(START + HOUR, 0.0)]));
    let mut asked = request(vec![perp]);
    asked.sim.leverage = 3.0;
    asked.sim.cash_interest_percent_per_year = Some(10.0);
    assert_eq!(run(&asked).interest_paid, 0.0);

    // Still a perpetual when no funding fell in its span, so still no loan, and no borrowing.
    let mut quiet = holding("AAAUSDT-PERP", &[100.0; 4], -2.0);
    quiet.funding = Some(Arc::new(Vec::new()));
    let mut asked = request(vec![quiet]);
    asked.sim.leverage = 3.0;
    asked.sim.cash_interest_percent_per_year = Some(10.0);
    asked.sim.borrow_percent_per_year = Some(10.0);
    let report = run(&asked);
    assert_eq!(report.interest_paid, 0.0);
    assert_eq!(report.borrow_paid, 0.0);
}

#[test]
fn interest_counts_as_a_cost_and_grows_a_compounding_book_less() {
    let mut asked = request(vec![holding("AAAUSDT", &[100.0; 20], 2.0)]);
    asked.sim.leverage = 3.0;
    asked.sim.cash_interest_percent_per_year = Some(100.0);
    let report = run(&asked);
    assert!(report.interest_paid > 0.0);
    assert!(*report.equity.last().unwrap() < 100_000.0);
}

#[test]
fn what_cant_be_done_says_so() {
    let leg = || holding("AAAUSDT", &[100.0, 100.0], 0.5);
    let wrong = |request: PortfolioRequest| run_portfolio(&request).unwrap_err().to_string();
    assert!(wrong(request(vec![])).contains("at least one"));
    assert!(wrong(request(vec![leg(), leg()])).contains("twice"));
    let mut other = holding("BBBEUR", &[1.0, 1.0], 0.5);
    other.instrument.quote = "EUR".into();
    assert!(wrong(request(vec![leg(), other])).contains("same currency"));
    let mut stops = request(vec![leg()]);
    stops.sim.stop_loss_percent = Some(1.0);
    assert!(wrong(stops).contains("at the market"));
    let mut delayed = at_next_open(vec![leg()]);
    delayed.sim.latency_ms = 10;
    assert!(wrong(delayed).contains("latency"));
    let mut short_row = request(vec![leg()]);
    short_row.weights = Some(Arc::new(vec![(START, vec![0.1, 0.2])]));
    assert!(wrong(short_row).contains("2 weights for 1"));
}

/// Writes down the volumes its bars have.
#[derive(Debug, Clone, Default)]
struct Listening(Arc<Mutex<Vec<f64>>>);

struct Heard(Arc<Mutex<Vec<f64>>>);

impl Signal for Heard {
    fn on_bar(&mut self, bar: &Candle) -> Result<Option<i8>, String> {
        self.0.lock().unwrap().push(bar.volume);
        Ok(None)
    }
}

impl SignalFactory for Listening {
    fn create(&self, _params: &[f64]) -> Result<Box<dyn Signal>, String> {
        Ok(Box::new(Heard(Arc::clone(&self.0))))
    }
}

fn listening_spec() -> StrategySpec {
    let info = StrategyInfo::new("Listening", "Keeps quiet", Vec::new());
    StrategySpec::from_info(StrategyKind::Plugin("listen".into()), Arc::new(info), &[])
}

#[test]
fn a_strategy_hears_the_real_volume_of_its_bars() {
    let heard = Listening::default();
    run_backtest(&BacktestRequest {
        instrument: spec("BTCUSDT"),
        bars: Timeframe::H1.into(),
        candles: candles(&[100.0, 101.0]),
        strategy: listening_spec(),
        plugin: Some(Arc::new(heard.clone())),
        ticks: None,
        starting_balance: 100_000.0,
        fees: FeeRates::ZERO,
        sim: SimSettings::default(),
        rules: TradingRules::default(),
        trade_from: 0,
        fill_bars: None,
        funding: None,
    })
    .unwrap();
    assert_eq!(*heard.0.lock().unwrap(), vec![VOLUME, VOLUME]);

    let heard = Listening::default();
    let mut leg = holding("BTCUSDT", &[100.0, 101.0], 1.0);
    leg.strategy = Some(listening_spec());
    leg.plugin = Some(Arc::new(heard.clone()));
    run(&request(vec![leg]));
    assert_eq!(*heard.0.lock().unwrap(), vec![VOLUME, VOLUME]);
}

fn capped(mut request: PortfolioRequest, percent: f64) -> PortfolioRequest {
    request.sim.max_gross_percent = Some(percent);
    request
}

#[test]
fn legs_entering_at_different_times_stack_past_the_account_unless_capped() {
    let legs = || {
        let a = holding("AAAUSDT", &[100.0, 100.0, 100.0, 100.0, 100.0], 0.6);
        let mut b = holding("BBBUSDT", &[50.0, 50.0, 50.0, 50.0, 50.0], 0.6);
        b.candles.drain(..2);
        vec![a, b]
    };
    let free = run(&request(legs()));
    assert!(close(free.peak_margin_use, 1.2), "{}", free.peak_margin_use);
    assert_eq!(free.orders_capped, 0);

    let report = run(&capped(request(legs()), 100.0));
    assert!(
        report.peak_margin_use <= 1.0 + 1e-6,
        "{}",
        report.peak_margin_use
    );
    // The first leg stays as it was; the second takes only what's left.
    assert!(close(report.legs[0].fills[0].qty, 600.0));
    assert!(
        close(report.legs[1].fills[0].qty, 800.0),
        "{:?}",
        report.legs[1].fills
    );
    assert_eq!(report.orders_capped, 1);
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.text.contains("1 orders were cut"))
    );
    assert!(
        !report
            .notes
            .iter()
            .any(|note| note.text.contains("in margin"))
    );
}

#[test]
fn a_cap_cuts_legs_growing_together_by_the_same_share() {
    let closes = [100.0, 100.0, 100.0];
    let legs = || {
        vec![
            holding("AAAUSDT", &closes, 0.8),
            holding("BBBUSDT", &closes, -0.8),
        ]
    };
    let report = run(&capped(request(legs()), 100.0));
    assert!(close(report.legs[0].fills[0].qty, 500.0));
    assert!(close(report.legs[1].fills[0].qty, 500.0));
    assert_eq!(report.legs[1].fills[0].side, Side::Sell);
    assert_eq!(report.orders_capped, 2);
    // With leverage, the account goes that much further.
    let mut levered = capped(request(legs()), 100.0);
    levered.sim.leverage = 2.0;
    let report = run(&levered);
    assert_eq!(report.orders_capped, 0);
    assert!(close(report.legs[0].fills[0].qty, 800.0));
}

#[test]
fn a_cap_that_never_binds_changes_nothing() {
    let legs = || {
        vec![
            PortfolioLeg {
                candles: wave(300),
                ..holding("AAAUSDT", &[], 0.5)
            },
            PortfolioLeg {
                candles: wave(300),
                ..holding("BBBUSDT", &[], -0.3)
            },
        ]
    };
    let mut free = request(legs());
    free.rebalance.every_bars = Some(10);
    let mut wide = capped(request(legs()), 1_000.0);
    wide.rebalance.every_bars = Some(10);
    let (free, wide) = (run(&free), run(&wide));
    assert_eq!(free.equity, wide.equity);
    assert_eq!(free.total_orders, wide.total_orders);
    assert_eq!(wide.orders_capped, 0);
}

#[test]
fn rebalancing_under_a_cap_keeps_the_whole_book_within_it() {
    let rising = [100.0, 100.0, 120.0, 140.0, 160.0, 180.0];
    let legs = vec![
        holding("AAAUSDT", &rising, 0.7),
        holding("BBBUSDT", &[100.0; 6], 0.7),
    ];
    let mut book = capped(request(legs), 100.0);
    book.rebalance.every_bars = Some(1);
    let report = run(&book);
    // Bought at one close, valued at the next: within a step of the cap.
    for (at, gross) in report.gross_exposure.iter().enumerate().skip(1) {
        assert!(*gross <= 1.0 + 0.12, "{gross} at {at}");
    }
    assert!(report.orders_capped >= 2);
}
