//! Markets from the local store (ticks imported with `persephone-cli import`): charting their
//! bars, and backtesting on their ticks when there aren't too many to replay.

use std::sync::Arc;

use persephone_chart::ChartData;
use persephone_core::{
    Timeframe,
    tick::{TickFeed, TickKind},
};
use persephone_data::store::Store;
use persephone_engine::FeeRates;

use crate::{HISTORY_BARS, MarketData};

/// The most ticks a chart's backtest replays (about 8 s in a release build). Past this, its
/// orders fill on the bars.
pub const TICK_FILL_LIMIT: u64 = 10_000_000;

/// Reads a stored market's latest bars, with its ticks for backtests.
///
/// # Errors
///
/// Returns an error if the market doesn't exist, can't be read or has no data yet.
pub fn load(
    store: &Store,
    venue: &str,
    symbol: &str,
    timeframe: Timeframe,
) -> anyhow::Result<MarketData> {
    let market = store.market(venue, symbol)?;
    let candles = market.candles(timeframe, HISTORY_BARS as usize)?;
    anyhow::ensure!(!candles.is_empty(), "{} has no data yet", market.id());
    let info = &market.info;

    // Ticks aren't spread evenly, but the chart's share of the market's span is a fair guess.
    let from = candles[0].ts.max(info.first);
    let to = (candles[candles.len() - 1].ts + timeframe.nanos()).min(info.last + 1);
    let span = info.last.saturating_sub(info.first).max(1);
    let estimate = (info.count as f64 * to.saturating_sub(from) as f64 / span as f64) as u64;
    let at = match info.kind {
        TickKind::Quotes => "the bid and ask",
        TickKind::Trades => "the latest trade",
    };
    let (ticks, fills) = if estimate <= TICK_FILL_LIMIT {
        let feed: Arc<dyn TickFeed> = Arc::new(market.clone());
        (Some(feed), format!("Orders fill on ticks, at {at}"))
    } else {
        (
            None,
            format!(
                "Orders fill on bars: this chart spans about {}M ticks, too many to replay. A shorter timeframe fills on ticks.",
                estimate / 1_000_000
            ),
        )
    };

    // Said in the chart's header, so a stored BINANCE market isn't taken for the live one.
    let data = ChartData::new(
        info.symbol.clone(),
        format!("{} (local)", info.venue),
        timeframe,
        info.price_precision,
        candles,
    );
    Ok(MarketData {
        data,
        instrument: market.instrument(),
        fees: FeeRates {
            maker: info.fee,
            taker: info.fee,
        },
        ticks,
        fills: Some(fills),
    })
}
