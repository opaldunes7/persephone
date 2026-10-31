//! A strategy given as a list: which position to hold from which bar. It's the simple way in for
//! predictions made elsewhere, such as by a model trained in Python.
//!
//! ```json
//! {"positions": [[1704067200000000000, 1], [1704153600000000000, -1], [1704240000000000000, 0]]}
//! ```
//!
//! A time is a bar's `ts`, as `get_bars` and exports give it: when the bar opens. Its order fills at
//! that bar's close, as any strategy's does, and holds until the next entry. Nothing stops a list looking ahead: that's up to whoever
//! made it.

use std::{fmt::Write as _, sync::Arc};

use persephone_core::{
    Candle, UnixNanos,
    signal::{Signal, SignalFactory},
    strategy::{StrategyInfo, StrategyKind, StrategySpec},
};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Wanted positions by time, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Positions {
    list: Arc<Vec<(UnixNanos, i8)>>,
}

impl Positions {
    /// Reads a list of `[ts, position]`.
    ///
    /// # Errors
    ///
    /// Returns a message saying which entry is wrong and why.
    pub fn parse(source: &Value) -> Result<Self, String> {
        let items = source
            .as_array()
            .filter(|items| !items.is_empty())
            .ok_or("positions: expected a list of [ts, position], with at least one")?;
        let mut list: Vec<(UnixNanos, i8)> = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let entry = item
                .as_array()
                .filter(|pair| pair.len() == 2)
                .and_then(|pair| Some((pair[0].as_u64()?, pair[1].as_i64()?)))
                .filter(|(_, position)| (-1..=1).contains(position))
                .ok_or_else(|| {
                    format!(
                        "positions[{i}]: expected [ts, position], with ts in nanoseconds and position 1, -1 or 0, not {item}"
                    )
                })?;
            if list.last().is_some_and(|last| last.0 >= entry.0) {
                return Err(format!(
                    "positions[{i}]: times should only go up, and {} doesn't",
                    entry.0
                ));
            }
            list.push((entry.0, entry.1 as i8));
        }
        Ok(Self {
            list: Arc::new(list),
        })
    }

    /// The same list by when bars close, which is when a strategy hears of them, from the times
    /// bars open and close.
    ///
    /// # Errors
    ///
    /// Returns a message naming a time that isn't a bar's.
    pub fn at_closes(&self, opens: &[UnixNanos], closes: &[UnixNanos]) -> Result<Self, String> {
        let list = self
            .list
            .iter()
            .map(|&(ts, position)| {
                let found = opens.binary_search(&ts).map_err(|_| {
                    format!(
                        "positions: no bar opens at {ts}; use the `ts` of a bar from get_bars, here from {} to {}",
                        opens.first().copied().unwrap_or(0),
                        opens.last().copied().unwrap_or(0),
                    )
                })?;
                Ok((closes[found], position))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Self {
            list: Arc::new(list),
        })
    }

    /// A short id for this exact list.
    #[must_use]
    pub fn id(&self) -> String {
        let mut digest = Sha256::new();
        for (ts, position) in self.list.iter() {
            digest.update(ts.to_le_bytes());
            digest.update(position.to_le_bytes());
        }
        let mut id = String::from("positions-");
        for byte in &digest.finalize()[..8] {
            let _ = write!(id, "{byte:02x}");
        }
        id
    }

    #[must_use]
    pub fn info(&self) -> StrategyInfo {
        StrategyInfo::new("Positions", "A list of positions by time", Vec::new())
    }

    /// The strategy to put in a backtest request, with this as its plugin.
    #[must_use]
    pub fn spec(&self) -> StrategySpec {
        StrategySpec::from_info(StrategyKind::Plugin(self.id()), Arc::new(self.info()), &[])
    }
}

impl SignalFactory for Positions {
    fn create(&self, _params: &[f64]) -> Result<Box<dyn Signal>, String> {
        Ok(Box::new(Running {
            list: Arc::clone(&self.list),
            next: 0,
            wanted: None,
        }))
    }
}

struct Running {
    list: Arc<Vec<(UnixNanos, i8)>>,
    next: usize,
    wanted: Option<i8>,
}

impl Signal for Running {
    fn on_bar(&mut self, bar: &Candle) -> Result<Option<i8>, String> {
        while let Some(&(ts, position)) = self.list.get(self.next)
            && ts <= bar.ts
        {
            self.wanted = Some(position);
            self.next += 1;
        }
        // Asked for on every bar, as a list says what to hold from then on: a span or random block
        // starting partway through takes up what's wanted by its first bar that can trade.
        Ok(self.wanted)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn bar(ts: UnixNanos) -> Candle {
        Candle {
            ts,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
            volume: 1.0,
        }
    }

    #[test]
    fn it_asks_for_each_position_on_its_bar_and_keeps_it_between() {
        let list = Positions::parse(&json!([[10, 1], [30, -1], [40, 0]])).unwrap();
        let mut running = list.create(&[]).unwrap();
        let wanted: Vec<Option<i8>> = [5, 10, 20, 30, 40, 50]
            .iter()
            .map(|&ts| running.on_bar(&bar(ts)).unwrap())
            .collect();
        assert_eq!(
            wanted,
            vec![None, Some(1), Some(1), Some(-1), Some(0), Some(0)]
        );
    }

    #[test]
    fn a_run_starting_later_picks_up_what_was_wanted_by_then() {
        let list = Positions::parse(&json!([[10, 1], [20, -1]])).unwrap();
        let mut running = list.create(&[]).unwrap();
        assert_eq!(running.on_bar(&bar(25)).unwrap(), Some(-1));
    }

    #[test]
    fn times_move_from_when_bars_open_to_when_they_close() {
        let list = Positions::parse(&json!([[100, 1], [200, 0]])).unwrap();
        let moved = list.at_closes(&[100, 200], &[199, 299]).unwrap();
        assert_eq!(*moved.list, vec![(199, 1), (299, 0)]);
        let error = list.at_closes(&[100, 300], &[299, 499]).unwrap_err();
        assert!(error.contains("no bar opens at 200"), "{error}");
    }

    #[test]
    fn bad_lists_say_what_is_wrong() {
        let error = |list: Value| Positions::parse(&list).unwrap_err();
        assert!(error(json!([])).contains("a list"));
        assert!(error(json!([[1, 2]])).contains("positions[0]"));
        assert!(error(json!([[1, 1], "x"])).contains("positions[1]"));
        assert!(error(json!([[5, 1], [5, 0]])).contains("only go up"));
    }
}
