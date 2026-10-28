//! Persephone's bridge to NautilusTrader.
//!
//! This is the only crate that imports `nautilus-*`. Its public API speaks Persephone types
//! ([`BacktestRequest`] in, [`BacktestReport`] out), so Nautilus version changes stay here.
//!
//! Nautilus's Rust core is single-threaded (`Rc`/`RefCell` throughout), so every backtest owns
//! a dedicated OS thread: use [`spawn_backtest`] from the UI, [`run_backtest`] when you are
//! already on a worker thread.

mod convert;
pub mod indicators;
#[cfg(feature = "live")]
pub mod live;
mod models;
mod report;
mod settings;
mod strategies;

use std::{cell::RefCell, rc::Rc, sync::Arc, thread};

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_common::logging::logger::LoggerConfig;
use nautilus_core::DurationNanos;
use nautilus_execution::models::{
    fee::{FeeModelAny, FixedFeeModel, MakerTakerFeeModel},
    fill::FillModelHandle,
    latency::{LatencyModelHandle, StaticLatencyModel},
};
use nautilus_model::{
    data::{Data, DataBatch, HasTsInit},
    enums::{AccountType, BookType, OmsType},
    identifiers::{InstrumentId, Venue},
    instruments::Instrument,
    types::{Currency, Money},
};
use persephone_core::{
    BarSpec, Candle, InstrumentSpec, Timeframe, UnixNanos,
    signal::{Signal, SignalFactory},
    strategy::{StrategyKind, StrategySpec},
    tick::{TickFeed, Ticks},
    time::NANOS_PER_DAY,
};
use rust_decimal::Decimal;

pub use report::{BacktestReport, Fill};
pub use settings::{Direction, Session, SimSettings, TradingRules};

use crate::{
    models::CostFillModel,
    strategies::{SignalStrategy, builtins},
};

/// Everything needed to run one backtest.
#[derive(Clone, Debug)]
pub struct BacktestRequest {
    pub instrument: InstrumentSpec,
    /// How the candles were cut (time, ticks or volume); decides when each bar closes.
    pub bars: BarSpec,
    /// Bars to replay, oldest first.
    pub candles: Vec<Candle>,
    pub strategy: StrategySpec,
    /// Makes the strategy's signal when it is a plugin; built-ins ignore it.
    pub plugin: Option<Arc<dyn SignalFactory>>,
    /// Ticks to fill orders on. The strategy still decides on the bars, but its orders fill at
    /// the prices the ticks show when they arrive: the ask and bid for quotes, the latest
    /// trade for trades. Without ticks, orders fill on the bars.
    pub ticks: Option<Arc<dyn TickFeed>>,
    /// Starting cash, in the instrument's quote currency.
    pub starting_balance: f64,
    pub fees: FeeRates,
    /// Flat fees, slippage, spread, latency and leverage.
    pub sim: SimSettings,
    pub rules: TradingRules,
    /// The strategy sees bars closing up to here without trading on them, to warm up.
    pub trade_from: UnixNanos,
    /// 1-minute bars spanning the candles, for orders to fill on when there are no ticks; the
    /// strategy still decides on the candles.
    pub fill_bars: Option<Arc<Vec<Candle>>>,
}

/// Maker/taker fee rates as fractions (0.001 = 0.1%).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FeeRates {
    pub maker: f64,
    pub taker: f64,
}

impl FeeRates {
    pub const ZERO: Self = Self {
        maker: 0.0,
        taker: 0.0,
    };
    /// Binance spot's base tier.
    pub const BINANCE_SPOT: Self = Self {
        maker: 0.001,
        taker: 0.001,
    };
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("no bars to backtest")]
    NoData,
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("backtest thread panicked")]
    Panicked,
    /// The strategy itself failed (e.g. a plugin crashed or ran out of time).
    #[error("{0}")]
    Strategy(String),
    #[error("couldn't read the ticks: {0}")]
    Ticks(String),
    #[error(transparent)]
    Nautilus(#[from] anyhow::Error),
}

/// Runs a backtest on the current thread and blocks until it finishes.
///
/// # Errors
///
/// Returns an error if the request is invalid or Nautilus aborts the run.
pub fn run_backtest(request: &BacktestRequest) -> Result<BacktestReport, EngineError> {
    if request.candles.is_empty() {
        return Err(EngineError::NoData);
    }

    if !(request.sim.leverage >= 1.0 && request.sim.leverage.is_finite()) {
        return Err(EngineError::InvalidRequest(format!(
            "leverage must be 1 or more, not {}",
            request.sim.leverage
        )));
    }
    let mut engine = BacktestEngine::new(engine_config())?;

    let instrument = convert::instrument(&request.instrument)?;
    let instrument_id = instrument.id();
    let quote = convert::currency(&request.instrument.quote)?;
    engine.add_venue(venue_config(request, quote, request.ticks.is_none())?)?;
    engine.add_instrument(&instrument)?;

    let closes = request.bars.closes(&request.candles);
    let bar_type = match (request.bars, &request.ticks) {
        (BarSpec::Ticks(_) | BarSpec::Volume(_), None) => convert::untimed_bar_type(instrument_id)?,
        (bars, _) => convert::bar_type(instrument_id, bars)?,
    };
    let mut bars = convert::bars(&request.candles, &closes, bar_type, &request.instrument)?;
    // Nautilus fills on the finest bars it's given. They go first so that, of bars closing
    // at the same moment, the finer one is matched before the strategy acts on the other.
    if let Some(fine) = request
        .fill_bars
        .as_ref()
        .filter(|_| request.ticks.is_none())
    {
        let minute: BarSpec = Timeframe::M1.into();
        let fine_type = convert::bar_type(instrument_id, minute)?;
        let mut all = convert::bars(fine, &minute.closes(fine), fine_type, &request.instrument)?;
        all.append(&mut bars);
        bars = all;
    }

    let strategy = &request.strategy;
    strategy.validate().map_err(EngineError::InvalidRequest)?;
    let notional = request.starting_balance * strategy.size_fraction() * request.sim.leverage;
    let last_close = request.candles.last().map_or(0.0, |c| c.close);
    convert::quantity(notional / last_close, &request.instrument)?;

    let signal: Box<dyn Signal> = match &strategy.kind {
        StrategyKind::BuiltIn(kind) => builtins::signal(*kind, strategy),
        StrategyKind::Plugin(id) => request
            .plugin
            .as_ref()
            .ok_or_else(|| EngineError::InvalidRequest(format!("plugin `{id}` isn't loaded")))?
            .create(strategy.own_params())
            .map_err(EngineError::Strategy)?,
    };

    let fills = Rc::new(RefCell::new(Vec::new()));
    let failure = Rc::new(RefCell::new(None));
    let signal = Rc::new(RefCell::new(signal));
    engine.add_strategy(
        SignalStrategy::new(
            bar_type,
            Rc::clone(&signal),
            notional,
            request.instrument.size_precision,
            Rc::clone(&fills),
            Rc::clone(&failure),
        )
        .ruled(request.rules.clone(), request.trade_from),
    )?;

    match &request.ticks {
        None => {
            engine.add_data(bars, None, true, true)?;
            engine.run(None, None, None, false)?;
        }
        Some(feed) => {
            let end = closes.last().copied().unwrap_or_default();
            run_on_ticks(
                &mut engine,
                feed.as_ref(),
                bars,
                request,
                end,
                instrument_id,
                &failure,
            )?;
        }
    }
    if let Some(message) = failure.take() {
        engine.dispose();
        return Err(EngineError::Strategy(message));
    }

    let result = engine.get_result();
    let mut report = report::build(
        &result,
        fills.take(),
        &request.candles,
        &closes,
        request.starting_balance,
        &request.instrument.quote,
    );
    engine.dispose();
    report.notes = signal.borrow_mut().notes();
    Ok(report)
}

/// Runs a backtest on its own thread. Join the handle to get the report.
#[must_use]
pub fn spawn_backtest(
    request: BacktestRequest,
) -> thread::JoinHandle<Result<BacktestReport, EngineError>> {
    thread::Builder::new()
        .name("persephone-backtest".into())
        .spawn(move || run_backtest(&request))
        .expect("failed to spawn backtest thread")
}

/// How many ticks to gather (whole days at a time) before handing them to Nautilus.
const TICKS_PER_BATCH: usize = 1_000_000;

/// Streams the ticks through Nautilus a day at a time, quiet days bundled, each batch with the
/// bars that close inside it, so memory stays bounded however much history there is.
fn run_on_ticks(
    engine: &mut BacktestEngine,
    feed: &dyn TickFeed,
    bars: Vec<Data>,
    request: &BacktestRequest,
    end: UnixNanos,
    id: InstrumentId,
    failure: &Rc<RefCell<Option<String>>>,
) -> Result<(), EngineError> {
    let first = request.candles[0].ts;
    let spec = &request.instrument;

    let mut bars = bars.into_iter().peekable();
    let mut batch_bars = Vec::new();
    let mut batch_ticks: Option<Ticks> = None;
    let mut next_trade_id = 0;
    let mut day = first - first % NANOS_PER_DAY;

    while day < end {
        let next = day + NANOS_PER_DAY;
        let ticks = feed.ticks(day, next).map_err(EngineError::Ticks)?;
        match (&mut batch_ticks, ticks) {
            (None, ticks) => batch_ticks = Some(ticks),
            (Some(Ticks::Trades(held)), Ticks::Trades(t)) => held.extend(t),
            (Some(Ticks::Quotes(held)), Ticks::Quotes(q)) => held.extend(q),
            _ => {
                return Err(EngineError::Ticks(
                    "the feed mixed trades and quotes".into(),
                ));
            }
        }
        // Bars are stamped with their close time; take those closing in this window.
        while bars.peek().is_some_and(|bar| bar.ts_init().as_u64() < next) {
            batch_bars.extend(bars.next());
        }

        let last = next >= end;
        if last {
            batch_bars.extend(bars.by_ref());
        }
        if last
            || batch_ticks
                .as_ref()
                .is_some_and(|t| t.len() >= TICKS_PER_BATCH)
        {
            match batch_ticks.take() {
                Some(Ticks::Trades(trades)) if !trades.is_empty() => {
                    let ticks = convert::trade_ticks(&trades, id, spec, &mut next_trade_id)?;
                    engine.add_data_batch(DataBatch::from(ticks), None, true, true)?;
                }
                Some(Ticks::Quotes(quotes)) if !quotes.is_empty() => {
                    let ticks = convert::quote_ticks(&quotes, id, spec)?;
                    engine.add_data_batch(DataBatch::from(ticks), None, true, true)?;
                }
                _ => {}
            }
            if !batch_bars.is_empty() {
                engine.add_data(std::mem::take(&mut batch_bars), None, true, true)?;
            }
            engine.run(None, None, None, true)?;
            engine.clear_data();
            if failure.borrow().is_some() {
                break;
            }
        }
        day = next;
    }
    engine.end()?;
    Ok(())
}

fn engine_config() -> BacktestEngineConfig {
    BacktestEngineConfig {
        logging: LoggerConfig {
            stdout_level: log::LevelFilter::Warn,
            ..LoggerConfig::default()
        },
        ..BacktestEngineConfig::default()
    }
}

fn venue_config(
    request: &BacktestRequest,
    quote: Currency,
    bar_execution: bool,
) -> Result<SimulatedVenueConfig, EngineError> {
    let decimal = |what: &str, v: f64| {
        Decimal::try_from(v).map_err(|e| EngineError::InvalidRequest(format!("{what} {v}: {e}")))
    };
    let sim = &request.sim;
    let fees = match sim.fee_per_order {
        Some(fee) => FeeModelAny::Fixed(FixedFeeModel::new(Money::new(fee, quote), Some(true))?),
        None => FeeModelAny::MakerTaker(MakerTakerFeeModel::new(
            decimal("fee rate", request.fees.maker)?,
            decimal("fee rate", request.fees.taker)?,
        )),
    };

    // Margin + netting lets the EMA cross flip between long and short on a spot pair.
    let config = SimulatedVenueConfig::builder()
        .venue(Venue::from(request.instrument.venue.as_str()))
        .oms_type(OmsType::Netting)
        .account_type(AccountType::Margin)
        .book_type(BookType::L1_MBP)
        .starting_balances(vec![Money::new(request.starting_balance, quote)])
        .default_leverage(decimal("leverage", sim.leverage)?)
        .fee_model(fees.into())
        .fill_model(FillModelHandle::new(CostFillModel::new(
            sim.slippage_ticks,
            sim.spread_ticks,
        )))
        .maybe_latency_model((sim.latency_ms > 0).then(|| {
            let latency = DurationNanos::new(u64::from(sim.latency_ms) * 1_000_000);
            let none = DurationNanos::new(0);
            LatencyModelHandle::new(StaticLatencyModel::new(latency, none, none, none))
        }))
        .bar_execution(bar_execution)
        .build()
        .map_err(|e| EngineError::InvalidRequest(format!("venue config: {e}")))?;
    Ok(config)
}
