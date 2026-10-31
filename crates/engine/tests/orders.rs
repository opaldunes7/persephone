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
    assert!((persephone_engine::impact_share(1.0, 100.0, 10_000.0) - 0.001).abs() < 1e-12);
    assert_eq!(persephone_engine::impact_share(1.0, 100.0, 0.0), 0.0);
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
