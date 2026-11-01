//! Every trade a run made, flat to flat, as one list: of one market, or of every market in a
//! portfolio together, in the order they opened. The app's inspection drawer and Lab's
//! `get_portfolio_trades` read the same.

use persephone_core::Side;
use persephone_engine::{BacktestReport, LegReport};

use crate::{
    portfolio::PortfolioRun,
    robustness::{RoundTrip, round_trips},
};

/// One trade and the market it was in.
#[derive(Clone, Debug, PartialEq)]
pub struct LedgerRow {
    pub symbol: String,
    pub trip: RoundTrip,
    /// What it made on what it put in: its profit over its size at its entry price.
    pub return_on_position: f64,
    /// What it made as a share of the run's starting capital, as `get_trades` gives it.
    pub return_on_capital: f64,
}

/// A run's trades, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ledger {
    pub rows: Vec<LedgerRow>,
}

impl Ledger {
    /// One market's run.
    #[must_use]
    pub fn from_report(report: &BacktestReport, symbol: &str) -> Self {
        Self::from_trips(round_trips(report), symbol, report.starting_balance)
    }

    /// Trades of one market already in hand, as Monte Carlo's draws and Random blocks keep them.
    #[must_use]
    pub fn from_trips(trips: Vec<RoundTrip>, symbol: &str, capital: f64) -> Self {
        Self {
            rows: trips
                .into_iter()
                .map(|trip| row(symbol, trip, capital))
                .collect(),
        }
    }

    /// Every leg of a portfolio, in the order its trades opened. A trade's bars are moments on the
    /// portfolio's timeline, not its market's own bars, so it's best found by its times.
    #[must_use]
    pub fn from_portfolio(run: &PortfolioRun) -> Self {
        let capital = run.report.starting_balance;
        let mut rows: Vec<LedgerRow> = run
            .report
            .legs
            .iter()
            .flat_map(|leg| {
                leg_round_trips(leg)
                    .into_iter()
                    .map(move |trip| row(&leg.symbol, trip, capital))
            })
            .collect();
        rows.sort_by(|a, b| {
            a.trip
                .entry_ts
                .cmp(&b.trip.entry_ts)
                .then_with(|| a.symbol.cmp(&b.symbol))
        });
        Self { rows }
    }

    /// The trades' profits added up.
    #[must_use]
    pub fn profit(&self) -> f64 {
        self.rows.iter().map(|row| row.trip.profit).sum()
    }
}

fn row(symbol: &str, trip: RoundTrip, capital: f64) -> LedgerRow {
    let put_in = trip.qty * trip.entry_price;
    LedgerRow {
        symbol: symbol.to_owned(),
        return_on_position: if put_in > 0.0 {
            trip.profit / put_in
        } else {
            0.0
        },
        return_on_capital: if capital > 0.0 {
            trip.profit / capital
        } else {
            0.0
        },
        trip,
    }
}

/// A portfolio leg's trades, from its fills alone. One still open at the end is valued at what the
/// position was last worth, less its fees, like a closed one: funding and borrowing are the leg's,
/// in its figures, rather than any one trade's.
#[must_use]
pub fn leg_round_trips(leg: &LegReport) -> Vec<RoundTrip> {
    let report = BacktestReport {
        fills: leg.fills.clone(),
        equity: vec![marked(leg)],
        starting_balance: 0.0,
        ..BacktestReport::default()
    };
    let mut trips = round_trips(&report);
    // An open trade's last bar is the timeline's, not the stand-in's one point.
    if let Some(trip) = trips.last_mut().filter(|trip| trip.open) {
        trip.exit_bar = leg.values.len().max(leg.closes.len()).saturating_sub(1);
    }
    trips
}

/// What a leg's fills alone left: the cash they moved, less their fees, and what's still held, at
/// what it was last worth. Saved runs keep the values but not the closes, so the values come first.
fn marked(leg: &LegReport) -> f64 {
    let (mut cash, mut held) = (0.0, 0.0);
    for fill in &leg.fills {
        let sign = if fill.side == Side::Buy { 1.0 } else { -1.0 };
        cash -= sign * fill.qty * fill.price + fill.fee;
        held += sign * fill.qty;
    }
    let worth = leg
        .values
        .last()
        .copied()
        .unwrap_or_else(|| held * leg.closes.last().copied().unwrap_or(0.0));
    cash + worth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::PortfolioFigures;
    use persephone_engine::{Fill, PortfolioReport};

    fn fill(ts: u64, bar: usize, side: Side, price: f64, qty: f64, fee: f64) -> Fill {
        Fill {
            ts,
            bar,
            side,
            price,
            qty,
            fee,
            maker: false,
            range_slip: 0.0,
        }
    }

    #[test]
    fn a_portfolios_trades_come_from_every_leg_in_the_order_they_opened() {
        // A: long 2 at 100, out at 110, fees 1 each way: 18. B: short 1 at 50 from later, still open
        // at 45, fee 0.5: 4.5.
        let a = LegReport {
            symbol: "AAA".into(),
            fills: vec![
                fill(10, 1, Side::Buy, 100.0, 2.0, 1.0),
                fill(30, 3, Side::Sell, 110.0, 2.0, 1.0),
            ],
            values: vec![0.0, 200.0, 210.0, 0.0, 0.0],
            ..LegReport::default()
        };
        let b = LegReport {
            symbol: "BBB".into(),
            fills: vec![fill(20, 2, Side::Sell, 50.0, 1.0, 0.5)],
            values: vec![0.0, 0.0, -50.0, -48.0, -45.0],
            // Funding it paid, which is the leg's and not the trade's.
            funding_paid: 3.0,
            contribution: vec![0.0, 0.0, -0.5, 1.5, 1.5],
            ..LegReport::default()
        };
        let run = PortfolioRun {
            report: PortfolioReport {
                starting_balance: 1_000.0,
                legs: vec![a, b],
                ..PortfolioReport::default()
            },
            figures: PortfolioFigures::default(),
            ..PortfolioRun::default()
        };
        let ledger = Ledger::from_portfolio(&run);
        let symbols: Vec<&str> = ledger.rows.iter().map(|row| row.symbol.as_str()).collect();
        assert_eq!(symbols, ["AAA", "BBB"]);
        let (long, short) = (&ledger.rows[0], &ledger.rows[1]);
        assert!((long.trip.profit - 18.0).abs() < 1e-9, "{long:?}");
        assert!((long.return_on_position - 18.0 / 200.0).abs() < 1e-12);
        assert!((long.return_on_capital - 0.018).abs() < 1e-12);
        assert!(!long.trip.open && long.trip.long);
        assert!((short.trip.profit - 4.5).abs() < 1e-9, "{short:?}");
        assert!(short.trip.open && !short.trip.long);
        assert_eq!(short.trip.exit_bar, 4);
        assert!((ledger.profit() - 22.5).abs() < 1e-9);
    }

    #[test]
    fn one_markets_ledger_adds_up_to_its_profit() {
        let report = BacktestReport {
            starting_balance: 1_000.0,
            fills: vec![
                fill(10, 1, Side::Buy, 100.0, 1.0, 0.1),
                fill(20, 2, Side::Sell, 104.0, 1.0, 0.1),
                fill(30, 3, Side::Buy, 103.0, 1.0, 0.1),
            ],
            // Still holding at 106 at the end.
            equity: vec![1_000.0, 999.9, 1_003.8, 1_003.7, 1_006.7],
            ..BacktestReport::default()
        };
        let ledger = Ledger::from_report(&report, "AAA");
        assert_eq!(ledger.rows.len(), 2);
        assert!((ledger.profit() - 6.7).abs() < 1e-9, "{ledger:?}");
        assert!(ledger.rows.iter().all(|row| row.symbol == "AAA"));
    }
}
