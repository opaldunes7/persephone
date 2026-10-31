//! A backtest's trades by the day they closed, for the panel's calendar: months by year, then
//! days by week, then the day's trades, each of which can be replayed on the chart.
//!
//! Days are UTC, like the chart's own time axis and daily bars. A trade counts on the day it
//! closed, however long it was open; one still open at the end counts on the last bar's day, at
//! that bar's close, so the calendar adds up to the backtest's whole profit.

use std::collections::BTreeMap;

use persephone_core::{
    UnixNanos,
    time::{DateTime, days_from_civil},
};

use crate::robustness::RoundTrip;

/// Which part of the calendar is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Level {
    /// Months, a row for each year.
    #[default]
    Years,
    /// A year and month's days, a row for each week.
    Month(i64, u32),
    /// A year, month and day's trades, beside its month.
    Day(i64, u32, u32),
}

/// The calendar's own state on the panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CalendarView {
    /// Showing the calendar instead of the equity curve.
    pub shown: bool,
    pub level: Level,
    /// The trade last sent to the chart, by its place in the backtest's trades.
    pub replaying: Option<usize>,
}

/// What a stretch of time came to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Total {
    pub profit: f64,
    pub trades: usize,
}

impl Total {
    fn add(&mut self, other: Self) {
        self.profit += other.profit;
        self.trades += other.trades;
    }
}

/// The trades, grouped by day.
#[derive(Clone, Debug)]
pub struct Calendar {
    pub trades: Vec<RoundTrip>,
    days: BTreeMap<(i64, u32, u32), Vec<usize>>,
}

impl Calendar {
    /// Groups `trades`, counting one still open as closing at `last`, the last bar's time.
    #[must_use]
    pub fn new(trades: Vec<RoundTrip>, last: UnixNanos) -> Self {
        let mut days: BTreeMap<_, Vec<usize>> = BTreeMap::new();
        for (index, trade) in trades.iter().enumerate() {
            let at = DateTime::from_nanos(if trade.open { last } else { trade.exit_ts });
            days.entry((at.year, at.month, at.day))
                .or_default()
                .push(index);
        }
        Self { trades, days }
    }

    fn total_of(&self, trades: &[usize]) -> Total {
        Total {
            profit: trades.iter().map(|&i| self.trades[i].profit).sum(),
            trades: trades.len(),
        }
    }

    /// Every year from the first trade's to the last's, in order.
    #[must_use]
    pub fn years(&self) -> Vec<i64> {
        match (self.days.keys().next(), self.days.keys().next_back()) {
            (Some(first), Some(last)) => (first.0..=last.0).collect(),
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub fn day(&self, year: i64, month: u32, day: u32) -> Total {
        self.days
            .get(&(year, month, day))
            .map_or_else(Total::default, |trades| self.total_of(trades))
    }

    /// The nearest earlier day with trades.
    #[must_use]
    pub fn day_before(&self, year: i64, month: u32, day: u32) -> Option<Level> {
        let (&(y, m, d), _) = self.days.range(..(year, month, day)).next_back()?;
        Some(Level::Day(y, m, d))
    }

    /// The nearest later day with trades.
    #[must_use]
    pub fn day_after(&self, year: i64, month: u32, day: u32) -> Option<Level> {
        let (&(y, m, d), _) = self.days.range((year, month, day + 1)..).next()?;
        Some(Level::Day(y, m, d))
    }

    /// Which trades closed on a day, by their place in the backtest's trades.
    #[must_use]
    pub fn trades_on(&self, year: i64, month: u32, day: u32) -> &[usize] {
        self.days
            .get(&(year, month, day))
            .map_or(&[], Vec::as_slice)
    }

    #[must_use]
    pub fn month(&self, year: i64, month: u32) -> Total {
        let mut total = Total::default();
        for trades in self
            .days
            .range((year, month, 0)..=(year, month, 31))
            .map(|(_, trades)| trades)
        {
            total.add(self.total_of(trades));
        }
        total
    }

    #[must_use]
    pub fn year(&self, year: i64) -> Total {
        let mut total = Total::default();
        for month in 1..=12 {
            total.add(self.month(year, month));
        }
        total
    }
}

/// How many days a month has.
#[must_use]
pub fn days_in_month(year: i64, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    (days_from_civil(next_year, next_month, 1) - days_from_civil(year, month, 1)) as u32
}

/// A month's days laid out in weeks, Monday first, with `None` outside the month.
#[must_use]
pub fn weeks(year: i64, month: u32) -> Vec<[Option<u32>; 7]> {
    // 1970-01-01 was a Thursday.
    let first = (days_from_civil(year, month, 1) + 3).rem_euclid(7) as usize;
    let mut weeks = vec![[None; 7]];
    for day in 1..=days_in_month(year, month) {
        let place = first + day as usize - 1;
        if place / 7 == weeks.len() {
            weeks.push([None; 7]);
        }
        weeks[place / 7][place % 7] = Some(day);
    }
    weeks
}

#[cfg(test)]
mod tests {
    use persephone_core::time::NANOS_PER_DAY;

    use super::*;

    fn at(year: i64, month: u32, day: u32) -> UnixNanos {
        days_from_civil(year, month, day) as UnixNanos * NANOS_PER_DAY
    }

    fn trade(entry: UnixNanos, exit: UnixNanos, profit: f64, open: bool) -> RoundTrip {
        RoundTrip {
            long: true,
            qty: 1.0,
            entry_ts: entry,
            entry_bar: 0,
            entry_price: 100.0,
            exit_ts: exit,
            exit_bar: 1,
            exit_price: 101.0,
            profit,
            fees: 0.0,
            open,
        }
    }

    #[test]
    fn a_month_lays_out_monday_first() {
        // November 2026 starts on a Sunday and takes six rows.
        let november = weeks(2026, 11);
        assert_eq!(november.len(), 6);
        assert_eq!(november[0], [None, None, None, None, None, None, Some(1)]);
        assert_eq!(november[1][0], Some(2));
        assert_eq!(november[5][0], Some(30));
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2026, 12), 31);
        // February 2027 starts on a Monday and fills exactly four.
        assert_eq!(weeks(2027, 2).len(), 4);
    }

    #[test]
    fn a_trade_counts_on_the_day_it_closed() {
        let calendar = Calendar::new(
            vec![
                // Opened in October, closed in November.
                trade(at(2026, 10, 30), at(2026, 11, 2), 50.0, false),
                trade(at(2026, 11, 2), at(2026, 11, 2), -20.0, false),
                trade(at(2026, 11, 3), at(2026, 11, 3), 5.0, false),
            ],
            at(2026, 12, 1),
        );
        assert_eq!(calendar.month(2026, 10), Total::default());
        let day = calendar.day(2026, 11, 2);
        assert_eq!((day.profit, day.trades), (30.0, 2));
        assert_eq!(calendar.trades_on(2026, 11, 2), &[0, 1]);
        assert_eq!(calendar.month(2026, 11).profit, 35.0);
        assert_eq!(calendar.month(2026, 11).trades, 3);
    }

    #[test]
    fn a_trade_still_open_counts_on_the_last_bars_day_so_it_all_adds_up() {
        let trades = vec![
            trade(at(2025, 12, 30), at(2026, 1, 2), 10.0, false),
            trade(at(2026, 1, 3), at(2026, 1, 3), 7.0, true),
        ];
        let total: f64 = trades.iter().map(|t| t.profit).sum();
        let calendar = Calendar::new(trades, at(2027, 3, 9));
        assert_eq!(calendar.day(2027, 3, 9).profit, 7.0);
        assert_eq!(calendar.years(), vec![2026, 2027]);
        let years: f64 = calendar
            .years()
            .iter()
            .map(|y| calendar.year(*y).profit)
            .sum();
        assert_eq!(years, total);
    }

    #[test]
    fn stepping_a_day_skips_days_without_trades_across_months() {
        let calendar = Calendar::new(
            vec![
                trade(at(2026, 10, 30), at(2026, 10, 30), 1.0, false),
                trade(at(2026, 11, 2), at(2026, 11, 2), 1.0, false),
                trade(at(2026, 11, 9), at(2026, 11, 9), 1.0, false),
            ],
            at(2026, 12, 1),
        );
        assert_eq!(
            calendar.day_before(2026, 11, 2),
            Some(Level::Day(2026, 10, 30))
        );
        assert_eq!(
            calendar.day_after(2026, 10, 30),
            Some(Level::Day(2026, 11, 2))
        );
        assert_eq!(
            calendar.day_after(2026, 11, 2),
            Some(Level::Day(2026, 11, 9))
        );
        assert_eq!(calendar.day_before(2026, 10, 30), None);
        assert_eq!(calendar.day_after(2026, 11, 9), None);
    }

    #[test]
    fn no_trades_is_an_empty_calendar() {
        let calendar = Calendar::new(Vec::new(), 0);
        assert!(calendar.years().is_empty());
        assert_eq!(calendar.trades_on(2026, 1, 1), &[] as &[usize]);
    }
}
