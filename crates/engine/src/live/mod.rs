//! Paper and live trading: the strategies that backtest, run by Nautilus's live node on bars as
//! they close.
//!
//! Bars come from Binance's public streams, which need no account. Orders go to:
//! - **paper**: Nautilus's sandbox, which fills them against those bars as a backtest would;
//! - **testnet**: Binance's spot testnet, with keys from `BINANCE_TESTNET_API_KEY` and
//!   `BINANCE_TESTNET_API_SECRET`;
//! - **real money**: Binance itself, with keys from `BINANCE_API_KEY` and `BINANCE_API_SECRET`.
//!
//! Keys are read by Nautilus from the environment and never pass through Persephone. Spot
//! accounts can't borrow, so on the testnet and with real money a strategy goes flat where it
//! would go short. Stopping a session closes its position.
//!
//! Like backtests, each session owns a thread: the node runs on a single-threaded Tokio runtime
//! there and reports through a callback.

mod replay;

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nautilus_binance::{
    common::enums::{BinanceEnvironment, BinanceProductType},
    config::{BinanceDataClientConfig, BinanceExecutionClientConfig, BinanceSpotMarketDataMode},
    factories::{BinanceDataClientFactory, BinanceExecutionClientFactory},
};
use nautilus_common::{enums::Environment, logging::logger::LoggerConfig};
use nautilus_core::UnixNanos as NautilusNanos;
use nautilus_execution::models::fee::{FeeModelAny, MakerTakerFeeModel};
use nautilus_live::node::{LiveNode, NodeRunMode};
use nautilus_model::{
    data::{Bar, BarType},
    enums::{AccountType, OmsType},
    identifiers::{AccountId, InstrumentId, TraderId, Venue},
    types::{Money, Price, Quantity},
};
use nautilus_sandbox::{SandboxExecutionClientConfig, SandboxExecutionClientFactory};
use persephone_core::{
    BarSpec, Candle, InstrumentSpec, Side, Timeframe, UnixNanos,
    signal::{Signal, SignalFactory},
    strategy::{StrategyKind, StrategySpec},
};
use rust_decimal::Decimal;

use crate::{
    EngineError, FeeRates, TradingRules, convert,
    report::RawFill,
    strategies::{SignalStrategy, Watcher, builtins},
};

/// Where a session's orders go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveMode {
    /// Simulated fills on live bars; no account needed.
    Paper,
    /// Binance's spot testnet: real orders, play money.
    Testnet,
    /// Binance itself.
    RealMoney,
}

impl LiveMode {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Paper => "paper",
            Self::Testnet => "testnet",
            Self::RealMoney => "real money",
        }
    }

    /// The environment variables Nautilus reads this mode's API key and secret from.
    #[must_use]
    pub const fn key_variables(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Paper => None,
            Self::Testnet => Some(("BINANCE_TESTNET_API_KEY", "BINANCE_TESTNET_API_SECRET")),
            Self::RealMoney => Some(("BINANCE_API_KEY", "BINANCE_API_SECRET")),
        }
    }

    /// Whether this mode's keys are set (without reading them).
    #[must_use]
    pub fn has_keys(self) -> bool {
        let set = |name| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        self.key_variables()
            .is_none_or(|(key, secret)| set(key) && set(secret))
    }
}

/// Where a session's bars come from.
#[derive(Clone, Debug)]
pub enum LiveFeed {
    /// Binance's public kline streams.
    Binance,
    /// These bars (oldest first), one every `every`: a rehearsal that works offline.
    Replay { bars: Vec<Candle>, every: Duration },
}

/// Everything needed to start trading a strategy live.
#[derive(Clone, Debug)]
pub struct LiveRequest {
    pub mode: LiveMode,
    pub feed: LiveFeed,
    pub instrument: InstrumentSpec,
    pub timeframe: Timeframe,
    /// Recent bars (oldest first) the strategy sees before going live, so its indicators are
    /// warm. A bar still forming is skipped.
    pub history: Vec<Candle>,
    pub strategy: StrategySpec,
    /// Makes the strategy's signal when it is a plugin; built-ins ignore it.
    pub plugin: Option<Arc<dyn SignalFactory>>,
    /// The cash positions are sized from, in the quote currency: the paper account's balance,
    /// or how much of a real account the strategy may use (the account itself isn't checked).
    pub balance: f64,
    /// The paper account's fees; real venues charge their own.
    pub fees: FeeRates,
    /// Which way it may trade and when it may open positions, as in its backtests.
    pub rules: TradingRules,
    /// Whether the node handles Ctrl-C itself (in a terminal). Apps stop it with
    /// [`LiveSession::stop`].
    pub owns_signals: bool,
}

/// What a live session reports, in order.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveEvent {
    /// Connected; the strategy is waiting for the next bar to close.
    Started,
    /// A bar closed and the strategy has seen it. Stamped with its open time, like charts.
    Bar(Candle),
    Fill(LiveFill),
    /// An order was refused, or the strategy failed (then the session stops).
    Problem(String),
    /// The session is over, with the reason if it failed.
    Stopped(Option<String>),
}

/// One execution in a live session.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LiveFill {
    pub ts: UnixNanos,
    /// Open time of the bar whose close triggered it.
    pub bar: UnixNanos,
    pub side: Side,
    pub price: f64,
    pub qty: f64,
    /// Commission paid, in the quote currency.
    pub fee: f64,
}

/// A session's position and profit, kept from its events and marked to the latest close.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Book {
    /// Signed, in the base asset.
    pub position: f64,
    /// Cash from trading, fees taken off, in the quote currency.
    pub cash: f64,
    /// The latest price: the last close or fill.
    pub mark: f64,
    pub fees: f64,
    pub fills: usize,
}

impl Book {
    pub fn apply(&mut self, event: &LiveEvent) {
        match event {
            LiveEvent::Bar(bar) => self.mark = bar.close,
            LiveEvent::Fill(fill) => {
                let signed = if fill.side == Side::Buy {
                    fill.qty
                } else {
                    -fill.qty
                };
                self.position += signed;
                self.cash -= signed * fill.price + fill.fee;
                self.mark = fill.price;
                self.fees += fill.fee;
                self.fills += 1;
            }
            _ => {}
        }
    }

    /// Profit so far, the open position valued at the mark.
    #[must_use]
    pub fn pnl(&self) -> f64 {
        self.cash + self.position * self.mark
    }
}

/// A running live session. Dropping it stops the session without waiting.
#[derive(Debug)]
pub struct LiveSession {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl LiveSession {
    /// Asks the session to stop; it reports [`LiveEvent::Stopped`] once it has.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }

    /// Waits for the session to end (call [`stop`](Self::stop) first, or let the feed run out).
    pub fn join(mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for LiveSession {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts trading on a thread of its own. `events` hears everything the session does, on that
/// thread, ending with [`LiveEvent::Stopped`].
///
/// # Errors
///
/// Returns an error if the request doesn't make sense (a replay with real orders, a market
/// Binance doesn't list); failures after that arrive as events.
pub fn start_live(
    request: LiveRequest,
    events: impl FnMut(LiveEvent) + Send + 'static,
) -> Result<LiveSession, EngineError> {
    check(&request)?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = thread::Builder::new()
        .name("persephone-live".into())
        .spawn(move || {
            let events: Events = Rc::new(RefCell::new(Box::new(events)));
            let outcome = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| EngineError::Nautilus(e.into()))
                .and_then(|runtime| runtime.block_on(run(&request, &events, flag)));
            (events.borrow_mut())(LiveEvent::Stopped(outcome.err().map(|e| e.to_string())));
        })
        .map_err(|e| EngineError::Nautilus(e.into()))?;
    Ok(LiveSession {
        stop,
        thread: Some(thread),
    })
}

type Events = Rc<RefCell<Box<dyn FnMut(LiveEvent) + Send>>>;

fn check(request: &LiveRequest) -> Result<(), EngineError> {
    let invalid = |message: &str| Err(EngineError::InvalidRequest(message.into()));
    request
        .strategy
        .validate()
        .map_err(EngineError::InvalidRequest)?;
    match (&request.feed, request.mode) {
        (LiveFeed::Replay { .. }, LiveMode::Testnet | LiveMode::RealMoney) => {
            return invalid("a replay can only paper trade");
        }
        (LiveFeed::Binance, _) if request.instrument.venue != "BINANCE" => {
            return invalid("live trading needs a Binance market");
        }
        _ => {}
    }
    if !(request.balance.is_finite() && request.balance > 0.0) {
        return invalid("the balance must be more than zero");
    }
    Ok(())
}

async fn run(
    request: &LiveRequest,
    events: &Events,
    stop: Arc<AtomicBool>,
) -> Result<(), EngineError> {
    let spec = &request.instrument;
    let instrument_id = InstrumentId::from(format!("{}.{}", spec.symbol, spec.venue).as_str());
    let bars = BarSpec::Time(request.timeframe);
    let bar_type = convert::bar_type(instrument_id, bars)?;

    let mut builder = LiveNode::builder(TraderId::from("PERSEPHONE-001"), Environment::Live)?
        .with_name("PERSEPHONE")
        .with_load_state(false)
        .with_save_state(false)
        // Problems arrive as events; the log would only repeat them.
        .with_logging(LoggerConfig {
            stdout_level: log::LevelFilter::Error,
            ..LoggerConfig::default()
        })
        // Time for the closing order to fill once stopped.
        .with_delay_post_stop_secs(if request.mode == LiveMode::Paper {
            1
        } else {
            5
        });

    let environment = match request.mode {
        LiveMode::Paper | LiveMode::RealMoney => BinanceEnvironment::Live,
        LiveMode::Testnet => BinanceEnvironment::Testnet,
    };
    builder = match &request.feed {
        LiveFeed::Binance => builder.add_data_client(
            None,
            Box::new(BinanceDataClientFactory::new()),
            Box::new(BinanceDataClientConfig {
                product_type: BinanceProductType::Spot,
                environment,
                // Public streams: no keys, and the only ones with klines.
                spot_market_data_mode: BinanceSpotMarketDataMode::Json,
                ..Default::default()
            }),
        )?,
        LiveFeed::Replay {
            bars: candles,
            every,
        } => builder.add_data_client(
            None,
            Box::new(replay::ReplayFactory),
            Box::new(replay::ReplayConfig {
                instrument: convert::instrument(spec)?,
                bars: replay_bars(candles, bars, bar_type, spec)?,
                every: *every,
            }),
        )?,
    };
    builder = match request.mode {
        LiveMode::Paper => builder.add_simulated_exec_client(
            Some(spec.venue.clone()),
            Box::new(SandboxExecutionClientFactory::new()),
            Box::new(sandbox(request)?),
        )?,
        LiveMode::Testnet | LiveMode::RealMoney => builder
            .add_exec_client(
                None,
                Box::new(BinanceExecutionClientFactory::new()),
                Box::new(BinanceExecutionClientConfig {
                    account_id: AccountId::from("BINANCE-001"),
                    product_type: BinanceProductType::Spot,
                    environment,
                    ..Default::default()
                }),
            )?
            .with_reconciliation(true),
    };
    let mut node = builder.build()?;

    let signal = Rc::new(RefCell::new(signal(request)?));
    warm_up(&mut **signal.borrow_mut(), request)?;
    let notional = request.balance * request.strategy.size_fraction();
    let strategy = SignalStrategy::new(
        bar_type,
        signal,
        notional,
        spec.size_precision,
        Rc::default(),
        Rc::default(),
    )
    .ruled(request.rules.clone(), 0)
    .watched(
        Box::new(Reporter {
            events: Rc::clone(events),
            timeframe: request.timeframe,
            last_bar: None,
            stop: Arc::clone(&stop),
        }),
        request.mode != LiveMode::Paper,
    );
    node.add_strategy(strategy)?;

    // The node's handle is how another thread stops it; poll the session's flag for it.
    let handle = node.handle();
    tokio::spawn(async move {
        while !stop.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        loop {
            handle.stop();
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let mode = if request.owns_signals {
        NodeRunMode::Owned
    } else {
        NodeRunMode::Hosted
    };
    let outcome = node.run_with_mode(mode).await;
    node.dispose();
    outcome?;
    Ok(())
}

fn signal(request: &LiveRequest) -> Result<Box<dyn Signal>, EngineError> {
    let strategy = &request.strategy;
    Ok(match &strategy.kind {
        StrategyKind::BuiltIn(kind) => builtins::signal(*kind, strategy),
        StrategyKind::Plugin(id) => request
            .plugin
            .as_ref()
            .ok_or_else(|| EngineError::InvalidRequest(format!("plugin `{id}` isn't loaded")))?
            .create(strategy.own_params())
            .map_err(EngineError::Strategy)?,
    })
}

/// Shows the signal the history, stamped with close times as in a backtest, leaving out a bar
/// that hasn't closed yet. What it wants meanwhile is ignored: the session starts flat.
fn warm_up(signal: &mut dyn Signal, request: &LiveRequest) -> Result<(), EngineError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    for candle in &request.history {
        let close = request.timeframe.close_time(candle.ts);
        if close > now {
            break;
        }
        signal
            .on_bar(&Candle {
                ts: close,
                ..*candle
            })
            .map_err(EngineError::Strategy)?;
    }
    Ok(())
}

/// Bars for the replay feed: stamped with their close times, with their own volume (live bars
/// carry real volume too).
fn replay_bars(
    candles: &[Candle],
    bars: BarSpec,
    bar_type: BarType,
    spec: &InstrumentSpec,
) -> Result<Vec<Bar>, EngineError> {
    let px = |v: f64| Price::new(v, spec.price_precision);
    let closes = bars.closes(candles);
    candles
        .iter()
        .zip(closes)
        .map(|(c, close)| {
            let ts = NautilusNanos::from(close);
            let volume = Quantity::new(c.volume, spec.size_precision);
            Ok(Bar::new_checked(
                bar_type,
                px(c.open),
                px(c.high),
                px(c.low),
                px(c.close),
                volume,
                ts,
                ts,
            )?)
        })
        .collect()
}

/// The paper account: margin and netting, as in backtests, so strategies can go short.
fn sandbox(request: &LiveRequest) -> Result<SandboxExecutionClientConfig, EngineError> {
    let quote = convert::currency(&request.instrument.quote)?;
    let rate = |r: f64| {
        Decimal::try_from(r).map_err(|e| EngineError::InvalidRequest(format!("fee rate {r}: {e}")))
    };
    let fees = MakerTakerFeeModel::new(rate(request.fees.maker)?, rate(request.fees.taker)?);
    Ok(SandboxExecutionClientConfig::builder()
        .account_id(AccountId::from(
            format!("{}-PAPER", request.instrument.venue).as_str(),
        ))
        .venue(Venue::from(request.instrument.venue.as_str()))
        .starting_balances(vec![Money::new(request.balance, quote)])
        .base_currency(quote)
        .oms_type(OmsType::Netting)
        .account_type(AccountType::Margin)
        .fee_model(FeeModelAny::MakerTaker(fees))
        .build())
}

/// Turns what the strategy sees and does into [`LiveEvent`]s.
struct Reporter {
    events: Events,
    timeframe: Timeframe,
    /// Open time of the latest bar.
    last_bar: Option<UnixNanos>,
    stop: Arc<AtomicBool>,
}

impl Watcher for Reporter {
    fn started(&mut self) {
        (self.events.borrow_mut())(LiveEvent::Started);
    }

    fn bar(&mut self, bar: &Candle) {
        // Bars are stamped at their close (Binance's close is a millisecond early).
        let open = self.timeframe.open_time(bar.ts - 1);
        self.last_bar = Some(open);
        (self.events.borrow_mut())(LiveEvent::Bar(Candle { ts: open, ..*bar }));
    }

    fn fill(&mut self, fill: &RawFill) {
        (self.events.borrow_mut())(LiveEvent::Fill(LiveFill {
            ts: fill.ts,
            bar: self.last_bar.unwrap_or(fill.ts),
            side: fill.side,
            price: fill.price,
            qty: fill.qty,
            fee: fill.fee,
        }));
    }

    fn problem(&mut self, message: String) {
        if message.starts_with("the strategy failed") {
            self.stop.store(true, Ordering::Relaxed);
        }
        (self.events.borrow_mut())(LiveEvent::Problem(message));
    }
}
