//! Markets from the local store (ticks or bars imported in the app or with
//! `persephone-cli import`): charting their bars, and backtesting on their ticks when they
//! have them and there aren't too many to replay.

use std::sync::Arc;

use persephone_chart::ChartData;
use persephone_core::{
    BarSpec, UnixNanos,
    tick::{TickFeed, TickKind},
};
use persephone_data::store::{MarketKind, Store};
use persephone_engine::FeeRates;

use crate::{HISTORY_BARS, MarketData, backtest::WARM_UP_BARS};

/// The most ticks a chart's backtest replays (about 8 s in a release build). Past this, its
/// orders fill on the bars.
pub const TICK_FILL_LIMIT: u64 = 10_000_000;

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
    let market = store.market(venue, symbol)?;
    let candles = match span {
        Some((from, to)) => {
            let warm_up = bars.timeframe().map_or(0, |tf| WARM_UP_BARS * tf.nanos());
            market.candles_between(bars, from.saturating_sub(warm_up), to)?
        }
        None => market.candles_by(bars, HISTORY_BARS as usize)?,
    };
    anyhow::ensure!(!candles.is_empty(), "{} has no data yet", market.id());
    let info = &market.info;

    // Ticks aren't spread evenly, but the chart's share of the market's span is a fair guess.
    let from = candles[0].ts.max(info.first);
    let to = bars
        .closes(&candles)
        .last()
        .map_or(info.last + 1, |&close| close.min(info.last + 1));
    let span = info.last.saturating_sub(info.first).max(1);
    let estimate = (info.count as f64 * to.saturating_sub(from) as f64 / span as f64) as u64;
    let at = match info.kind {
        MarketKind::Ticks(TickKind::Quotes) => "the bid and ask",
        MarketKind::Ticks(TickKind::Trades) => "the latest trade",
        MarketKind::Bars(_) => "",
    };
    let (ticks, fills) = if info.kind.ticks().is_none() {
        (
            None,
            "Orders fill on bars: this market was imported as bars.".to_owned(),
        )
    } else if estimate <= TICK_FILL_LIMIT {
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
        bars,
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
