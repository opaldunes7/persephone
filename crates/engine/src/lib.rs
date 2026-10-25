//! Persephone's bridge to NautilusTrader.
//!
//! This is the only crate that imports `nautilus-*`. Its public API speaks Persephone types
//! ([`BacktestRequest`] in, [`BacktestReport`] out), so Nautilus version changes stay here.
//!
//! Nautilus's Rust core is single-threaded (`Rc`/`RefCell` throughout), so every backtest owns
//! a dedicated OS thread: use [`spawn_backtest`] from the UI, [`run_backtest`] when you are
//! already on a worker thread.

mod convert;
mod report;
mod strategies;

use std::{cell::RefCell, rc::Rc, thread};

use nautilus_backtest::{
    config::{BacktestEngineConfig, SimulatedVenueConfig},
    engine::BacktestEngine,
};
use nautilus_common::logging::logger::LoggerConfig;
use nautilus_execution::models::fee::{FeeModelAny, MakerTakerFeeModel};
use nautilus_model::{
    enums::{AccountType, BookType, OmsType},
    identifiers::Venue,
    instruments::Instrument,
    types::{Currency, Money},
};
use persephone_core::{Candle, InstrumentSpec, Timeframe};
use rust_decimal::Decimal;

pub use report::{BacktestReport, Fill};

use crate::strategies::ema_cross::EmaCross;

/// Everything needed to run one backtest.
#[derive(Clone, Debug)]
pub struct BacktestRequest {
    pub instrument: InstrumentSpec,
    pub timeframe: Timeframe,
    /// Bars to replay, oldest first.
    pub candles: Vec<Candle>,
    pub strategy: StrategySpec,
    /// Starting cash, in the instrument's quote currency.
    pub starting_balance: f64,
    pub fees: FeeRates,
}

/// Built-in strategies. WASM plugin strategies join this list in M5.
#[derive(Clone, Debug)]
pub enum StrategySpec {
    /// Goes long when the fast EMA crosses above the slow one and short on the opposite cross.
    EmaCross {
        fast: usize,
        slow: usize,
        /// Position size in base units (e.g. BTC).
        trade_size: f64,
    },
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
    let quote = Currency::from(request.instrument.quote.as_str());
    engine.add_venue(venue_config(request, quote)?)?;
    engine.add_instrument(&instrument)?;

    let bar_type = convert::bar_type(instrument_id, request.timeframe)?;
    let bars = convert::bars(
        &request.candles,
        bar_type,
        &request.instrument,
        request.timeframe,
    )?;

    let fills = Rc::new(RefCell::new(Vec::new()));
    match request.strategy {
        StrategySpec::EmaCross {
            fast,
            slow,
            trade_size,
        } => {
            if fast == 0 || fast >= slow {
                return Err(EngineError::InvalidRequest(format!(
                    "EMA periods must satisfy 0 < fast < slow (got {fast} and {slow})"
                )));
            }
            let size = convert::quantity(trade_size, &request.instrument)?;
            engine.add_strategy(EmaCross::new(bar_type, size, fast, slow, Rc::clone(&fills)))?;
        }
    }

    engine.add_data(bars, None, true, true)?;
    engine.run(None, None, None, false)?;

    let result = engine.get_result();
    let report = report::build(&result, fills.take(), &request.instrument.quote);
    engine.dispose();
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
        .build()
        .map_err(|e| EngineError::InvalidRequest(format!("venue config: {e}")))?;
    Ok(config)
}
