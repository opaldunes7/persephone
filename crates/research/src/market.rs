//! Loading a market's bars, from the local store or Binance, with what the engine needs to trade
//! them.

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use persephone_core::{
    BarSpec, Candle, InstrumentSpec, Timeframe, UnixNanos,
    tick::{TickFeed, TickKind},
};
use persephone_data::{
    binance::{BinanceClient, check_range},
    store::{MarketKind, Store},
};
use persephone_engine::FeeRates;

use crate::WARM_UP_BARS;

/// How many bars a chart with no chosen span loads.
pub const HISTORY_BARS: u64 = 5_000;
/// The most ticks a chart's backtest replays (about 8 s in a release build). Past this, its
/// orders fill on the bars.
pub const TICK_FILL_LIMIT: u64 = 10_000_000;

/// The most 1-minute bars High precision loads for a span.
pub const MAX_FINE_BARS: u64 = 1_000_000;

/// One market's bars plus what the engine needs to trade it.
#[derive(Clone, Debug)]
pub struct MarketBars {
    pub symbol: String,
    /// Where they come from, as a chart's header says it.
    pub venue: String,
    pub bars: BarSpec,
    pub price_precision: u8,
    pub candles: Vec<Candle>,
    pub instrument: InstrumentSpec,
    pub fees: FeeRates,
    /// Ticks to fill backtest orders on; `None` fills them on the bars.
    pub ticks: Option<Arc<dyn TickFeed>>,
    /// How backtest orders fill, for the report; `None` for a plain bar market.
    pub fills: Option<String>,
}

/// The 1-minute bars of `from..to`, for High precision to fill orders on when there are no ticks.
/// `local` is the store and venue of a stored market; without one they come from Binance.
///
/// # Errors
///
/// Returns an error if there'd be too many, or they can't be read.
pub fn fine_bars(
    local: Option<(&Store, &str)>,
    symbol: &str,
    from: UnixNanos,
    to: UnixNanos,
) -> anyhow::Result<Vec<Candle>> {
    let minutes = to.saturating_sub(from) / Timeframe::M1.nanos();
    anyhow::ensure!(
        minutes <= MAX_FINE_BARS,
        "High precision would need {minutes} 1-minute bars here, more than {MAX_FINE_BARS}; pick a shorter span"
    );
    match local {
        Some((store, venue)) => {
            store
                .market(venue, symbol)?
                .candles_between(Timeframe::M1.into(), from, to)
        }
        None => BinanceClient::new()?.klines(symbol, Timeframe::M1, from, to),
    }
}

/// Reads a stored market's latest bars, or those of `span` and some before it to warm
/// strategies up on, with its ticks for backtests.
///
/// # Errors
///
/// Returns an error if the market doesn't exist, can't be read or has no data yet.
pub fn load_local(
    store: &Store,
    venue: &str,
    symbol: &str,
    bars: BarSpec,
    span: Option<(UnixNanos, UnixNanos)>,
) -> anyhow::Result<MarketBars> {
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

    Ok(MarketBars {
        symbol: info.symbol.clone(),
        // Said in the chart's header, so a stored BINANCE market isn't taken for the live one.
        venue: format!("{} (local)", info.venue),
        bars,
        price_precision: info.price_precision,
        candles,
        instrument: market.instrument(),
        fees: FeeRates {
            maker: info.fee,
            taker: info.fee,
        },
        ticks,
        fills: Some(fills),
    })
}

/// Fetches Binance's latest bars, or those of `span` and some before it to warm strategies up on.
///
/// # Errors
///
/// Returns an error if the range is too long or Binance can't be reached or has no such market.
pub fn load_binance(
    symbol: &str,
    timeframe: Timeframe,
    span: Option<(UnixNanos, UnixNanos)>,
) -> anyhow::Result<MarketBars> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
    let latest = timeframe.close_time(now); // include the forming bar
    let (start, end) = match span {
        Some((from, to)) => {
            let start = from.saturating_sub(WARM_UP_BARS * timeframe.nanos());
            let end = to.min(latest);
            check_range(timeframe, start, end)?;
            (start, end)
        }
        None => (
            latest.saturating_sub(HISTORY_BARS * timeframe.nanos()),
            latest,
        ),
    };

    let binance = BinanceClient::new()?;
    let instrument = binance.instrument(symbol)?;
    let candles = binance.klines(symbol, timeframe, start, end)?;
    anyhow::ensure!(!candles.is_empty(), "Binance returned no bars for {symbol}");
    Ok(MarketBars {
        symbol: instrument.symbol.clone(),
        venue: instrument.venue.clone(),
        bars: timeframe.into(),
        price_precision: instrument.price_precision,
        candles,
        instrument,
        fees: FeeRates::BINANCE_SPOT,
        ticks: None,
        fills: None,
    })
}
