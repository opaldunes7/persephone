//! Markets from the local store (ticks or bars imported in the app or with
//! `persephone-cli import`): charting their bars, and backtesting on their ticks when they
//! have them and there aren't too many to replay.

use persephone_core::{BarSpec, UnixNanos};
use persephone_data::store::Store;
use persephone_research::market::load_local;

use crate::MarketData;

/// Reads a stored market's latest bars, or those of `span` and some before it to warm
/// strategies up on, with its ticks for backtests.
///
/// # Errors
///
/// Returns an error if the market doesn't exist, can't be read or has no data yet.
pub fn load(
    store: &Store,
    venue: &str,
    symbol: &str,
    bars: BarSpec,
    span: Option<(UnixNanos, UnixNanos)>,
) -> anyhow::Result<MarketData> {
    load_local(store, venue, symbol, bars, span).map(MarketData::from)
}
