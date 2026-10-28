//! Persephone types → Nautilus types.

use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, BarSpecification, BarType, Data, QuoteTick, TradeTick},
    enums::{AggregationSource, AggressorSide, BarAggregation, CurrencyType, PriceType},
    identifiers::{InstrumentId, Symbol, TradeId},
    instruments::{CurrencyPair, InstrumentAny},
    types::{Currency, Price, Quantity},
};
use persephone_core::{
    BarSpec, Candle, InstrumentSpec, Timeframe,
    tick::{Aggressor, Quote, Trade},
};

use crate::EngineError;

pub(crate) fn instrument(spec: &InstrumentSpec) -> Result<InstrumentAny, EngineError> {
    let id = InstrumentId::from(format!("{}.{}", spec.symbol, spec.venue).as_str());
    let price_increment = Price::new(
        10f64.powi(-i32::from(spec.price_precision)),
        spec.price_precision,
    );
    let size_increment = Quantity::new(
        10f64.powi(-i32::from(spec.size_precision)),
        spec.size_precision,
    );

    let pair = CurrencyPair::builder()
        .instrument_id(id)
        .raw_symbol(Symbol::from(spec.symbol.as_str()))
        .base_currency(currency(&spec.base)?)
        .quote_currency(currency(&spec.quote)?)
        .price_precision(spec.price_precision)
        .size_precision(spec.size_precision)
        .price_increment(price_increment)
        .size_increment(size_increment)
        .min_quantity(size_increment)
        .ts_event(UnixNanos::default())
        .ts_init(UnixNanos::default())
        .build()
        .map_err(|e| EngineError::InvalidRequest(format!("instrument {id}: {e}")))?;
    Ok(InstrumentAny::CurrencyPair(pair))
}

/// A currency by code. Codes Nautilus doesn't know (an index's made-up base, an old coin from
/// imported data) are registered on first use instead of failing the run.
pub(crate) fn currency(code: &str) -> Result<Currency, EngineError> {
    if let Some(known) = Currency::try_from_str(code) {
        return Ok(known);
    }
    let currency = Currency::new_checked(code, 8, 0, code, CurrencyType::Crypto)
        .map_err(|e| EngineError::InvalidRequest(format!("currency `{code}`: {e}")))?;
    let _ = Currency::register(currency, false);
    Ok(currency)
}

pub(crate) fn bar_type(id: InstrumentId, bars: BarSpec) -> Result<BarType, EngineError> {
    // The bars come from outside Nautilus, so this only names them; volume steps are whole
    // numbers there.
    let (step, aggregation) = match bars {
        BarSpec::Time(Timeframe::M1) => (1, BarAggregation::Minute),
        BarSpec::Time(Timeframe::M5) => (5, BarAggregation::Minute),
        BarSpec::Time(Timeframe::M15) => (15, BarAggregation::Minute),
        BarSpec::Time(Timeframe::M30) => (30, BarAggregation::Minute),
        BarSpec::Time(Timeframe::H1) => (1, BarAggregation::Hour),
        BarSpec::Time(Timeframe::H4) => (4, BarAggregation::Hour),
        BarSpec::Time(Timeframe::D1) => (1, BarAggregation::Day),
        BarSpec::Time(Timeframe::W1) => (1, BarAggregation::Week),
        BarSpec::Time(Timeframe::MN1) => (1, BarAggregation::Month),
        BarSpec::Ticks(n) => (n.max(1) as usize, BarAggregation::Tick),
        BarSpec::Volume(v) => (v.round().max(1.0) as usize, BarAggregation::Volume),
    };
    let spec = BarSpecification::new_checked(step, aggregation, PriceType::Last)?;
    Ok(BarType::new(id, spec, AggregationSource::External))
}

/// A name for tick or volume bars that orders fill on. Nautilus ranks the bars it fills on by
/// duration, which these don't have, so they go by the shortest one; it's only a name.
pub(crate) fn untimed_bar_type(id: InstrumentId) -> Result<BarType, EngineError> {
    let spec = BarSpecification::new_checked(1, BarAggregation::Millisecond, PriceType::Last)?;
    Ok(BarType::new(id, spec, AggregationSource::External))
}

/// Converts candles to Nautilus bars, stamped with `closes`.
///
/// Nautilus stamps a bar with its **close** time, while [`Candle::ts`] is the open time;
/// stamping the close keeps a strategy from seeing a bar before it has closed. Bars get a deep
/// book like ticks do: Nautilus caps bar fills at the bar's volume, which for bars made from
/// quotes is only a count of quotes.
pub(crate) fn bars(
    candles: &[Candle],
    closes: &[u64],
    bar_type: BarType,
    spec: &InstrumentSpec,
) -> Result<Vec<Data>, EngineError> {
    let px = |v: f64| Price::new(v, spec.price_precision);
    candles
        .iter()
        .zip(closes)
        .map(|(c, &close)| {
            let closed_at = UnixNanos::from(close);
            let bar = Bar::new_checked(
                bar_type,
                px(c.open),
                px(c.high),
                px(c.low),
                px(c.close),
                depth(spec),
                closed_at,
                closed_at,
            )?;
            Ok(Data::Bar(bar))
        })
        .collect()
}

/// The size every tick gets in the simulated book: deep enough that an order always fills in
/// full at the tick's price. A trade's size says nothing about the book's depth, and quote
/// sizes come in whatever units the source used, so neither can cap fills (on an L1 book
/// Nautilus slips whatever a level can't fill by one tick).
const BOOK_DEPTH: f64 = 1e9;

fn depth(spec: &InstrumentSpec) -> Quantity {
    Quantity::new(BOOK_DEPTH, spec.size_precision)
}

/// Converts trades to Nautilus trade ticks, numbering their ids from `next_id`.
pub(crate) fn trade_ticks(
    trades: &[Trade],
    id: InstrumentId,
    spec: &InstrumentSpec,
    next_id: &mut u64,
) -> Result<Vec<TradeTick>, EngineError> {
    trades
        .iter()
        .map(|t| {
            *next_id += 1;
            let aggressor = match t.aggressor {
                Aggressor::Buyer => AggressorSide::Buy,
                Aggressor::Seller => AggressorSide::Sell,
                Aggressor::Unknown => AggressorSide::NoAggressor,
            };
            let ts = UnixNanos::from(t.ts);
            Ok(TradeTick::new_checked(
                id,
                Price::new(t.price, spec.price_precision),
                depth(spec),
                aggressor,
                TradeId::new(next_id.to_string()),
                ts,
                ts,
            )?)
        })
        .collect()
}

pub(crate) fn quote_ticks(
    quotes: &[Quote],
    id: InstrumentId,
    spec: &InstrumentSpec,
) -> Result<Vec<QuoteTick>, EngineError> {
    let px = |v: f64| Price::new(v, spec.price_precision);
    quotes
        .iter()
        .map(|q| {
            let ts = UnixNanos::from(q.ts);
            Ok(QuoteTick::new_checked(
                id,
                px(q.bid),
                px(q.ask),
                depth(spec),
                depth(spec),
                ts,
                ts,
            )?)
        })
        .collect()
}

pub(crate) fn quantity(value: f64, spec: &InstrumentSpec) -> Result<Quantity, EngineError> {
    let qty = Quantity::new(value, spec.size_precision);
    if qty.is_zero() {
        return Err(EngineError::InvalidRequest(format!(
            "trade size {value} rounds to zero at {} decimals",
            spec.size_precision
        )));
    }
    Ok(qty)
}
