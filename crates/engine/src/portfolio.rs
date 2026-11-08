//! Several markets on one account. Legs share one cash balance and one simulated venue, and everything
//! is worked out on one timeline, so that a profit on one supports another and nothing is ever valued
//! from prices of different times.

use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};

use nautilus_backtest::{config::SimulatedVenueConfig, engine::BacktestEngine};
use nautilus_core::DurationNanos;
use nautilus_execution::models::{
    fee::{FeeModel, FeeModelAny, FeeModelHandle, FixedFeeModel, MakerTakerFeeModel},
    fill::FillModelHandle,
    latency::{LatencyModelHandle, StaticLatencyModel},
};
use nautilus_model::{
    data::DataBatch,
    enums::{AccountType, BookType, OmsType},
    identifiers::{InstrumentId, Venue},
    instruments::{Instrument, InstrumentAny},
    orders::OrderAny,
    types::{Money, Price, Quantity},
};
use persephone_core::{
    BarSpec, Candle, InstrumentSpec, Side, UnixNanos,
    signal::{Note, Signal, SignalFactory},
    strategy::{StrategyKind, StrategySpec},
    tick::{Aggressor, Trade},
    time::{DateTime, NANOS_PER_MINUTE},
};
use rust_decimal::Decimal;

use crate::{
    EngineError, FeeRates,
    models::{BarNow, CostFillModel, JitteredLatency, range_costs},
    report::{self, Fill, Holding},
    settings::{SimSettings, TradingRules},
    strategies::{
        builtins,
        portfolio::{Leg, Outcome, PortfolioStrategy, Settings, Step, Wants},
    },
};

pub use crate::strategies::portfolio::Rebalance;

/// One market in a portfolio.
#[derive(Clone, Debug)]
pub struct PortfolioLeg {
    pub instrument: InstrumentSpec,
    /// Its bars, oldest first.
    pub candles: Vec<Candle>,
    /// What decides its position. Without one it holds its weight, or follows the weight table.
    pub strategy: Option<StrategySpec>,
    /// Makes the strategy's signal when it's a plugin.
    pub plugin: Option<Arc<dyn SignalFactory>>,
    /// With a strategy, what its position size is multiplied by. Without one, the signed share of
    /// the capital to hold. Unused with a weight table.
    pub weight: f64,
    pub fees: FeeRates,
    /// A perpetual's funding rates by when they're paid, as fractions.
    pub funding: Option<Arc<Vec<(UnixNanos, f64)>>>,
}

/// Weights by when they apply, one for each leg.
pub type WeightTable = Arc<Vec<(UnixNanos, Vec<f64>)>>;

/// Everything needed to run one portfolio.
#[derive(Clone, Debug)]
pub struct PortfolioRequest {
    pub legs: Vec<PortfolioLeg>,
    /// How every leg's candles were cut. Time bars only.
    pub bars: BarSpec,
    /// Each row is what to hold from then on, one signed share of the account for each leg, whatever
    /// a row leaves at zero going flat. Rows apply at the first close at or after their time.
    pub weights: Option<WeightTable>,
    pub rebalance: Rebalance,
    pub starting_balance: f64,
    pub sim: SimSettings,
    pub rules: TradingRules,
    pub trade_from: UnixNanos,
    /// What a position needs to stay open, as a percentage of its value. Everything is liquidated when
    /// the account falls to that.
    pub maintenance_percent: f64,
}

/// One market's part in a portfolio.
#[derive(Clone, Debug, Default)]
pub struct LegReport {
    pub symbol: String,
    pub fills: Vec<Fill>,
    /// What it has made so far at each moment on the timeline, costs taken off.
    pub contribution: Vec<f64>,
    /// The starting balance plus that, to draw on one scale with the account.
    pub account_line: Vec<f64>,
    /// What the position was worth at each moment, signed.
    pub values: Vec<f64>,
    /// Its close at each moment, the last known where it had no bar.
    pub closes: Vec<f64>,
    /// Whether it really had a bar at each moment.
    pub real: Vec<bool>,
    /// What traded in its bar at each moment.
    pub volumes: Vec<f64>,
    pub fees: f64,
    pub funding_paid: f64,
    pub borrow_paid: f64,
    pub orders_worked: usize,
    pub bars_to_fill_average: f64,
    pub bars_to_fill_longest: usize,
    pub unfilled_at_end: f64,
    /// Moments after its first bar with none of its own.
    pub missing_bars: usize,
}

/// What a finished portfolio hands back.
#[derive(Clone, Debug, Default)]
pub struct PortfolioReport {
    /// The moments on the shared timeline: every leg's bar closes, joined.
    pub times: Vec<UnixNanos>,
    pub equity: Vec<f64>,
    pub starting_balance: f64,
    pub max_drawdown: f64,
    pub fees: f64,
    pub funding_paid: f64,
    pub borrow_paid: f64,
    /// What was charged for cash below zero.
    pub interest_paid: f64,
    pub total_orders: usize,
    pub legs: Vec<LegReport>,
    /// What's held long plus short, and long minus short, as shares of the account at each moment.
    pub gross_exposure: Vec<f64>,
    pub net_exposure: Vec<f64>,
    /// All that was traded, as a multiple of the starting balance.
    pub turnover: f64,
    /// The most of the account margin ever took.
    pub peak_margin_use: f64,
    /// The least the account ever was, at the worst of a bar, as a multiple of what its positions
    /// needed to stay open. `None` if it never held anything.
    pub closest_to_liquidation: Option<f64>,
    pub liquidated_at: Option<UnixNanos>,
    /// Orders made smaller to keep within `max_gross_percent`.
    pub orders_capped: usize,
    pub notes: Vec<Note>,
}

impl PortfolioReport {
    /// The final return, as a fraction of the starting balance.
    #[must_use]
    pub fn net_return(&self) -> f64 {
        self.equity
            .last()
            .map_or(0.0, |last| last / self.starting_balance - 1.0)
    }
}

/// Fees by market, so spot and perpetual legs each pay their own on the one venue.
#[derive(Debug)]
struct FeesByMarket(HashMap<InstrumentId, FeeModelAny>);

impl FeeModel for FeesByMarket {
    fn get_commission(
        &self,
        order: &OrderAny,
        fill_quantity: Quantity,
        fill_px: Price,
        instrument: &InstrumentAny,
    ) -> anyhow::Result<Money> {
        self.0
            .get(&instrument.id())
            .ok_or_else(|| anyhow::anyhow!("no fees for {}", instrument.id()))?
            .get_commission(order, fill_quantity, fill_px, instrument)
    }
}

fn refuse(message: impl Into<String>) -> EngineError {
    EngineError::InvalidRequest(message.into())
}

fn check(request: &PortfolioRequest) -> Result<(), EngineError> {
    let sim = &request.sim;
    if request.legs.is_empty() {
        return Err(refuse("a portfolio needs at least one market"));
    }
    if !matches!(request.bars, BarSpec::Time(_)) {
        return Err(refuse(
            "a portfolio needs time bars, so its markets line up",
        ));
    }
    if !(sim.leverage >= 1.0 && sim.leverage.is_finite()) {
        return Err(refuse(format!(
            "leverage must be 1 or more, not {}",
            sim.leverage
        )));
    }
    if let Some(share) = sim.max_participation_percent
        && !(share > 0.0 && share <= 100.0)
    {
        return Err(refuse(format!(
            "the most of a bar's volume an order can take must be above 0% and at most 100%, not {share}%"
        )));
    }
    if sim.limit.is_some()
        || sim.stop_loss_percent.is_some()
        || sim.take_profit_percent.is_some()
        || sim.trailing_stop_percent.is_some()
    {
        return Err(refuse(
            "a portfolio goes at the market for now: limit orders, stops and targets are for single markets",
        ));
    }
    if sim.close_at_end {
        return Err(refuse(
            "closing at the end is for single markets for now: a portfolio's holdings are valued at the last close",
        ));
    }
    if sim.next_open && (sim.latency_ms > 0 || sim.latency_jitter_ms > 0) {
        return Err(refuse(
            "filling at the next open can't go with latency; pick one",
        ));
    }
    let quote = &request.legs[0].instrument.quote;
    for (index, leg) in request.legs.iter().enumerate() {
        if leg.candles.is_empty() {
            return Err(refuse(format!("{} has no bars", leg.instrument.symbol)));
        }
        if leg.instrument.quote != *quote {
            return Err(refuse(format!(
                "every market must be quoted in the same currency to share an account: {} is in {} and {} in {}",
                request.legs[0].instrument.symbol,
                quote,
                leg.instrument.symbol,
                leg.instrument.quote,
            )));
        }
        if request.legs[..index]
            .iter()
            .any(|other| other.instrument.symbol == leg.instrument.symbol)
        {
            return Err(refuse(format!(
                "{} is in there twice",
                leg.instrument.symbol
            )));
        }
        if request.weights.is_some() && leg.strategy.is_some() {
            return Err(refuse(format!(
                "{} has a strategy and there's a weight table too; pick one",
                leg.instrument.symbol
            )));
        }
        if leg.strategy.is_some() && (leg.weight.is_nan() || leg.weight <= 0.0) {
            return Err(refuse(format!(
                "{}'s weight scales its strategy's position, so must be above 0; invert the strategy to trade it the other way",
                leg.instrument.symbol
            )));
        }
    }
    if let Some(table) = &request.weights {
        for (at, (when, weights)) in table.iter().enumerate() {
            if weights.len() != request.legs.len() {
                return Err(refuse(format!(
                    "weight row {at} has {} weights for {} markets",
                    weights.len(),
                    request.legs.len()
                )));
            }
            if weights.iter().any(|weight| !weight.is_finite()) {
                return Err(refuse(format!(
                    "weight row {at} has a weight that isn't a number"
                )));
            }
            if at > 0 && table[at - 1].0 > *when {
                return Err(refuse("weight rows must be in time order"));
            }
        }
    }
    Ok(())
}

/// The moments every leg's bars close at, joined.
fn timeline(request: &PortfolioRequest) -> Vec<UnixNanos> {
    let mut times: Vec<UnixNanos> = request
        .legs
        .iter()
        .flat_map(|leg| request.bars.closes(&leg.candles))
        .collect();
    times.sort_unstable();
    times.dedup();
    times
}

/// A leg's bar for each moment. Where it has none it carries its last close at no volume, so that
/// cash, equity and margin always read every market at the same moment.
fn steps_of(
    leg: &PortfolioLeg,
    bars: BarSpec,
    times: &[UnixNanos],
    percent: f64,
    look_back: u32,
) -> Vec<Option<Step>> {
    let closes = bars.closes(&leg.candles);
    let costs = if percent > 0.0 {
        range_costs(
            percent,
            look_back,
            leg.candles.iter().map(|c| c.high - c.low),
        )
    } else {
        vec![0.0; leg.candles.len()]
    };
    let mut next = 0;
    let mut last: Option<f64> = None;
    times
        .iter()
        .map(|&time| {
            if closes.get(next) == Some(&time) {
                // As Nautilus has it, in the market's price steps, so sizes come out the same.
                let tidy = |price: f64| Price::new(price, leg.instrument.price_precision).as_f64();
                let raw = leg.candles[next];
                let candle = Candle {
                    open: tidy(raw.open),
                    high: tidy(raw.high),
                    low: tidy(raw.low),
                    close: tidy(raw.close),
                    ..raw
                };
                let cost = costs[next];
                next += 1;
                last = Some(candle.close);
                return Some(Step {
                    candle,
                    real: true,
                    range_cost: cost,
                });
            }
            let close = last?;
            Some(Step {
                candle: Candle {
                    ts: time,
                    open: close,
                    high: close,
                    low: close,
                    close,
                    volume: 0.0,
                },
                real: false,
                range_cost: 0.0,
            })
        })
        .collect()
}

/// Rates by when they fall due, to the nearest minute: one stamped a moment off the hour by the
/// venue must land on the same step as the rest.
fn on_the_minute(rates: &[(UnixNanos, f64)]) -> Vec<(UnixNanos, f64)> {
    rates
        .iter()
        .map(|&(when, rate)| {
            let minute = (when + NANOS_PER_MINUTE / 2) / NANOS_PER_MINUTE * NANOS_PER_MINUTE;
            (minute, rate)
        })
        .collect()
}

fn venue(
    request: &PortfolioRequest,
    fees: FeesByMarket,
    bars_by_market: HashMap<InstrumentId, Arc<BarNow>>,
) -> Result<SimulatedVenueConfig, EngineError> {
    let sim = &request.sim;
    let quote = crate::convert::currency(&request.legs[0].instrument.quote)?;
    let leverage = Decimal::try_from(sim.leverage)
        .map_err(|e| refuse(format!("leverage {}: {e}", sim.leverage)))?;
    let fill_model = CostFillModel::new(
        sim.slippage_ticks,
        sim.spread_ticks,
        sim.slippage_percent,
        sim.impact_percent,
        sim.impact_exponent,
        false,
        Arc::default(),
    )
    .by_market(bars_by_market);
    SimulatedVenueConfig::builder()
        .venue(Venue::from(VENUE))
        .oms_type(OmsType::Netting)
        .account_type(AccountType::Margin)
        .book_type(BookType::L1_MBP)
        .starting_balances(vec![Money::new(request.starting_balance, quote)])
        .default_leverage(leverage)
        .fee_model(FeeModelHandle::new(fees))
        .fill_model(FillModelHandle::new(fill_model))
        .maybe_latency_model(
            (sim.latency_ms > 0 || sim.latency_jitter_ms > 0 || sim.next_open).then(|| {
                if sim.latency_jitter_ms > 0 {
                    return LatencyModelHandle::new(JitteredLatency::new(
                        sim.latency_ms,
                        sim.latency_jitter_ms,
                        sim.latency_seed,
                    ));
                }
                let ms = if sim.next_open {
                    crate::ORDER_REACHES_OPEN_MS
                } else {
                    sim.latency_ms
                };
                let latency = DurationNanos::new(u64::from(ms) * 1_000_000);
                let none = DurationNanos::new(0);
                LatencyModelHandle::new(StaticLatencyModel::new(latency, none, none, none))
            }),
        )
        .bar_execution(!sim.next_open)
        .build()
        .map_err(|e| refuse(format!("venue config: {e}")))
}

/// The one simulated venue every leg trades on, as separate venues would mean separate accounts.
const VENUE: &str = "PORTFOLIO";

fn fee_model(
    sim: &SimSettings,
    fees: FeeRates,
    quote: nautilus_model::types::Currency,
) -> Result<FeeModelAny, EngineError> {
    let decimal = |v: f64| Decimal::try_from(v).map_err(|e| refuse(format!("fee rate {v}: {e}")));
    Ok(match sim.fee_per_order {
        Some(fee) => FeeModelAny::Fixed(FixedFeeModel::new(Money::new(fee, quote), Some(true))?),
        None => FeeModelAny::MakerTaker(MakerTakerFeeModel::new(
            decimal(fees.maker)?,
            decimal(fees.taker)?,
        )),
    })
}

fn signal_of(
    strategy: &StrategySpec,
    plugin: Option<&Arc<dyn SignalFactory>>,
) -> Result<Box<dyn Signal>, EngineError> {
    strategy.validate().map_err(EngineError::InvalidRequest)?;
    match &strategy.kind {
        StrategyKind::BuiltIn(kind) => Ok(builtins::signal(*kind, strategy)),
        StrategyKind::Plugin(id) => plugin
            .ok_or_else(|| refuse(format!("plugin `{id}` isn't loaded")))?
            .create(strategy.own_params())
            .map_err(EngineError::Strategy),
    }
}

/// Runs a portfolio on its own thread, as Nautilus needs one each. Join the handle to get the report.
#[must_use]
pub fn spawn_portfolio(
    request: PortfolioRequest,
) -> std::thread::JoinHandle<Result<PortfolioReport, EngineError>> {
    std::thread::Builder::new()
        .name("persephone-portfolio".into())
        .spawn(move || run_portfolio(&request))
        .expect("failed to spawn portfolio thread")
}

/// Runs a portfolio on the current thread and blocks until it finishes.
///
/// # Errors
///
/// Returns an error if the request is invalid or Nautilus aborts the run.
#[allow(clippy::too_many_lines)]
pub fn run_portfolio(request: &PortfolioRequest) -> Result<PortfolioReport, EngineError> {
    check(request)?;
    let times = Arc::new(timeline(request));
    let quote = crate::convert::currency(&request.legs[0].instrument.quote)?;
    let mut engine = BacktestEngine::new(crate::engine_config())?;

    let mut fees = HashMap::new();
    let mut bars_by_market = HashMap::new();
    let mut prepared = Vec::with_capacity(request.legs.len());
    for leg in &request.legs {
        let spec = InstrumentSpec {
            venue: VENUE.into(),
            ..leg.instrument.clone()
        };
        let instrument = crate::convert::instrument(&spec)?;
        let id = instrument.id();
        fees.insert(id, fee_model(&request.sim, leg.fees, quote)?);
        let bar_now = Arc::new(BarNow::default());
        bars_by_market.insert(id, Arc::clone(&bar_now));
        let steps = steps_of(
            leg,
            request.bars,
            &times,
            request.sim.range_slippage_percent,
            request.sim.range_slippage_look_back,
        );
        prepared.push((spec, instrument, id, bar_now, steps));
    }
    engine.add_venue(venue(request, FeesByMarket(fees), bars_by_market)?)?;

    let mut data = Vec::new();
    let mut legs = Vec::with_capacity(prepared.len());
    let mut expected_bars = vec![0; times.len()];
    let mut funding = Vec::with_capacity(prepared.len());
    let mut next_trade_id = 0;
    for (leg, (spec, instrument, id, bar_now, steps)) in request.legs.iter().zip(prepared) {
        engine.add_instrument(&instrument)?;
        let bar_type = crate::convert::bar_type(id, request.bars)?;
        let (mut candles, mut closes) = (Vec::new(), Vec::new());
        for (at, step) in steps.iter().enumerate() {
            if let Some(step) = step {
                candles.push(step.candle);
                closes.push(times[at]);
                expected_bars[at] += 1;
            }
        }
        data.extend(crate::convert::bars(&candles, &closes, bar_type, &spec)?);
        // Filling at the next open, each bar that really was there opens with a trade to fill on.
        if request.sim.next_open {
            let opens: Vec<Trade> = steps
                .iter()
                .flatten()
                .filter(|step| step.real)
                .map(|step| Trade {
                    ts: step.candle.ts + crate::OPEN_TRADES_AT_NANOS,
                    price: step.candle.open,
                    size: 1.0,
                    aggressor: Aggressor::Unknown,
                })
                .collect();
            let ticks = crate::convert::trade_ticks(&opens, id, &spec, &mut next_trade_id)?;
            engine.add_data_batch(DataBatch::from(ticks), None, true, true)?;
        }
        let wants = match (&leg.strategy, &request.weights) {
            (_, Some(_)) => Wants::Table,
            (Some(strategy), None) => Wants::Signal(
                signal_of(strategy, leg.plugin.as_ref())?,
                leg.weight * strategy.size_fraction() * request.sim.leverage,
            ),
            (None, None) => Wants::Hold(leg.weight),
        };
        let rates = Arc::new(on_the_minute(
            leg.funding.as_deref().map_or(&[], Vec::as_slice),
        ));
        funding.push(Arc::clone(&rates));
        legs.push(Leg {
            id,
            bar_type,
            size_precision: spec.size_precision,
            wants,
            steps: Arc::new(steps),
            // A perpetual is one given funding, even if no rate fell in its span.
            borrows: leg.funding.is_none(),
            funding: rates,
            bar_now,
            next_funding: 0,
            position: 0.0,
            entry_price: f64::NAN,
            in_flight: Vec::new(),
            share: 0.0,
            stale: false,
            goal: 0.0,
            working_bars: 0,
            last_volume: 0.0,
            range_now: 0.0,
        });
    }

    let count = legs.len();
    let outcome = Rc::new(RefCell::new(Outcome {
        fills: vec![Vec::new(); count],
        orders_worked: vec![0; count],
        work_bars: vec![0; count],
        longest_work: vec![0; count],
        works_done: vec![0; count],
        unfilled: vec![0.0; count],
        early_weights: vec![0; count],
        ..Outcome::default()
    }));
    let steps: Vec<_> = legs.iter().map(|leg| Arc::clone(&leg.steps)).collect();
    let sim = &request.sim;
    engine.add_strategy(PortfolioStrategy::new(
        legs,
        Settings {
            starting_balance: request.starting_balance,
            leverage: sim.leverage,
            compound: sim.compound,
            maintenance: request.maintenance_percent / 100.0,
            participation: sim.max_participation_percent.map(|percent| percent / 100.0),
            watch_volume: sim.impact_percent > 0.0,
            volume_warning: sim.volume_warning_percent / 100.0,
            borrow_percent_per_year: sim.borrow_percent_per_year.unwrap_or(0.0),
            interest_percent_per_year: sim.cash_interest_percent_per_year.unwrap_or(0.0),
            next_open: sim.next_open,
            rules: request.rules.clone(),
            trade_from: request.trade_from,
            rebalance: request.rebalance,
            table: request.weights.clone(),
            max_gross: sim.max_gross_percent.map(|percent| percent / 100.0),
        },
        Arc::clone(&times),
        Arc::new(expected_bars),
        Rc::clone(&outcome),
    ))?;
    engine.add_data(data, None, true, true)?;
    engine.run(None, None, None, false)?;

    let total_orders = engine.get_result().total_orders;
    engine.dispose();
    let outcome = outcome.take();
    if let Some(message) = outcome.failure.clone() {
        return Err(EngineError::Strategy(message));
    }
    Ok(build_report(
        request,
        &times,
        &steps,
        &funding,
        total_orders,
        &outcome,
    ))
}

fn build_report(
    request: &PortfolioRequest,
    times: &[UnixNanos],
    steps: &[Arc<Vec<Option<Step>>>],
    funding: &[Arc<Vec<(UnixNanos, f64)>>],
    total_orders: usize,
    outcome: &Outcome,
) -> PortfolioReport {
    let start = request.starting_balance;
    let mut equity = vec![start; times.len()];
    let mut legs = Vec::with_capacity(request.legs.len());
    let mut traded = 0.0;
    let (mut total_fees, mut total_funding, mut total_borrow) = (0.0, 0.0, 0.0);
    for (index, leg) in request.legs.iter().enumerate() {
        let fills = report::assign_bars(&outcome.fills[index], times, request.sim.next_open);
        let first_open = leg.candles[0].open;
        let full: Vec<Candle> = steps[index]
            .iter()
            .zip(times)
            .map(|(step, &time)| {
                step.map_or(
                    Candle {
                        ts: time,
                        open: first_open,
                        high: first_open,
                        low: first_open,
                        close: first_open,
                        volume: 0.0,
                    },
                    |step| step.candle,
                )
            })
            .collect();
        let borrow = if leg.funding.is_none() {
            request.sim.borrow_percent_per_year.unwrap_or(0.0)
        } else {
            0.0
        };
        let (contribution, funding_paid, borrow_paid) = report::equity_curve(
            &fills,
            &full,
            times,
            0.0,
            &Holding {
                funding: &funding[index],
                borrow_percent_per_year: borrow,
            },
        );
        for (total, part) in equity.iter_mut().zip(&contribution) {
            *total += part;
        }
        let mut position = 0.0;
        let mut next = 0;
        let values = full
            .iter()
            .enumerate()
            .map(|(at, candle)| {
                while let Some(fill) = fills.get(next).filter(|fill| fill.bar <= at) {
                    position += if fill.side == Side::Buy {
                        fill.qty
                    } else {
                        -fill.qty
                    };
                    next += 1;
                }
                position * candle.close
            })
            .collect();
        traded += fills.iter().map(|fill| fill.qty * fill.price).sum::<f64>();
        let fees = fills.iter().map(|fill| fill.fee).sum::<f64>() + 0.0;
        total_fees += fees;
        total_funding += funding_paid;
        total_borrow += borrow_paid;
        let real: Vec<bool> = steps[index]
            .iter()
            .map(|step| step.is_some_and(|step| step.real))
            .collect();
        let missing_bars = steps[index]
            .iter()
            .filter(|step| step.is_some_and(|step| !step.real))
            .count();
        legs.push(LegReport {
            symbol: leg.instrument.symbol.clone(),
            fills,
            account_line: contribution.iter().map(|made| start + made).collect(),
            contribution,
            values,
            closes: full.iter().map(|candle| candle.close).collect(),
            volumes: full.iter().map(|candle| candle.volume).collect(),
            real,
            fees,
            funding_paid,
            borrow_paid,
            orders_worked: outcome.orders_worked[index],
            bars_to_fill_average: if outcome.works_done[index] > 0 {
                outcome.work_bars[index] as f64 / outcome.works_done[index] as f64
            } else {
                0.0
            },
            bars_to_fill_longest: outcome.longest_work[index],
            unfilled_at_end: outcome.unfilled[index],
            missing_bars,
        });
    }
    // Cash below zero is a loan, charged on what it was going into each step.
    let rate = request.sim.cash_interest_percent_per_year.unwrap_or(0.0) / 100.0;
    let mut interest_paid = 0.0;
    if rate > 0.0 {
        for at in 1..times.len() {
            // A perpetual isn't bought with cash: only what's bought outright can be a loan.
            let held: f64 = legs
                .iter()
                .zip(&request.legs)
                .filter(|(_, asked)| asked.funding.is_none())
                .map(|(leg, _)| leg.values[at - 1])
                .sum();
            let cash = equity[at - 1] - held;
            if cash < 0.0 {
                let years = times[at].saturating_sub(times[at - 1]) as f64 / report::NANOS_PER_YEAR;
                interest_paid += -cash * rate * years;
            }
            equity[at] -= interest_paid;
        }
    }
    let exposure = |at: usize, of: fn(f64) -> f64| -> f64 {
        let sum: f64 = legs.iter().map(|leg: &LegReport| of(leg.values[at])).sum();
        if equity[at] > 0.0 {
            sum / equity[at]
        } else {
            0.0
        }
    };
    let gross_exposure = (0..times.len()).map(|at| exposure(at, f64::abs)).collect();
    let net_exposure = (0..times.len())
        .map(|at| exposure(at, |value| value))
        .collect();
    let mut notes = notes_of(
        request,
        &legs,
        outcome,
        equity.last().copied().unwrap_or(start),
    );
    let unreached = request.weights.as_ref().map_or(0, |table| {
        let end = times.last().copied().unwrap_or(0);
        table.iter().filter(|row| row.0 > end).count()
    });
    if unreached > 0 {
        notes.push(Note {
            text: format!(
                "{unreached} rows of the weight table are for after the last bar, so they did nothing."
            ),
            warning: true,
        });
    }
    PortfolioReport {
        times: times.to_vec(),
        max_drawdown: report::max_drawdown(&equity),
        equity,
        starting_balance: start,
        fees: total_fees,
        funding_paid: total_funding,
        borrow_paid: total_borrow,
        interest_paid,
        total_orders,
        legs,
        gross_exposure,
        net_exposure,
        turnover: if start > 0.0 { traded / start } else { 0.0 },
        peak_margin_use: outcome.peak_margin_use,
        closest_to_liquidation: outcome.closest_to_liquidation,
        liquidated_at: outcome.liquidated_at,
        orders_capped: outcome.orders_capped,
        notes,
    }
}

fn notes_of(
    request: &PortfolioRequest,
    legs: &[LegReport],
    outcome: &Outcome,
    last: f64,
) -> Vec<Note> {
    let warn = |text: String| Note {
        text,
        warning: true,
    };
    let at = |ts| {
        let at = DateTime::from_nanos(ts);
        format!("{} {}", at.date(), at.time())
    };
    let mut notes = Vec::new();
    if let Some(when) = outcome.liquidated_at {
        notes.push(warn(format!(
            "The account ran out of margin at {} and everything was liquidated in one go at that close, with no more trading after. Its worst moment is judged from each market's worst in the bar, as bars can't show what came first, so a hedged book may be cut sooner than it really would.",
            at(when)
        )));
    }
    // A little over is only fees on a position that took all the cash.
    if outcome.peak_margin_use > 1.05 {
        notes.push(warn(format!(
            "At its most the book needed {:.0}% of the account in margin at a leverage of {}, which a venue wouldn't allow: it would have refused the orders or closed positions. Lower the weights, raise the leverage setting if that much is really on offer, or turn on `max_gross_percent` to cut orders that would take the book past what the account allows.",
            outcome.peak_margin_use * 100.0,
            request.sim.leverage,
        )));
    }
    if last <= 0.0 {
        notes.push(warn(format!(
            "Account balance depleted ({last:.2} left): the losses went past what it had."
        )));
    }
    if let (count @ 1.., Some(first)) = (outcome.refused.0, outcome.refused.1.as_ref()) {
        notes.push(warn(format!(
            "{count} orders were refused, so those legs stayed as they were and these results don't follow what was asked. The first: {first}"
        )));
    }
    for (index, leg) in legs.iter().enumerate() {
        if leg.missing_bars > 0 {
            notes.push(warn(format!(
                "{} had no bar at {} moments. It was valued at its last close and couldn't trade there.",
                leg.symbol, leg.missing_bars
            )));
        }
        if outcome.early_weights[index] > 0 {
            notes.push(warn(format!(
                "{} was given a weight {} times before it had any bars. It took up its weight at its first bar, so the book wasn't what was asked for until then.",
                leg.symbol, outcome.early_weights[index]
            )));
        }
        if leg.unfilled_at_end > 0.0 {
            notes.push(warn(format!(
                "{} still had {} to go when the run ended: the volume wasn't there to reach its size.",
                leg.symbol, leg.unfilled_at_end
            )));
        }
    }
    if outcome.cut_short > 0 {
        notes.push(warn(format!(
            "{} orders were replaced before all of them had gone out. The legs didn't always reach the size asked for, as the volume wasn't there.",
            outcome.cut_short
        )));
    }
    if outcome.over_volume > 0 {
        notes.push(warn(format!(
            "{} orders were over {}% of their bar's volume. Fills like those are fiction whatever the costs say: a smaller position is the real answer.",
            outcome.over_volume, request.sim.volume_warning_percent
        )));
    }
    if outcome.orders_capped > 0
        && let Some(most) = request.sim.max_gross_percent
    {
        notes.push(Note {
            text: format!(
                "{} orders were cut to keep the book within {most}% of the account at a leverage of {}, as max_gross_percent asks.",
                outcome.orders_capped, request.sim.leverage
            ),
            warning: false,
        });
    }
    if outcome.too_small > 0 {
        notes.push(warn(format!(
            "{} times a leg's position rounded to nothing at its market's size steps. Raise the capital or the weight.",
            outcome.too_small
        )));
    }
    if request.sim.latency_jitter_ms > 0 {
        notes.push(Note {
            text: "Latency jitter changes nothing here: an order sent at a close reaches the next bar whether it took a little longer or not. Only fills on ticks can show it, and portfolios fill on bars.".into(),
            warning: false,
        });
    }
    notes
}
