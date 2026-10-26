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
mod report;
mod strategies;

use std::{cell::RefCell, rc::Rc, sync::Arc, thread};

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_common::logging::logger::LoggerConfig;
use nautilus_execution::models::fee::{FeeModelAny, MakerTakerFeeModel};
use nautilus_model::{
    data::{Data, DataBatch, HasTsInit},
    enums::{AccountType, BookType, OmsType},
    identifiers::{InstrumentId, Venue},
    instruments::Instrument,
    types::{Currency, Money},
};
use persephone_core::{
    Candle, InstrumentSpec, Timeframe,
    signal::{Signal, SignalFactory},
    strategy::{StrategyKind, StrategySpec},
    tick::{TickFeed, Ticks},
    time::NANOS_PER_DAY,
};
use rust_decimal::Decimal;

pub use report::{BacktestReport, Fill};

use crate::strategies::{SignalStrategy, builtins};

/// Everything needed to run one backtest.
#[derive(Clone, Debug)]
pub struct BacktestRequest {
    pub instrument: InstrumentSpec,
    pub timeframe: Timeframe,
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

    let mut engine = BacktestEngine::new(engine_config())?;

    let instrument = convert::instrument(&request.instrument)?;
    let instrument_id = instrument.id();
    let quote = convert::currency(&request.instrument.quote)?;
    engine.add_venue(venue_config(request, quote, request.ticks.is_none())?)?;
    engine.add_instrument(&instrument)?;

    let bar_type = convert::bar_type(instrument_id, request.timeframe)?;
    let bars = convert::bars(
        &request.candles,
        bar_type,
        &request.instrument,
        request.timeframe,
    )?;

    let strategy = &request.strategy;
    strategy.validate().map_err(EngineError::InvalidRequest)?;
    let notional = request.starting_balance * strategy.size_fraction();
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
    engine.add_strategy(SignalStrategy::new(
        bar_type,
        Rc::clone(&signal),
        notional,
        request.instrument.size_precision,
        Rc::clone(&fills),
        Rc::clone(&failure),
    ))?;

    match &request.ticks {
        None => {
            engine.add_data(bars, None, true, true)?;
            engine.run(None, None, None, false)?;
        }
        Some(feed) => run_on_ticks(
            &mut engine,
            feed.as_ref(),
            bars,
            request,
            instrument_id,
            &failure,
        )?,
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
        request.timeframe,
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
    id: InstrumentId,
    failure: &Rc<RefCell<Option<String>>>,
) -> Result<(), EngineError> {
    let step = request.timeframe.nanos();
    let first = request.candles[0].ts;
    let end = request.candles[request.candles.len() - 1].ts + step;
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
    let rate = |r: f64| {
        Decimal::try_from(r).map_err(|e| EngineError::InvalidRequest(format!("fee rate {r}: {e}")))
    };
    let fees = MakerTakerFeeModel::new(rate(request.fees.maker)?, rate(request.fees.taker)?);

    // Margin + netting lets the EMA cross flip between long and short on a spot pair.
    let config = SimulatedVenueConfig::builder()
        .venue(Venue::from(request.instrument.venue.as_str()))
        .oms_type(OmsType::Netting)
        .account_type(AccountType::Margin)
        .book_type(BookType::L1_MBP)
        .starting_balances(vec![Money::new(request.starting_balance, quote)])
        .fee_model(FeeModelAny::MakerTaker(fees).into())
        .bar_execution(bar_execution)
        .build()
        .map_err(|e| EngineError::InvalidRequest(format!("venue config: {e}")))?;
    Ok(config)
}
