//! Limit orders, stops and targets, on bars worked out by hand. Bars can't show a place in the queue,
//! so what matters most here is that nothing fills that might not really have.

use std::{collections::BTreeMap, sync::Arc};

use persephone_core::{
    Candle, InstrumentSpec, Side, Timeframe,
    signal::{Signal, SignalFactory},
    strategy::{StrategyInfo, StrategyKind, StrategySpec},
};
use persephone_engine::{
    BacktestReport, BacktestRequest, FeeRates, LimitOrders, SimSettings, TradingRules,
    spawn_backtest,
};

const HOUR: u64 = 3_600_000_000_000;
const START: u64 = 1_735_689_600_000_000_000;
/// Every bar's traded size.
const VOLUME: f64 = 10_000.0;

/// (open, high, low, close)
type Ohlc = (f64, f64, f64, f64);

/// Asks for a position at given bars, and keeps quiet otherwise.
#[derive(Debug, Clone)]
struct Script(BTreeMap<usize, i8>);

struct Running(BTreeMap<usize, i8>, usize);

impl Signal for Running {
    fn on_bar(&mut self, _bar: &Candle) -> Result<Option<i8>, String> {
        let wanted = self.0.get(&self.1).copied();
        self.1 += 1;
        Ok(wanted)
    }
}

impl SignalFactory for Script {
    fn create(&self, _params: &[f64]) -> Result<Box<dyn Signal>, String> {
        Ok(Box::new(Running(self.0.clone(), 0)))
    }
}

fn run(bars: &[Ohlc], wants: &[(usize, i8)], sim: SimSettings, fees: FeeRates) -> BacktestReport {
    run_with_funding(bars, wants, sim, fees, None)
}

fn run_with_funding(
    bars: &[Ohlc],
    wants: &[(usize, i8)],
    sim: SimSettings,
    fees: FeeRates,
    funding: Option<Vec<(u64, f64)>>,
) -> BacktestReport {
    let candles = bars
        .iter()
        .enumerate()
        .map(|(i, &(open, high, low, close))| Candle {
            ts: START + i as u64 * HOUR,
            open,
            high,
            low,
            close,
            volume: VOLUME,
        })
        .collect();
    let info = StrategyInfo::new("Script", "A script", Vec::new());
    let request = BacktestRequest {
        instrument: InstrumentSpec {
            symbol: "BTCUSDT".into(),
            venue: "BINANCE".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 5,
        },
        bars: Timeframe::H1.into(),
        candles,
        strategy: StrategySpec::from_info(
            StrategyKind::Plugin("script".into()),
            Arc::new(info),
            &[],
        ),
        plugin: Some(Arc::new(Script(wants.iter().copied().collect()))),
        ticks: None,
        starting_balance: 100_000.0,
        fees,
        sim,
        rules: TradingRules::default(),
        trade_from: 0,
        fill_bars: None,
        funding: funding.map(Arc::new),
    };
    spawn_backtest(request).join().unwrap().unwrap()
}

fn limit(offset_percent: f64, expires_bars: u32) -> SimSettings {
    SimSettings {
        limit: Some(LimitOrders {
            offset_percent,
            expires_bars,
            through_percent: 0.0,
        }),
        ..SimSettings::default()
    }
}

const FLAT: Ohlc = (100.0, 100.5, 99.5, 100.0);
const NO_FEES: FeeRates = FeeRates::ZERO;

/// Long at bar 1's close of 100, as a limit 1% under, so at 99.
fn entry(next: Ohlc) -> BacktestReport {
    run(&[FLAT, FLAT, next, FLAT], &[(1, 1)], limit(1.0, 1), NO_FEES)
}

#[test]
fn a_limit_inside_the_bars_range_fills_at_its_price_as_a_maker() {
    // Opened above, traded through 99 to 98, and came back.
    let report = entry((100.0, 100.5, 98.0, 99.5));
    assert_eq!(report.fills.len(), 1, "{:?}", report.fills);
    let fill = &report.fills[0];
    assert_eq!(fill.side, Side::Buy);
    assert!((fill.price - 99.0).abs() < 1e-9, "{fill:?}");
    assert!(fill.maker, "{fill:?}");
    assert_eq!((report.limit_orders, report.limits_filled), (1, 1));
}

#[test]
fn a_limit_price_never_reached_doesnt_fill() {
    let report = entry((100.0, 100.5, 99.5, 100.0));
    assert!(report.fills.is_empty(), "{:?}", report.fills);
    assert_eq!((report.limit_orders, report.limits_filled), (1, 0));
    assert!(report.notes.is_empty(), "{:?}", report.notes);
}

#[test]
fn a_limit_only_touched_doesnt_fill() {
    // The low is exactly 99. Others may have been ahead in the queue and taken all there was.
    let report = entry((100.0, 100.5, 99.0, 100.0));
    assert!(report.fills.is_empty(), "{:?}", report.fills);
}

#[test]
fn a_gap_through_a_limit_fills_no_better_than_its_price() {
    // Opened at 95, well under 99. Filling at 95 would assume a price nobody's order rested at.
    let report = entry((95.0, 96.0, 94.0, 95.5));
    assert_eq!(report.fills.len(), 1, "{:?}", report.fills);
    assert!(
        (report.fills[0].price - 99.0).abs() < 1e-9,
        "{:?}",
        report.fills[0]
    );
}

#[test]
fn a_short_limit_rests_above_and_mirrors_it() {
    let bars = |high| [FLAT, FLAT, (100.0, high, 99.5, 100.0), FLAT];
    let wants = [(1, -1)];
    assert!(
        run(&bars(101.0), &wants, limit(1.0, 1), NO_FEES)
            .fills
            .is_empty(),
        "a touch"
    );
    let report = run(&bars(102.0), &wants, limit(1.0, 1), NO_FEES);
    assert_eq!(report.fills.len(), 1);
    assert_eq!(report.fills[0].side, Side::Sell);
    assert!((report.fills[0].price - 101.0).abs() < 1e-9);
}

#[test]
fn a_limit_can_wait_and_then_lapses() {
    let quiet = (100.0, 100.5, 99.5, 100.0);
    let through = (100.0, 100.5, 98.0, 99.5);
    // Resting for three bars: the third bar reaches it.
    let report = run(
        &[FLAT, FLAT, quiet, quiet, through, FLAT],
        &[(1, 1)],
        limit(1.0, 3),
        NO_FEES,
    );
    assert_eq!(report.fills.len(), 1, "{:?}", report.fills);
    // Resting for one: it's gone by then, and nothing's held.
    let report = run(
        &[FLAT, FLAT, quiet, quiet, through, FLAT],
        &[(1, 1)],
        limit(1.0, 1),
        NO_FEES,
    );
    assert!(report.fills.is_empty(), "{:?}", report.fills);
    assert!(report.notes.is_empty(), "no fault: {:?}", report.notes);
}

#[test]
fn an_exit_that_lapses_still_happens_at_the_market() {
    let quiet = FLAT;
    let up = (100.0, 102.0, 98.0, 100.0);
    // In at bar 2 (99), then told to get out at bar 3, 1% above the close, which nothing reaches.
    let report = run(
        &[FLAT, FLAT, up, quiet, quiet, quiet, quiet],
        &[(1, 1), (3, 0)],
        limit(1.0, 1),
        NO_FEES,
    );
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert!(!report.fills[1].maker, "{:?}", report.fills[1]);
    assert_eq!(report.fills[1].side, Side::Sell);
}

#[test]
fn a_makers_fee_is_charged_on_a_limit_and_a_takers_on_a_market_order() {
    let fees = FeeRates {
        maker: 0.0002,
        taker: 0.001,
    };
    let bars = [FLAT, FLAT, (100.0, 100.5, 98.0, 99.5), FLAT];
    let maker = run(&bars, &[(1, 1)], limit(1.0, 1), fees);
    let fill = &maker.fills[0];
    assert!(
        (fill.fee - fill.qty * 99.0 * 0.0002).abs() < 0.01,
        "{fill:?}"
    );
    let taker = run(&bars, &[(1, 1)], SimSettings::default(), fees);
    let fill = &taker.fills[0];
    assert!(!fill.maker);
    assert!(
        (fill.fee - fill.qty * fill.price * 0.001).abs() < 0.01,
        "{fill:?}"
    );
}

fn guarded(stop: Option<f64>, target: Option<f64>) -> SimSettings {
    SimSettings {
        stop_loss_percent: stop,
        take_profit_percent: target,
        ..SimSettings::default()
    }
}

#[test]
fn a_stop_fills_inside_the_bar_and_at_the_open_on_a_gap() {
    // In at bar 1's close of 100, with a 2% stop at 98.
    let inside = (100.0, 100.5, 97.0, 99.0);
    let report = run(
        &[FLAT, FLAT, FLAT, inside, FLAT],
        &[(1, 1)],
        guarded(Some(2.0), None),
        NO_FEES,
    );
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert!(
        (report.fills[1].price - 98.0).abs() < 0.2,
        "{:?}",
        report.fills[1]
    );

    // Opening at 90, it can only get out around there, not at 98.
    let gap = (90.0, 91.0, 89.0, 90.5);
    let report = run(
        &[FLAT, FLAT, FLAT, gap, FLAT],
        &[(1, 1)],
        guarded(Some(2.0), None),
        NO_FEES,
    );
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert!(report.fills[1].price < 92.0, "{:?}", report.fills[1]);
    // Nothing left resting opens anything afterwards, and nothing's called a fault.
    assert!(report.notes.is_empty(), "{:?}", report.notes);
}

#[test]
fn a_target_fills_at_its_price_and_takes_the_stop_with_it() {
    let up = (100.0, 104.0, 99.5, 103.0);
    let down = (103.0, 103.5, 90.0, 91.0);
    let report = run(
        &[FLAT, FLAT, FLAT, up, down, FLAT],
        &[(1, 1)],
        guarded(Some(2.0), Some(3.0)),
        NO_FEES,
    );
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert!(
        (report.fills[1].price - 103.0).abs() < 1e-9,
        "{:?}",
        report.fills[1]
    );
    assert!(report.fills[1].maker);
}

#[test]
fn a_bar_reaching_both_counts_as_the_stop_for_longs_and_shorts() {
    let wide = (100.0, 105.0, 95.0, 100.0);
    let long = run(
        &[FLAT, FLAT, FLAT, wide, FLAT],
        &[(1, 1)],
        guarded(Some(2.0), Some(3.0)),
        NO_FEES,
    );
    assert_eq!(long.fills.len(), 2, "{:?}", long.fills);
    assert!(long.fills[1].price < 100.0, "the stop: {:?}", long.fills[1]);
    let short = run(
        &[FLAT, FLAT, FLAT, wide, FLAT],
        &[(1, -1)],
        guarded(Some(2.0), Some(3.0)),
        NO_FEES,
    );
    assert_eq!(short.fills.len(), 2, "{:?}", short.fills);
    assert!(
        short.fills[1].price > 100.0,
        "the stop: {:?}",
        short.fills[1]
    );
}

#[test]
fn a_new_position_cancels_the_old_guards() {
    // Stop guards a long, then the strategy flips short: the old stop mustn't sell again.
    let report = run(
        &[FLAT, FLAT, FLAT, FLAT, (100.0, 100.5, 90.0, 95.0), FLAT],
        &[(1, 1), (3, -1)],
        guarded(Some(5.0), None),
        NO_FEES,
    );
    // Buy, flip, and nothing more: the short's own stop is above.
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
}

#[test]
fn a_position_already_past_its_guards_at_the_next_close_still_gets_out() {
    // A limit entry fills at 99 in a bar closing at 110: past a 3% target of 101.97 already.
    let bars = [
        FLAT,
        FLAT,
        (100.0, 111.0, 98.0, 110.0),
        (110.0, 110.5, 109.5, 110.0),
        FLAT,
    ];
    let sim = SimSettings {
        take_profit_percent: Some(3.0),
        ..limit(1.0, 1)
    };
    let report = run(&bars, &[(1, 1)], sim, NO_FEES);
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert_eq!(report.fills[1].side, Side::Sell);
    assert!(report.notes.is_empty(), "no refusal: {:?}", report.notes);

    // And in a bar closing at 90: past a 3% stop of 96.03.
    let bars = [
        FLAT,
        FLAT,
        (100.0, 100.5, 89.0, 90.0),
        (90.0, 90.5, 89.5, 90.0),
        FLAT,
    ];
    let sim = SimSettings {
        stop_loss_percent: Some(3.0),
        ..limit(1.0, 1)
    };
    let report = run(&bars, &[(1, 1)], sim, NO_FEES);
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert_eq!(report.fills[1].side, Side::Sell);
    assert!(report.notes.is_empty(), "no refusal: {:?}", report.notes);
}

#[test]
fn a_market_entrys_guards_rest_through_the_very_next_bar() {
    // In at bar 1's close of 100, and bar 2 goes straight through a 2% stop at 98 and comes back.
    let through = (100.0, 100.5, 97.0, 99.5);
    let report = run(
        &[FLAT, FLAT, through, FLAT, FLAT],
        &[(1, 1)],
        guarded(Some(2.0), None),
        NO_FEES,
    );
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert!(
        (report.fills[1].price - 98.0).abs() < 1e-9,
        "{:?}",
        report.fills[1]
    );
    assert_eq!(report.fills[1].bar, 2, "{:?}", report.fills[1]);
    assert!(report.notes.is_empty(), "{:?}", report.notes);

    // A target the same, and at its price as a maker, however high the close.
    let up = (100.0, 106.0, 99.5, 105.0);
    let report = run(
        &[FLAT, FLAT, up, FLAT, FLAT],
        &[(1, 1)],
        guarded(None, Some(3.0)),
        NO_FEES,
    );
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert!(
        (report.fills[1].price - 103.0).abs() < 1e-9,
        "{:?}",
        report.fills[1]
    );
    assert!(report.fills[1].maker, "{:?}", report.fills[1]);

    // Reaching both on that bar, the stop came first, even closing beyond the target.
    let wide = (100.0, 106.0, 97.0, 105.0);
    let report = run(
        &[FLAT, FLAT, wide, FLAT, FLAT],
        &[(1, 1)],
        guarded(Some(2.0), Some(3.0)),
        NO_FEES,
    );
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert!(
        (report.fills[1].price - 98.0).abs() < 1e-9,
        "{:?}",
        report.fills[1]
    );
}

#[test]
fn what_cant_be_modelled_together_says_so() {
    let candles = [FLAT, FLAT];
    let tried = |sim: SimSettings| {
        let bars = candles;
        std::panic::catch_unwind(move || run(&bars, &[], sim, NO_FEES)).is_err()
    };
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let refused = [
        SimSettings {
            latency_ms: 100,
            ..limit(1.0, 1)
        },
        SimSettings {
            next_open: true,
            ..limit(1.0, 1)
        },
        SimSettings {
            next_open: true,
            ..guarded(Some(2.0), None)
        },
        SimSettings {
            next_open: true,
            ..guarded(None, Some(2.0))
        },
    ]
    .map(tried);
    std::panic::set_hook(previous);
    assert_eq!(refused, [true; 4]);
}

#[test]
fn funding_on_the_hour_falls_to_what_was_held_going_into_it() {
    // In at bar 1's close, which is bar 2's open. A rate exactly then is paid by what's just been
    // bought, and one stamped a moment later the same; a rate at bar 1's open isn't paid at all.
    let bars = [FLAT, FLAT, FLAT, FLAT];
    let on_the_hour = |offset: u64| {
        let funding = vec![(START + HOUR, 0.5), (START + 2 * HOUR + offset, 0.001)];
        run_with_funding(
            &bars,
            &[(1, 1)],
            SimSettings::default(),
            NO_FEES,
            Some(funding),
        )
        .funding_paid
    };
    assert!((on_the_hour(0) - 10.0).abs() < 1e-6, "{}", on_the_hour(0));
    assert!(
        (on_the_hour(1_000_000) - 10.0).abs() < 1e-6,
        "{}",
        on_the_hour(1_000_000)
    );
}

#[test]
fn a_required_trade_through_holds_it_back_but_it_pays_what_it_offered() {
    let needing = |through_percent| SimSettings {
        limit: Some(LimitOrders {
            offset_percent: 1.0,
            expires_bars: 1,
            through_percent,
        }),
        ..SimSettings::default()
    };
    let fees = FeeRates {
        maker: 0.001,
        taker: 0.001,
    };
    // Offered at 99. A half a percent past that is 98.505.
    let bars = |low| [FLAT, FLAT, (100.0, 100.5, low, 99.5), FLAT];
    let run_at = |low, through| run(&bars(low), &[(1, 1)], needing(through), fees);
    assert_eq!(
        run_at(98.6, 0.0).fills.len(),
        1,
        "no requirement: through is enough"
    );
    assert!(run_at(98.6, 0.5).fills.is_empty(), "not far enough");
    let report = run_at(98.0, 0.5);
    assert_eq!(report.fills.len(), 1);
    let fill = report.fills[0];
    assert!((fill.price - 99.0).abs() < 1e-9, "{fill:?}");
    assert!(
        (fill.fee - fill.qty * 99.0 * 0.001).abs() < 0.001,
        "{fill:?}"
    );
    // The account holds it at what it paid: bought at 99, worth 99.5, then 100, less its fee.
    let expected = 100_000.0 + fill.qty - fill.fee;
    assert!(
        (report.equity.last().unwrap() - expected).abs() < 1e-6,
        "{:?}",
        report.equity
    );

    // A short the same way, and a stop's and target's entry price is what it paid.
    let bars = [FLAT, FLAT, (100.0, 102.0, 99.5, 100.0), FLAT];
    let report = run(&bars, &[(1, -1)], needing(0.5), NO_FEES);
    assert!(
        (report.fills[0].price - 101.0).abs() < 1e-9,
        "{:?}",
        report.fills[0]
    );
}

#[test]
fn every_limit_order_is_logged_with_what_became_of_it_and_what_the_market_did() {
    use persephone_engine::LimitOutcome::{Filled, Lapsed, Open, Replaced};
    // Bar 1's close of 100 sends a buy at 99, which bar 2 trades 1 through, closing at 101.
    let filled = run(
        &[
            FLAT,
            FLAT,
            (100.0, 101.5, 98.0, 101.0),
            (101.0, 102.0, 100.5, 102.0),
        ],
        &[(1, 1)],
        limit(1.0, 1),
        NO_FEES,
    );
    let row = filled.limit_log[0];
    assert_eq!((row.outcome, row.side, row.bars), (Filled, Side::Buy, 1));
    assert_eq!(row.placed, START + 2 * HOUR);
    assert_eq!(row.ended, filled.fills[0].ts);
    assert!((row.price - 99.0).abs() < 1e-9, "{row:?}");
    assert!(
        (row.reached_percent - (1.0 / 99.0 * 100.0)).abs() < 1e-9,
        "{row:?}"
    );
    assert!((row.move_1_percent.unwrap() - 1.0).abs() < 1e-9, "{row:?}");
    assert_eq!(row.move_24_percent, None, "not enough bars");

    // Never reached, it lapses, and says how far off it stayed. The market then moved its way.
    let lapsed = run(
        &[FLAT, FLAT, (100.0, 103.0, 99.5, 102.0), FLAT],
        &[(1, 1)],
        limit(1.0, 1),
        NO_FEES,
    );
    let row = lapsed.limit_log[0];
    assert_eq!(row.outcome, Lapsed);
    assert!(
        (row.reached_percent - (99.0 - 99.5) / 99.0 * 100.0).abs() < 1e-9,
        "{row:?}"
    );
    assert!((row.move_1_percent.unwrap() - 2.0).abs() < 1e-9, "{row:?}");

    // A newer wish replaces it, and one left at the end stays open. A short's move counts its way.
    let replaced = run(
        &[FLAT, FLAT, FLAT, FLAT],
        &[(1, 1), (2, -1)],
        limit(1.0, 5),
        NO_FEES,
    );
    assert_eq!(replaced.limit_log.len(), 2, "{:?}", replaced.limit_log);
    assert_eq!(replaced.limit_log[0].outcome, Replaced);
    assert_eq!(replaced.limit_log[1].outcome, Open);
    assert_eq!(replaced.limit_log[1].side, Side::Sell);
}

#[test]
fn a_rebate_for_making_is_paid_to_you() {
    let fees = FeeRates {
        maker: -0.0001,
        taker: 0.001,
    };
    let report = run(
        &[FLAT, FLAT, (100.0, 100.5, 98.0, 99.5), FLAT],
        &[(1, 1)],
        limit(1.0, 1),
        fees,
    );
    let fill = &report.fills[0];
    assert!(
        (fill.fee + fill.qty * 99.0 * 0.0001).abs() < 0.01,
        "{fill:?}"
    );
}

#[test]
fn funding_is_paid_by_what_is_held_when_it_falls_due() {
    // In long at bar 1's close of 100 with 100 units, and funding of 0.1% in bars 2 and 3, of value
    // 10,000 at their opens of 100: 10 each. Before it's held, and after, nothing.
    let at = |bar: u64| START + bar * HOUR + HOUR / 2;
    let funding = vec![(at(0), 0.5), (at(2), 0.001), (at(3), 0.001)];
    let bars = [FLAT, FLAT, FLAT, FLAT, FLAT];
    let long = run_with_funding(
        &bars,
        &[(1, 1)],
        SimSettings::default(),
        NO_FEES,
        Some(funding.clone()),
    );
    assert!(
        (long.funding_paid - 20.0).abs() < 1e-6,
        "{}",
        long.funding_paid
    );
    assert!((long.equity.last().unwrap() - (100_000.0 - 20.0)).abs() < 1e-6);
    // A short receives it.
    let short = run_with_funding(
        &bars,
        &[(1, -1)],
        SimSettings::default(),
        NO_FEES,
        Some(funding),
    );
    assert!(
        (short.funding_paid + 20.0).abs() < 1e-6,
        "{}",
        short.funding_paid
    );
    assert!((short.equity.last().unwrap() - (100_000.0 + 20.0)).abs() < 1e-6);
}

#[test]
fn borrowing_charges_shorts_only() {
    // 10,000 short at 365% a year is 1% a day, so 100 a day, over three hours.
    let sim = SimSettings {
        borrow_percent_per_year: Some(365.0),
        ..SimSettings::default()
    };
    let bars = [FLAT, FLAT, FLAT, FLAT, FLAT];
    let short = run(&bars, &[(1, -1)], sim, NO_FEES);
    assert!(
        (short.borrow_paid - 100.0 * 3.0 / 24.0).abs() < 1e-6,
        "{}",
        short.borrow_paid
    );
    let long = run(&bars, &[(1, 1)], sim, NO_FEES);
    assert_eq!(long.borrow_paid, 0.0);
}

#[test]
fn impact_grows_with_the_square_root_of_an_orders_share_of_volume() {
    assert!((persephone_engine::impact_share(1.0, 0.5, 100.0, 10_000.0) - 0.001).abs() < 1e-12);
    assert_eq!(persephone_engine::impact_share(1.0, 0.5, 100.0, 0.0), 0.0);
    // 100 units on bars of 10,000: a tenth of 1%, on 100. And a sale the same way, for a 10% size of
    // 100,000 at 100.
    let sim = SimSettings {
        impact_percent: 1.0,
        ..SimSettings::default()
    };
    let bars = [FLAT, FLAT, FLAT, FLAT, FLAT];
    let report = run(&bars, &[(1, 1), (3, 0)], sim, NO_FEES);
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert!(
        (report.fills[0].price - 100.1).abs() < 1e-9,
        "{:?}",
        report.fills[0]
    );
    assert!(
        (report.fills[1].price - 99.9).abs() < 1e-9,
        "{:?}",
        report.fills[1]
    );
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    // It costs what it says: 0.1 twice, on 100 units.
    assert!((report.equity.last().unwrap() - (100_000.0 - 20.0)).abs() < 1e-6);
}

#[test]
fn with_nothing_on_it_all_goes_as_it_did() {
    let bars = [FLAT, FLAT, (100.0, 101.0, 99.0, 100.5), FLAT, FLAT];
    let report = run(&bars, &[(1, 1), (3, 0)], SimSettings::default(), NO_FEES);
    assert_eq!(report.fills.len(), 2);
    assert!(report.fills.iter().all(|fill| !fill.maker));
    assert_eq!(report.limit_orders, 0);
}

// A position here is 100 units at a price of 100, on bars each trading 10,000.

fn sized(percent: f64) -> SimSettings {
    SimSettings {
        max_participation_percent: Some(percent),
        ..SimSettings::default()
    }
}

fn close_at(close: f64) -> Ohlc {
    (close, close + 0.5, close - 0.5, close)
}

fn fills_of(report: &BacktestReport) -> Vec<(usize, Side, f64, f64)> {
    report
        .fills
        .iter()
        .map(|fill| (fill.bar, fill.side, fill.price, fill.qty))
        .collect()
}

#[test]
fn a_big_order_goes_out_in_slices_at_each_bars_price() {
    let bars = [
        FLAT,
        FLAT,
        close_at(110.0),
        close_at(120.0),
        close_at(120.0),
    ];
    let report = run(&bars, &[(1, 1)], sized(0.4), NO_FEES);
    assert_eq!(
        fills_of(&report),
        vec![
            (1, Side::Buy, 100.0, 40.0),
            (2, Side::Buy, 110.0, 40.0),
            (3, Side::Buy, 120.0, 20.0),
        ]
    );
    assert_eq!(report.orders_worked, 1);
    assert_eq!(report.bars_to_fill_longest, 3);
    assert!((report.bars_to_fill_average - 3.0).abs() < 1e-9);
    assert_eq!(report.unfilled_at_end, 0.0);
    assert!(report.notes.is_empty(), "{:?}", report.notes);
    // Bought 40 at 100, 40 at 110 and 20 at 120, and it's all at 120 now.
    let expected = 100_000.0 + 40.0 * 20.0 + 40.0 * 10.0;
    assert!((report.equity.last().unwrap() - expected).abs() < 1e-6);
}

#[test]
fn an_order_small_enough_for_its_bar_goes_at_once() {
    let bars = [FLAT, FLAT, FLAT];
    let limited = run(&bars, &[(1, 1)], sized(50.0), NO_FEES);
    let free = run(&bars, &[(1, 1)], SimSettings::default(), NO_FEES);
    assert_eq!(fills_of(&limited), fills_of(&free));
    assert_eq!(limited.orders_worked, 0);
}

#[test]
fn a_new_wish_replaces_what_is_still_to_go() {
    let bars = [FLAT, FLAT, FLAT, FLAT, FLAT, FLAT];
    // 40 in on bar 1, 40 more on bar 2, then told to go flat on bar 3.
    let report = run(&bars, &[(1, 1), (3, 0)], sized(0.4), NO_FEES);
    assert_eq!(
        fills_of(&report),
        vec![
            (1, Side::Buy, 100.0, 40.0),
            (2, Side::Buy, 100.0, 40.0),
            (3, Side::Sell, 100.0, 40.0),
            (4, Side::Sell, 100.0, 40.0),
        ]
    );
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.text.contains("replaced")),
        "{:?}",
        report.notes
    );
    assert!(!report.notes.iter().any(|note| note.text.contains("fault")));
}

#[test]
fn a_reversal_goes_through_flat_in_slices() {
    let bars = [FLAT; 8];
    // Two bars in, then two bars out and two more on the other side.
    let report = run(&bars, &[(1, 1), (4, -1)], sized(0.5), NO_FEES);
    assert_eq!(
        fills_of(&report),
        vec![
            (1, Side::Buy, 100.0, 50.0),
            (2, Side::Buy, 100.0, 50.0),
            (4, Side::Sell, 100.0, 50.0),
            (5, Side::Sell, 100.0, 50.0),
            (6, Side::Sell, 100.0, 50.0),
            (7, Side::Sell, 100.0, 50.0),
        ]
    );
    assert_eq!(report.orders_worked, 2);
    assert_eq!(report.bars_to_fill_longest, 4);
}

#[test]
fn what_never_went_out_is_reported() {
    let report = run(&[FLAT, FLAT, FLAT], &[(1, 1)], sized(0.4), NO_FEES);
    assert_eq!(report.fills.len(), 2);
    assert!((report.unfilled_at_end - 20.0).abs() < 1e-9);
    assert!(
        report
            .notes
            .iter()
            .any(|note| note.text.contains("still to go")),
        "{:?}",
        report.notes
    );
}

#[test]
fn a_limit_fills_part_of_each_bar_and_the_rest_keeps_resting() {
    let through = (100.0, 100.5, 98.0, 99.5);
    let bars = [FLAT, FLAT, through, through, through, FLAT];
    let sim = SimSettings {
        max_participation_percent: Some(0.4),
        ..limit(1.0, 10)
    };
    let report = run(&bars, &[(1, 1)], sim, NO_FEES);
    // A bar passes the limit at several prices, yet it takes 40 of a bar in all.
    assert_eq!(
        fills_of(&report),
        vec![
            (2, Side::Buy, 99.0, 40.0),
            (3, Side::Buy, 99.0, 40.0),
            (4, Side::Buy, 99.0, 20.0),
        ]
    );
    assert!(report.fills.iter().all(|fill| fill.maker));
    assert_eq!((report.limit_orders, report.limits_filled), (1, 1));
    assert_eq!(report.limit_log[0].filled, 100.0);
    assert!(
        !report.notes.iter().any(|note| note.text.contains("fault")),
        "{:?}",
        report.notes
    );
}

#[test]
fn a_limit_that_lapses_keeps_what_filled() {
    let through = (100.0, 100.5, 98.0, 99.5);
    let bars = [FLAT, FLAT, through, FLAT, FLAT, FLAT];
    let sim = SimSettings {
        max_participation_percent: Some(0.4),
        ..limit(1.0, 2)
    };
    let report = run(&bars, &[(1, 1)], sim, NO_FEES);
    assert_eq!(fills_of(&report), vec![(2, Side::Buy, 99.0, 40.0)]);
    assert_eq!(report.limit_log[0].filled, 40.0);
    assert_eq!(report.limits_filled, 0);
    assert!(
        !report.notes.iter().any(|note| note.text.contains("fault")),
        "{:?}",
        report.notes
    );
}

#[test]
fn a_stop_takes_what_the_bar_allows_and_the_rest_follows() {
    // In by bar 2. The stop at 95 takes its share of bar 3, and the rest goes at that close.
    let crash = (100.0, 100.5, 90.0, 92.0);
    let bars = [
        FLAT,
        FLAT,
        FLAT,
        crash,
        close_at(91.0),
        close_at(90.0),
        FLAT,
    ];
    let sim = SimSettings {
        max_participation_percent: Some(0.5),
        stop_loss_percent: Some(5.0),
        ..SimSettings::default()
    };
    let report = run(&bars, &[(1, 1)], sim, NO_FEES);
    assert_eq!(
        fills_of(&report),
        vec![
            (1, Side::Buy, 100.0, 50.0),
            (2, Side::Buy, 100.0, 50.0),
            (3, Side::Sell, 95.0, 50.0),
            (3, Side::Sell, 92.0, 50.0),
        ],
    );
    assert!(!report.notes.iter().any(|note| note.text.contains("fault")));
}

#[test]
fn compounding_keeps_the_size_it_set_out_for() {
    let bars = [FLAT, FLAT, close_at(20.0), close_at(20.0), close_at(20.0)];
    let sim = SimSettings {
        max_participation_percent: Some(0.4),
        compound: true,
        ..SimSettings::default()
    };
    let report = run(&bars, &[(1, 1)], sim, NO_FEES);
    let total: f64 = report.fills.iter().map(|fill| fill.qty).sum();
    assert!((total - 100.0).abs() < 1e-9, "{:?}", report.fills);
}

fn ranged(percent: f64) -> SimSettings {
    SimSettings {
        range_slippage_percent: percent,
        ..SimSettings::default()
    }
}

#[test]
fn a_market_order_pays_for_the_bar_just_closed_and_not_the_next() {
    let quiet = (100.0, 101.0, 99.0, 100.0);
    let wide = (100.0, 110.0, 90.0, 100.0);
    // Two wide bars ahead change nothing for an order at the close of a quiet one: 10% of a range of 2.
    let report = run(
        &[quiet, quiet, wide, wide],
        &[(1, 1)],
        ranged(10.0),
        NO_FEES,
    );
    assert!(
        (report.fills[0].price - 100.2).abs() < 1e-9,
        "{:?}",
        report.fills
    );
    assert!((report.fills[0].range_slip - 0.2).abs() < 1e-9);
    // Going at the close of a wide bar instead: 10% of 20, times 10 for being ten times the usual.
    let report = run(
        &[quiet, quiet, wide, quiet],
        &[(2, 1)],
        ranged(10.0),
        NO_FEES,
    );
    assert!(
        (report.fills[0].price - 120.0).abs() < 1e-9,
        "{:?}",
        report.fills
    );
}

#[test]
fn a_stop_pays_for_the_bar_it_goes_in() {
    let quiet = (100.0, 101.0, 99.0, 100.0);
    let crash = (100.0, 100.0, 80.0, 85.0);
    let sim = SimSettings {
        stop_loss_percent: Some(5.0),
        ..ranged(1.0)
    };
    let report = run(&[quiet, quiet, crash, quiet], &[(1, 1)], sim, NO_FEES);
    // The entry pays 1% of 2, so the stop's at 95% of 100.02 = 95.019. The crash is 10 times the usual
    // width, so it pays 1% of 20 times 10, which is 2.
    let entry = report.fills[0].price;
    assert!((entry - 100.02).abs() < 1e-9, "{:?}", report.fills);
    let stop = report.fills[1];
    assert!((stop.price - (95.02 - 2.0)).abs() < 0.011, "{stop:?}");
    assert!((stop.range_slip - 2.0).abs() < 1e-9, "{stop:?}");
}

#[test]
fn a_limit_pays_no_range_slippage() {
    let through = (100.0, 100.5, 95.0, 99.5);
    let sim = SimSettings {
        range_slippage_percent: 10.0,
        ..limit(1.0, 2)
    };
    let report = run(&[FLAT, FLAT, through, FLAT], &[(1, 1)], sim, NO_FEES);
    assert!(
        (report.fills[0].price - 99.0).abs() < 1e-9,
        "{:?}",
        report.fills
    );
    assert_eq!(report.fills[0].range_slip, 0.0);
}

#[test]
fn jitter_changes_nothing_on_bars_and_says_so() {
    let bars = [FLAT, close_at(101.0), close_at(102.0), close_at(103.0)];
    let plain = SimSettings {
        latency_ms: 50,
        ..SimSettings::default()
    };
    let jittered = SimSettings {
        latency_jitter_ms: 200,
        latency_seed: 7,
        ..plain
    };
    let a = run(&bars, &[(1, 1)], plain, NO_FEES);
    let b = run(&bars, &[(1, 1)], jittered, NO_FEES);
    assert_eq!(fills_of(&a), fills_of(&b));
    assert!(
        b.notes.iter().any(|note| note.text.contains("jitter")),
        "{:?}",
        b.notes
    );
}

#[test]
fn how_far_back_a_usual_bar_is_judged_from_can_be_set() {
    let quiet = (100.0, 101.0, 99.0, 100.0);
    let wide = (100.0, 110.0, 90.0, 100.0);
    // With a look-back of 1, the wide bar before it is what's usual, so this wide one's no wider.
    let bars = [quiet, wide, wide, quiet];
    let sim = |look_back| SimSettings {
        range_slippage_percent: 10.0,
        range_slippage_look_back: look_back,
        ..SimSettings::default()
    };
    let near = run(&bars, &[(2, 1)], sim(1), NO_FEES);
    assert!(
        (near.fills[0].range_slip - 2.0).abs() < 1e-9,
        "{:?}",
        near.fills
    );
    // Looking back 100, the median of 2 and 20 makes it 20 / 11 wider than usual.
    let far = run(&bars, &[(2, 1)], sim(100), NO_FEES);
    assert!(
        (far.fills[0].range_slip - 2.0 * 20.0 / 11.0).abs() < 1e-9,
        "{:?}",
        far.fills
    );
}

#[test]
fn how_steeply_impact_grows_can_be_set() {
    assert!((persephone_engine::impact_share(1.0, 1.0, 100.0, 10_000.0) - 0.0001).abs() < 1e-12);
    let bars = [FLAT, FLAT, FLAT];
    let sim = |impact_exponent| SimSettings {
        impact_percent: 10.0,
        impact_exponent,
        ..SimSettings::default()
    };
    // 100 of 10,000 is 1% of the bar: 10% times 1% in proportion, or times 10% by the square root.
    let steep = run(&bars, &[(1, 1)], sim(1.0), NO_FEES);
    assert!(
        (steep.fills[0].price - 100.1).abs() < 1e-9,
        "{:?}",
        steep.fills
    );
    let root = run(&bars, &[(1, 1)], sim(0.5), NO_FEES);
    assert!(
        (root.fills[0].price - 101.0).abs() < 1e-9,
        "{:?}",
        root.fills
    );
}

#[test]
fn what_gets_a_warning_can_be_set() {
    let bars = [FLAT, FLAT, FLAT];
    let with = |volume_warning_percent| SimSettings {
        impact_percent: 1.0,
        volume_warning_percent,
        ..SimSettings::default()
    };
    // An order of 100 is 1% of its bar.
    assert!(run(&bars, &[(1, 1)], with(10.0), NO_FEES).notes.is_empty());
    let warned = run(&bars, &[(1, 1)], with(0.5), NO_FEES);
    assert!(
        warned
            .notes
            .iter()
            .any(|note| note.text.contains("over 0.5%")),
        "{:?}",
        warned.notes
    );

    let flips: Vec<(usize, i8)> = (1..7)
        .map(|at| (at, if at % 2 == 0 { -1 } else { 1 }))
        .collect();
    let misses = |fill_warning_percent| {
        let sim = SimSettings {
            fill_warning_percent,
            ..limit(1.0, 1)
        };
        run(&[FLAT; 9], &flips, sim, NO_FEES)
    };
    let quiet = misses(0.0);
    assert!(
        !quiet
            .notes
            .iter()
            .any(|note| note.text.contains("limit orders filled"))
    );
    let told = misses(50.0);
    assert!(
        told.notes
            .iter()
            .any(|note| note.text.contains("limit orders filled"))
    );
}

fn trailing(percent: f64) -> SimSettings {
    SimSettings {
        trailing_stop_percent: Some(percent),
        ..SimSettings::default()
    }
}

#[test]
fn a_trailing_stop_follows_the_best_price_and_fills_inside_a_bar() {
    let bars = [
        FLAT,
        FLAT,
        // Up to 110, so the stop moves to 99 at this close.
        (100.0, 110.0, 100.0, 108.0),
        // Through 99 in this bar.
        (108.0, 108.0, 95.0, 96.0),
        (96.0, 96.0, 96.0, 96.0),
    ];
    let report = run(&bars, &[(1, 1)], trailing(10.0), NO_FEES);
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert_eq!(report.fills[1].side, Side::Sell);
    assert_eq!(report.fills[1].bar, 3);
    assert!(
        (report.fills[1].price - 99.0).abs() < 1e-9,
        "{:?}",
        report.fills[1]
    );
}

#[test]
fn a_trailing_stop_never_loosens_and_fills_at_the_open_on_a_gap() {
    let bars = [
        FLAT,
        FLAT,
        (100.0, 120.0, 100.0, 118.0),
        // A fall that doesn't reach 108 leaves it there, not lower.
        (118.0, 118.0, 110.0, 111.0),
        // Opens at 100, under the 108 it's at.
        (100.0, 101.0, 99.0, 100.0),
        FLAT,
    ];
    let report = run(&bars, &[(1, 1)], trailing(10.0), NO_FEES);
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert_eq!(report.fills[1].bar, 4);
    assert!(
        (report.fills[1].price - 100.0).abs() < 1e-9,
        "{:?}",
        report.fills[1]
    );
}

#[test]
fn a_trailing_stop_on_a_short_mirrors_it_and_the_nearer_of_two_stops_goes_first() {
    let bars = [
        FLAT,
        FLAT,
        (100.0, 100.0, 90.0, 92.0),
        (92.0, 105.0, 92.0, 104.0),
        FLAT,
    ];
    let report = run(&bars, &[(1, -1)], trailing(10.0), NO_FEES);
    assert_eq!(report.fills[1].side, Side::Buy);
    assert_eq!(report.fills[1].bar, 3);
    assert!(
        (report.fills[1].price - 99.0).abs() < 1e-9,
        "{:?}",
        report.fills[1]
    );

    // Long at 100: a fixed stop at 98 is nearer than a trail at 90.
    let both = SimSettings {
        stop_loss_percent: Some(2.0),
        ..trailing(10.0)
    };
    let bars = [FLAT, FLAT, (100.0, 101.0, 95.0, 96.0), FLAT];
    let report = run(&bars, &[(1, 1)], both, NO_FEES);
    assert!(
        (report.fills[1].price - 98.0).abs() < 1e-9,
        "{:?}",
        report.fills
    );
}

#[test]
fn a_close_already_past_where_a_trail_would_go_gets_out_at_that_close() {
    // The high of 120 puts it at 108, and that bar closed at 105.
    let bars = [FLAT, FLAT, (100.0, 120.0, 100.0, 105.0), FLAT, FLAT];
    let report = run(&bars, &[(1, 1)], trailing(10.0), NO_FEES);
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    assert_eq!(report.fills[1].bar, 2);
    assert!(
        (report.fills[1].price - 105.0).abs() < 1e-9,
        "{:?}",
        report.fills[1]
    );
}

#[test]
fn a_trailing_stop_respects_the_volume_limit() {
    let bars = [
        FLAT,
        FLAT,
        FLAT,
        (100.0, 100.0, 80.0, 82.0),
        close_at(81.0),
        FLAT,
    ];
    let sim = SimSettings {
        max_participation_percent: Some(0.5),
        ..trailing(10.0)
    };
    let report = run(&bars, &[(1, 1)], sim, NO_FEES);
    assert_eq!(
        fills_of(&report),
        vec![
            (1, Side::Buy, 100.0, 50.0),
            (2, Side::Buy, 100.0, 50.0),
            // Flat bars reach 100.5, so 10% under that.
            (3, Side::Sell, 90.45, 50.0),
            (3, Side::Sell, 82.0, 50.0),
        ],
    );
}

#[test]
fn a_decision_log_says_what_happened_on_every_bar() {
    let bars = [FLAT, FLAT, FLAT, FLAT];
    let sim = SimSettings {
        decision_log: true,
        ..SimSettings::default()
    };
    let report = run(&bars, &[(1, 1), (2, 1)], sim, NO_FEES);
    let rows: Vec<(Option<i8>, i8, f64, &str)> = report
        .decisions
        .iter()
        .map(|row| (row.wanted, row.held, row.order, row.why))
        .collect();
    assert_eq!(
        rows,
        vec![
            (None, 0, 0.0, "nothing asked"),
            (Some(1), 0, 100.0, "an order went out"),
            (Some(1), 1, 0.0, "already there"),
            (None, 1, 0.0, "nothing asked"),
        ]
    );
    assert!(
        run(&bars, &[(1, 1)], SimSettings::default(), NO_FEES)
            .decisions
            .is_empty()
    );
}

#[test]
fn the_decision_log_says_when_a_stop_filled() {
    let bars = [FLAT, FLAT, (100.0, 101.0, 90.0, 92.0), FLAT];
    let sim = SimSettings {
        decision_log: true,
        stop_loss_percent: Some(5.0),
        ..SimSettings::default()
    };
    let report = run(&bars, &[(1, 1)], sim, NO_FEES);
    assert_eq!(report.fills.len(), 2);
    // With the size it sold.
    let sold = report.fills[1].qty;
    assert!(
        report
            .decisions
            .iter()
            .any(|row| row.why == "a stop filled" && (row.order + sold).abs() < 1e-9),
        "{:?}",
        report.decisions
    );
}

#[test]
fn the_decision_log_says_when_a_trailing_stop_filled() {
    // Up to 110 after the entry, then down through 10% under that, but not 20% under the entry.
    let bars = [
        FLAT,
        FLAT,
        (100.0, 110.0, 100.0, 110.0),
        (110.0, 110.0, 97.0, 97.0),
        FLAT,
    ];
    let sim = SimSettings {
        decision_log: true,
        stop_loss_percent: Some(20.0),
        trailing_stop_percent: Some(10.0),
        ..SimSettings::default()
    };
    let report = run(&bars, &[(1, 1)], sim, NO_FEES);
    assert_eq!(report.fills.len(), 2, "{:?}", report.fills);
    let sold = report.fills[1].qty;
    assert!(
        report
            .decisions
            .iter()
            .any(|row| row.why == "a trailing stop filled" && (row.order + sold).abs() < 1e-9),
        "{:?}",
        report.decisions
    );
    assert!(
        !report
            .decisions
            .iter()
            .any(|row| row.why == "a stop filled"),
        "{:?}",
        report.decisions
    );
}

#[test]
fn closing_at_the_end_sells_what_is_open_on_the_last_bar_a_fill_can_happen() {
    let bars = [FLAT, FLAT, FLAT, close_at(110.0)];
    let fees = FeeRates {
        maker: 0.001,
        taker: 0.001,
    };
    let left = run(&bars, &[(1, 1)], SimSettings::default(), fees);
    assert_eq!(
        fills_of(&left).iter().map(|f| f.1).collect::<Vec<_>>(),
        [Side::Buy]
    );

    let closing = SimSettings {
        close_at_end: true,
        ..SimSettings::default()
    };
    let closed = run(&bars, &[(1, 1)], closing, fees);
    let fills = fills_of(&closed);
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_eq!((fills[1].0, fills[1].1, fills[1].2), (3, Side::Sell, 110.0));
    assert!((fills[1].3 - fills[0].3).abs() < 1e-9);
    // The exit's fee is paid, so it ends under the same trade left open.
    assert!(closed.equity.last().unwrap() < left.equity.last().unwrap());

    // With next-open fills it's decided a bar sooner and filled at the last bar's open.
    let next_open = SimSettings {
        next_open: true,
        ..closing
    };
    let opened = run(&bars, &[(1, 1)], next_open, NO_FEES);
    let fills = fills_of(&opened);
    assert_eq!(fills.len(), 2, "{fills:?}");
    assert_eq!((fills[1].0, fills[1].1, fills[1].2), (3, Side::Sell, 110.0));
}
