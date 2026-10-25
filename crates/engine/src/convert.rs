//! Persephone types → Nautilus types.

use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, BarSpecification, BarType, Data},
    enums::{AggregationSource, BarAggregation, PriceType},
    identifiers::{InstrumentId, Symbol},
    instruments::{CurrencyPair, InstrumentAny},
    types::{Currency, Price, Quantity},
};
use persephone_core::{Candle, InstrumentSpec, Timeframe};

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
        .base_currency(Currency::from(spec.base.as_str()))
        .quote_currency(Currency::from(spec.quote.as_str()))
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

pub(crate) fn bar_type(id: InstrumentId, timeframe: Timeframe) -> Result<BarType, EngineError> {
    let (step, aggregation) = match timeframe {
        Timeframe::M1 => (1, BarAggregation::Minute),
        Timeframe::M5 => (5, BarAggregation::Minute),
        Timeframe::M15 => (15, BarAggregation::Minute),
        Timeframe::M30 => (30, BarAggregation::Minute),
        Timeframe::H1 => (1, BarAggregation::Hour),
        Timeframe::H4 => (4, BarAggregation::Hour),
        Timeframe::D1 => (1, BarAggregation::Day),
    };
    let spec = BarSpecification::new_checked(step, aggregation, PriceType::Last)?;
    Ok(BarType::new(id, spec, AggregationSource::External))
}

/// Converts candles to Nautilus bars.
///
/// Nautilus stamps a bar with its **close** time, while [`Candle::ts`] is the open time. Shifting
/// by one timeframe keeps a strategy from seeing a bar before it has closed.
pub(crate) fn bars(
    candles: &[Candle],
    bar_type: BarType,
    spec: &InstrumentSpec,
    timeframe: Timeframe,
) -> Result<Vec<Data>, EngineError> {
    let px = |v: f64| Price::new(v, spec.price_precision);
    candles
        .iter()
        .map(|c| {
            let closed_at = UnixNanos::from(c.ts + timeframe.nanos());
            let bar = Bar::new_checked(
                bar_type,
                px(c.open),
                px(c.high),
                px(c.low),
                px(c.close),
                Quantity::new(c.volume, spec.size_precision),
                closed_at,
                closed_at,
            )?;
            Ok(Data::Bar(bar))
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
