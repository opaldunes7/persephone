//! The bridge from "which position do I want?" to Nautilus orders.

use std::{cell::RefCell, fmt, rc::Rc, sync::Arc};

use nautilus_common::actor::DataActor;
use nautilus_model::{
    data::{Bar, BarType},
    enums::{LiquiditySide, OrderSide},
    events::{OrderDenied, OrderFilled, OrderRejected},
    identifiers::{ClientOrderId, InstrumentId, StrategyId},
    instruments::Instrument,
    orders::{Order as _, OrderAny},
    types::{Price, Quantity},
};
use nautilus_trading::{
    nautilus_strategy,
    strategy::{Strategy, StrategyConfig, StrategyCore},
};
use persephone_core::{Candle, Side, UnixNanos, signal::Signal};

use crate::{
    models::BarNow,
    report::{LimitOutcome, NANOS_PER_YEAR, RawFill, RawLimit},
    settings::{LimitOrders, TradingRules},
};

/// Hears what a live strategy sees and does, as it happens.
pub(crate) trait Watcher {
    /// Connected and subscribed; bars come next.
    fn started(&mut self);
    /// A bar closed and the signal has seen it.
    fn bar(&mut self, bar: &Candle);
    fn fill(&mut self, fill: &RawFill);
    /// An order was refused, or the signal failed (the strategy has stopped trading then).
    fn problem(&mut self, message: String);
}

/// Runs a [`Signal`] inside Nautilus. Each position is worth `notional` in the quote currency
/// at entry; reversing long to short (or back) is one order.
pub(crate) struct SignalStrategy {
    core: StrategyCore,
    bar_type: BarType,
    /// Shared with the engine thread, which asks it for notes after the run.
    signal: Rc<RefCell<Box<dyn Signal>>>,
    notional: f64,
    size_precision: u8,
    /// Signed position in base units, kept from fills.
    position: f64,
    /// What the position now held was entered at.
    entry_price: f64,
    /// Orders sent and not filled yet, with how much of each is to come, signed. With latency one
    /// can still be on its way at the next bar, so the next is worked out from where it will leave
    /// things: from what's held alone, it would double the position.
    in_flight: Vec<(ClientOrderId, f64)>,
    /// What the account started with, when positions are sized from what it holds now.
    compound_from: Option<f64>,
    /// Cash, kept from fills, starting from nothing: the account is this plus what it started with
    /// and the position at its price.
    cash: f64,
    /// Orders refused, and why the first was, for the report.
    refused: Rc<RefCell<(usize, Option<String>)>>,
    /// The position last asked for, after the trading rules, and how big its order was.
    asked: i8,
    asked_size: f64,
    /// The last order was refused, which is reported already and explains not holding what's asked.
    last_refused: bool,
    /// Where what's held didn't match what was asked for, for the report.
    missed: Rc<RefCell<Missed>>,
    price_precision: u8,
    /// What orders resting further off than they offered pay when they fill.
    quoted: Vec<(ClientOrderId, f64)>,
    /// How orders go out as limits, and stops and targets while a position's held.
    limit: Option<LimitOrders>,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    /// The limit order resting now, if any.
    resting: Option<Resting>,
    /// The stop and target resting now.
    brackets: Brackets,
    /// Each bar's close, open, high, low and real volume (what's handed to Nautilus is deliberately
    /// more, so it doesn't cap fills), only to tell when one reaches both a stop and a target. Which
    /// came first can't be known from a bar, so the target stands down for it and the stop goes first.
    /// Its open tells the fill model where a stop passed on the way should fill.
    bars_ahead: Arc<Vec<BarAhead>>,
    /// Orders fill on finer data, which shows what came first, so nothing needs assuming.
    fills_finer: bool,
    bar_now: Arc<BarNow>,
    /// The last close seen, for what's done when a fill comes in.
    last_candle: Option<Candle>,
    /// What holding costs, charged to `cash`, so positions sized from the account feel it.
    funding: Arc<Vec<(UnixNanos, f64)>>,
    next_funding: usize,
    borrow_percent_per_year: f64,
    last_bar: Option<UnixNanos>,
    /// Impact's on, so orders large against their bar's volume are worth saying.
    watch_volume: bool,
    /// Shared with the engine thread that runs this strategy; collected after the run.
    fills: Rc<RefCell<Vec<RawFill>>>,
    /// Set when the signal fails; the strategy stops trading and the run reports it.
    failure: Rc<RefCell<Option<String>>>,
    /// Flat instead of short, for spot accounts that can't borrow.
    long_only: bool,
    rules: TradingRules,
    /// Bars closing up to here only warm the signal up.
    trade_from: UnixNanos,
    watcher: Option<Box<dyn Watcher>>,
}

impl SignalStrategy {
    pub(crate) fn new(
        bar_type: BarType,
        signal: Rc<RefCell<Box<dyn Signal>>>,
        notional: f64,
        size_precision: u8,
        fills: Rc<RefCell<Vec<RawFill>>>,
        failure: Rc<RefCell<Option<String>>>,
    ) -> Self {
        Self {
            core: StrategyCore::new(config(false)),
            bar_type,
            signal,
            notional,
            size_precision,
            position: 0.0,
            entry_price: f64::NAN,
            in_flight: Vec::new(),
            compound_from: None,
            cash: 0.0,
            refused: Rc::default(),
            asked: 0,
            asked_size: 0.0,
            last_refused: false,
            missed: Rc::default(),
            price_precision: 8,
            quoted: Vec::new(),
            limit: None,
            stop_loss: None,
            take_profit: None,
            resting: None,
            brackets: Brackets::default(),
            bars_ahead: Arc::default(),
            fills_finer: false,
            bar_now: Arc::default(),
            last_candle: None,
            funding: Arc::default(),
            next_funding: 0,
            borrow_percent_per_year: 0.0,
            last_bar: None,
            watch_volume: false,
            fills,
            failure,
            long_only: false,
            rules: TradingRules::default(),
            trade_from: 0,
            watcher: None,
        }
    }

    /// Trades by `rules`, and only on bars closing after `trade_from`; earlier ones warm the
    /// signal up.
    /// Counts refused orders in `refused`.
    pub(crate) fn counting_refusals(
        mut self,
        refused: Rc<RefCell<(usize, Option<String>)>>,
    ) -> Self {
        self.refused = refused;
        self
    }

    /// Sizes positions from the account as it is now, it having started with `balance`.
    pub(crate) const fn compounding(mut self, balance: Option<f64>) -> Self {
        self.compound_from = balance;
        self
    }

    /// Sends orders as limits, and keeps stops and targets on what's held, as these say.
    pub(crate) fn with_orders(
        mut self,
        limit: Option<LimitOrders>,
        stop_loss: Option<f64>,
        take_profit: Option<f64>,
        bars_ahead: Arc<Vec<BarAhead>>,
        fills_finer: bool,
        bar_now: Arc<BarNow>,
    ) -> Self {
        self.limit = limit;
        self.stop_loss = stop_loss;
        self.take_profit = take_profit;
        self.bars_ahead = bars_ahead;
        self.fills_finer = fills_finer;
        self.bar_now = bar_now;
        self
    }

    /// Charges what holding costs, and says when orders are large against their bars.
    pub(crate) fn holding(
        mut self,
        funding: Arc<Vec<(UnixNanos, f64)>>,
        borrow_percent_per_year: Option<f64>,
        impact_percent: f64,
    ) -> Self {
        self.funding = funding;
        self.borrow_percent_per_year = borrow_percent_per_year.unwrap_or(0.0);
        self.watch_volume = impact_percent > 0.0;
        self
    }

    /// Takes what fell due over this bar from the cash, as the report's equity does.
    fn charge_holding(&mut self, candle: &Candle) {
        while let Some(&(when, rate)) = self.funding.get(self.next_funding) {
            if when >= candle.ts {
                break;
            }
            self.cash -= self.position * candle.open * rate;
            self.next_funding += 1;
        }
        if self.position < 0.0
            && let Some(last) = self.last_bar
        {
            let years = candle.ts.saturating_sub(last) as f64 / NANOS_PER_YEAR;
            self.cash +=
                self.position.abs() * -candle.open * self.borrow_percent_per_year / 100.0 * years;
        }
        self.last_bar = Some(candle.ts);
    }

    /// Keeps count of bars that didn't get what the strategy asked for in `missed`.
    pub(crate) fn counting_misses(mut self, missed: Rc<RefCell<Missed>>) -> Self {
        self.missed = missed;
        self
    }

    /// What a position is worth now: as set, or grown or shrunk with the account.
    fn notional_at(&self, close: f64) -> f64 {
        match self.compound_from {
            Some(balance) if balance > 0.0 => {
                let account = balance + self.cash + self.position * close;
                self.notional * (account / balance).max(0.0)
            }
            _ => self.notional,
        }
    }

    pub(crate) fn ruled(mut self, rules: TradingRules, trade_from: UnixNanos) -> Self {
        self.rules = rules;
        self.trade_from = trade_from;
        self
    }

    /// For live trading: reports to `watcher` as things happen, goes flat instead of short
    /// when `long_only`, and closes its position when stopped.
    #[cfg(feature = "live")]
    pub(crate) fn watched(mut self, watcher: Box<dyn Watcher>, long_only: bool) -> Self {
        self.core = StrategyCore::new(config(true));
        self.watcher = Some(watcher);
        self.long_only = long_only;
        self
    }

    /// An order won't fill: says so, and lets the next go.
    fn refuse(&mut self, order: ClientOrderId, message: String) {
        self.in_flight.retain(|(id, _)| *id != order);
        self.last_refused = true;
        if let Some(resting) = self.resting.filter(|resting| resting.id == order) {
            self.resting = None;
            self.log_end(resting.logged, LimitOutcome::Refused);
        }
        let mut refused = self.refused.borrow_mut();
        refused.0 += 1;
        refused.1.get_or_insert_with(|| message.clone());
        drop(refused);
        self.report(message);
    }

    fn report(&mut self, message: String) {
        if let Some(watcher) = &mut self.watcher {
            watcher.problem(message);
        }
    }

    /// Which way the position is: `1`, `-1` or `0`.
    fn held(&self) -> i8 {
        sign_of(self.position)
    }

    /// `value` in the market's own steps: floating-point sums of sizes don't always come back to
    /// exactly zero, and a dust of a short would read as being short.
    fn in_steps(&self, value: f64) -> f64 {
        let steps = 10_f64.powi(i32::from(self.size_precision));
        (value * steps).round() / steps
    }

    /// The position once everything on its way has filled.
    fn expected(&self) -> f64 {
        let coming: f64 = self.in_flight.iter().map(|(_, left)| left).sum();
        self.in_steps(self.position + coming)
    }
}

/// A bar's close time, open, high, low and real volume.
pub(crate) type BarAhead = (UnixNanos, f64, f64, f64, f64);

/// A limit order resting on the book.
#[derive(Clone, Copy, Debug)]
struct Resting {
    id: ClientOrderId,
    /// The position it's to bring about.
    target: i8,
    /// How many bars it's been there.
    bars: u32,
    /// It closes or reverses what's held, so can't just lapse.
    closes_position: bool,
    /// Its place in the log.
    logged: usize,
}

/// The stop and target on what's held.
#[derive(Clone, Copy, Debug, Default)]
struct Brackets {
    stop: Option<ClientOrderId>,
    take: Option<ClientOrderId>,
}

/// Where a run didn't do what its strategy asked, with nothing else having said so.
#[derive(Debug, Default)]
pub(crate) struct Missed {
    /// Bars where what's held wasn't what was asked for, and the first of them.
    pub(crate) unmet: usize,
    pub(crate) first_unmet: UnixNanos,
    /// Bars where the position asked for rounded to nothing, and the first of them.
    pub(crate) too_small: usize,
    pub(crate) first_too_small: UnixNanos,
    /// Orders over a tenth of their bar's volume, and the first of them.
    pub(crate) over_volume: usize,
    pub(crate) first_over_volume: UnixNanos,
    /// Every limit order, as they went out.
    pub(crate) limits: Vec<RawLimit>,
    /// How many limit orders went out, and filled.
    pub(crate) limit_orders: usize,
    pub(crate) limits_filled: usize,
}

impl Missed {
    fn unmet(&mut self, ts: UnixNanos) {
        if self.unmet == 0 {
            self.first_unmet = ts;
        }
        self.unmet += 1;
    }

    fn over_volume(&mut self, ts: UnixNanos) {
        if self.over_volume == 0 {
            self.first_over_volume = ts;
        }
        self.over_volume += 1;
    }

    fn too_small(&mut self, ts: UnixNanos) {
        if self.too_small == 0 {
            self.first_too_small = ts;
        }
        self.too_small += 1;
    }
}

fn sign_of(position: f64) -> i8 {
    if position > 0.0 {
        1
    } else if position < 0.0 {
        -1
    } else {
        0
    }
}

/// Whether `position` is what was asked for: flat to within the market's steps, or on that side and
/// at least half the size of the order sent. Rounding leaves it close, so anything further off isn't
/// rounding.
fn meets(asked: i8, asked_size: f64, position: f64, size_precision: u8) -> bool {
    if asked == 0 {
        position.abs() < 0.5 / 10_f64.powi(i32::from(size_precision))
    } else {
        position * f64::from(asked) >= asked_size / 2.0
    }
}

fn config(exit_on_stop: bool) -> StrategyConfig {
    StrategyConfig {
        strategy_id: Some(StrategyId::from("PERSEPHONE-001")),
        order_id_tag: Some("001".to_string()),
        log_events: false,
        manage_stop: exit_on_stop,
        ..Default::default()
    }
}

nautilus_strategy!(SignalStrategy, {
    fn on_order_filled(&mut self, event: &OrderFilled) {
        let qty = event.last_qty.as_f64();
        let side = match event.order_side {
            OrderSide::Sell => Side::Sell,
            _ => Side::Buy,
        };
        let before = self.position;
        let signed = if side == Side::Buy { qty } else { -qty };
        self.position = self.in_steps(self.position + signed);
        if let Some(at) = self
            .in_flight
            .iter()
            .position(|(id, _)| *id == event.client_order_id)
        {
            self.in_flight[at].1 -= signed;
            if self.in_steps(self.in_flight[at].1) == 0.0 {
                self.in_flight.remove(at);
            }
        }
        // An order resting further off than it offered pays what it offered, and its fee on that.
        let offered = self
            .quoted
            .iter()
            .position(|(id, _)| *id == event.client_order_id)
            .map(|at| self.quoted.swap_remove(at).1);
        let price = offered.unwrap_or_else(|| event.last_px.as_f64());
        let fee = event.commission.map_or(0.0, |fee| fee.as_f64()) * price / event.last_px.as_f64();
        let value = qty * price;
        self.cash += if side == Side::Buy { -value } else { value };
        self.cash -= fee;
        if self.position == 0.0 {
            self.entry_price = f64::NAN;
        } else if before == 0.0 || before.is_sign_positive() != self.position.is_sign_positive() {
            self.entry_price = price;
        }
        self.after_fill(event.client_order_id, event.ts_event.as_u64());
        let fill = RawFill {
            ts: event.ts_event.as_u64(),
            side,
            price,
            qty,
            fee,
            maker: event.liquidity_side == LiquiditySide::Maker,
        };
        if let Some(watcher) = &mut self.watcher {
            watcher.fill(&fill);
        }
        self.fills.borrow_mut().push(fill);
    }

    fn on_order_rejected(&mut self, event: OrderRejected) {
        self.refuse(
            event.client_order_id,
            format!("order rejected: {}", event.reason),
        );
    }

    fn on_order_denied(&mut self, event: OrderDenied) {
        self.refuse(
            event.client_order_id,
            format!("order denied: {}", event.reason),
        );
    }
});

impl fmt::Debug for SignalStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignalStrategy")
            .field("bar_type", &self.bar_type)
            .field("notional", &self.notional)
            .field("position", &self.position)
            .finish()
    }
}

impl DataActor for SignalStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        // Live, the venue's own instrument may trade in finer or coarser steps than ours.
        if let Some(instrument) = self.cache().instrument(&self.bar_type.instrument_id()) {
            self.size_precision = instrument.size_precision();
            self.price_precision = instrument.price_precision();
        }
        self.subscribe_bars(self.bar_type, None, None);
        if let Some(watcher) = &mut self.watcher {
            watcher.started();
        }
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.unsubscribe_bars(self.bar_type, None, None);
        Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        let candle = Candle {
            ts: bar.ts_event.as_u64(),
            open: bar.open.as_f64(),
            high: bar.high.as_f64(),
            low: bar.low.as_f64(),
            close: bar.close.as_f64(),
            volume: bar.volume.as_f64(),
        };
        if self.failure.borrow().is_some() {
            return Ok(());
        }
        let wanted = {
            let mut signal = self.signal.borrow_mut();
            signal.holding(self.held(), self.entry_price);
            signal.on_bar(&candle)
        };
        if let Some(watcher) = &mut self.watcher {
            watcher.bar(&candle);
        }
        let wanted = match wanted {
            Ok(wanted) => wanted,
            Err(message) => {
                self.report(format!("the strategy failed: {message}"));
                *self.failure.borrow_mut() = Some(message);
                return Ok(());
            }
        };
        let next = self.bars_ahead.partition_point(|bar| bar.0 <= candle.ts);
        if let Some(bar) = self.bars_ahead.get(next).filter(|_| !self.fills_finer) {
            self.bar_now.set_open(bar.1);
        }
        let volume = next
            .checked_sub(1)
            .and_then(|at| self.bars_ahead.get(at))
            .map_or(0.0, |bar| bar.4);
        self.bar_now.set_volume(volume);
        self.last_candle = Some(candle);
        self.charge_holding(&candle);
        // Bars are stamped with their close: the moment an order would go out.
        if candle.ts <= self.trade_from {
            return Ok(());
        }
        self.decide(&candle, wanted)?;
        self.keep_brackets(&candle)
    }
}

impl SignalStrategy {
    /// Acts on what the strategy wants at this close.
    fn decide(&mut self, candle: &Candle, wanted: Option<i8>) -> anyhow::Result<()> {
        // A check on everything below, so it mustn't lean on any of it: what's asked for by now
        // should be held, unless an order is still on its way or was refused.
        if self.in_flight.is_empty()
            && !self.last_refused
            && !meets(
                self.asked,
                self.asked_size,
                self.position,
                self.size_precision,
            )
        {
            self.missed.borrow_mut().unmet(candle.ts);
        }
        let lapsed = self.age_resting()?;
        let expected = self.expected();
        let target = self.rules.target(wanted, sign_of(expected), candle.ts);
        // An exit that didn't fill as a limit still has to happen.
        let at_market =
            target.is_none() && lapsed.is_some() || target == lapsed && lapsed.is_some();
        let Some(target) = target.or(lapsed) else {
            return Ok(());
        };
        let target = if self.long_only {
            target.max(0)
        } else {
            target
        };
        // Stopping a live session closes the position; nothing new meanwhile.
        if candle.close <= 0.0 || self.is_exiting() {
            return Ok(());
        }
        // A newer wish replaces what's resting; the same one leaves it be.
        if let Some(resting) = self.resting {
            if resting.target == target {
                return Ok(());
            }
            self.end_resting(LimitOutcome::Replaced)?;
        }
        let expected = self.expected();
        if sign_of(expected) == target {
            self.asked = target;
            return Ok(());
        }

        let size = Quantity::new(
            self.notional_at(candle.close) / candle.close,
            self.size_precision,
        )
        .as_f64();
        let delta = f64::from(target) * size - expected;
        let qty = Quantity::new(delta.abs(), self.size_precision);
        if qty.is_zero() {
            if size == 0.0 {
                self.missed.borrow_mut().too_small(candle.ts);
            }
            return Ok(());
        }
        let volume = self
            .bars_ahead
            .get(
                self.bars_ahead
                    .partition_point(|bar| bar.0 <= candle.ts)
                    .wrapping_sub(1),
            )
            .map_or(0.0, |bar| bar.4);
        if self.watch_volume && volume > 0.0 && qty.as_f64() > volume * 0.1 {
            self.missed.borrow_mut().over_volume(candle.ts);
        }
        self.asked = target;
        self.asked_size = size;
        self.last_refused = false;
        let side = if delta > 0.0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };
        let instrument_id = self.bar_type.instrument_id();
        let limit = self.limit.filter(|_| !at_market);
        let order = match limit {
            None => {
                // What's held is going, so what guards it goes first.
                self.cancel_brackets()?;
                self.order().market(
                    instrument_id,
                    side,
                    qty,
                    None, // time_in_force
                    None, // reduce_only
                    None, // quote_quantity
                    None, // exec_algorithm_id
                    None, // exec_algorithm_params
                    None, // tags
                    None, // client_order_id
                )
            }
            Some(limit) => {
                let share = limit.offset_percent / 100.0;
                let sign = if delta > 0.0 { 1.0 } else { -1.0 };
                let quoted = candle.close * (1.0 - sign * share);
                // It rests further off, so the market has to trade that far past before it
                // fills, but pays what it offered.
                let past = limit.through_percent / 100.0;
                let resting_at = Price::new(quoted * (1.0 - sign * past), self.price_precision);
                let quoted = Price::new(quoted, self.price_precision);
                let order = self.limit_order(instrument_id, side, qty, resting_at, true);
                self.quoted.push((order.client_order_id(), quoted.as_f64()));
                let mut missed = self.missed.borrow_mut();
                missed.limits.push(RawLimit {
                    placed: candle.ts,
                    side: if delta > 0.0 { Side::Buy } else { Side::Sell },
                    price: quoted.as_f64(),
                    qty: qty.as_f64(),
                    outcome: LimitOutcome::Open,
                    ended: candle.ts,
                });
                let logged = missed.limits.len() - 1;
                missed.limit_orders += 1;
                drop(missed);
                self.resting = Some(Resting {
                    id: order.client_order_id(),
                    target,
                    bars: 0,
                    closes_position: sign_of(self.position) != 0,
                    logged,
                });
                order
            }
        };
        // Before it goes, as a refusal can come back straight away.
        self.in_flight
            .push((order.client_order_id(), delta.signum() * qty.as_f64()));
        self.submit_order(order, None, None, None)
    }

    fn limit_order(
        &mut self,
        instrument_id: InstrumentId,
        side: OrderSide,
        qty: Quantity,
        price: Price,
        rests: bool,
    ) -> OrderAny {
        self.order().limit(
            instrument_id,
            side,
            qty,
            price,
            None,         // time_in_force
            None,         // expire_time
            Some(rests),  // post_only: an entry's offset means it rests
            Some(!rests), // reduce_only: a target only ever closes
            None,         // quote_quantity
            None,         // display_qty
            None,         // emulation_trigger
            None,         // trigger_instrument_id
            None,         // exec_algorithm_id
            None,         // exec_algorithm_params
            None,         // tags
            None,         // client_order_id
        )
    }

    /// One more bar for what's resting. If its time's up it goes, and an exit's target is returned,
    /// as that has to go ahead at the market. An entry just lapses.
    fn age_resting(&mut self) -> anyhow::Result<Option<i8>> {
        let (Some(limit), Some(resting)) = (self.limit, self.resting.as_mut()) else {
            return Ok(None);
        };
        resting.bars += 1;
        if resting.bars < limit.expires_bars {
            return Ok(None);
        }
        let resting = *resting;
        self.end_resting(LimitOutcome::Lapsed)?;
        self.asked = sign_of(self.position);
        Ok(resting.closes_position.then_some(resting.target))
    }

    fn end_resting(&mut self, outcome: LimitOutcome) -> anyhow::Result<()> {
        if let Some(resting) = self.resting.take() {
            self.in_flight.retain(|(id, _)| *id != resting.id);
            self.log_end(resting.logged, outcome);
            self.cancel_order(resting.id, None, None)?;
        }
        Ok(())
    }

    fn log_end(&mut self, logged: usize, outcome: LimitOutcome) {
        let ended = self.last_candle.map_or(0, |candle| candle.ts);
        if let Some(order) = self.missed.borrow_mut().limits.get_mut(logged) {
            order.outcome = outcome;
            order.ended = ended;
        }
    }

    fn cancel_brackets(&mut self) -> anyhow::Result<()> {
        for id in [self.brackets.stop.take(), self.brackets.take.take()]
            .into_iter()
            .flatten()
        {
            self.cancel_order(id, None, None)?;
        }
        self.brackets = Brackets::default();
        Ok(())
    }

    /// What follows a fill: what it replaces, or what's now out of date, goes.
    fn after_fill(&mut self, order: ClientOrderId, filled_at: UnixNanos) {
        let bracket = self.brackets.stop == Some(order) || self.brackets.take == Some(order);
        if bracket {
            // The position's gone, and with it what guarded it and any exit still waiting.
            self.asked = 0;
            let _ = self.cancel_brackets_except(order);
            let _ = self.end_resting(LimitOutcome::Cancelled);
            return;
        }
        if self.resting.is_some_and(|resting| resting.id == order) {
            if let Some(resting) = self.resting.take() {
                self.log_end(resting.logged, LimitOutcome::Filled);
                if let Some(order) = self.missed.borrow_mut().limits.get_mut(resting.logged) {
                    order.ended = filled_at;
                }
            }
            self.missed.borrow_mut().limits_filled += 1;
            let _ = self.cancel_brackets();
            return;
        }
        // A market order fills at the close it went out on, so its guards rest through the next bar.
        // A limit's fills partway through one, so they start at that bar's close.
        if let Some(candle) = self.last_candle {
            let _ = self.keep_brackets(&candle);
        }
    }

    fn cancel_brackets_except(&mut self, filled: ClientOrderId) -> anyhow::Result<()> {
        if self.brackets.stop == Some(filled) {
            self.brackets.stop = None;
        }
        if self.brackets.take == Some(filled) {
            self.brackets.take = None;
        }
        self.cancel_brackets()
    }

    /// Keeps a stop and a target on what's held, from the bar after it's entered.
    fn keep_brackets(&mut self, candle: &Candle) -> anyhow::Result<()> {
        if self.stop_loss.is_none() && self.take_profit.is_none() {
            return Ok(());
        }
        let held = self.held();
        if held == 0
            || !self
                .in_flight
                .iter()
                .all(|order| self.resting.is_some_and(|r| r.id == order.0))
            || !self.entry_price.is_finite()
            || self.is_exiting()
        {
            return Ok(());
        }
        let side = f64::from(held);
        let instrument_id = self.bar_type.instrument_id();
        let qty = Quantity::new(self.position.abs(), self.size_precision);
        let closing = if held > 0 {
            OrderSide::Sell
        } else {
            OrderSide::Buy
        };
        let stop_at = self
            .stop_loss
            .map(|percent| self.entry_price * (1.0 - side * percent / 100.0));
        let take_at = self
            .take_profit
            .map(|percent| self.entry_price * (1.0 + side * percent / 100.0));
        // Already past the stop at this close, there's nothing to rest: out now.
        if self.brackets.stop.is_none()
            && stop_at.is_some_and(|at| (candle.close - at) * side <= 0.0)
        {
            self.cancel_brackets()?;
            self.end_resting(LimitOutcome::Cancelled)?;
            self.asked = 0;
            let order = self.order().market(
                instrument_id,
                closing,
                qty,
                None,       // time_in_force
                Some(true), // reduce_only
                None,       // quote_quantity
                None,       // exec_algorithm_id
                None,       // exec_algorithm_params
                None,       // tags
                None,       // client_order_id
            );
            self.in_flight
                .push((order.client_order_id(), -side * qty.as_f64()));
            return self.submit_order(order, None, None, None);
        }
        if let (Some(at), None) = (stop_at, self.brackets.stop) {
            let order = self.order().stop_market(
                instrument_id,
                closing,
                qty,
                Price::new(at, self.price_precision),
                None,       // trigger_type
                None,       // time_in_force
                None,       // expire_time
                Some(true), // reduce_only
                None,       // quote_quantity
                None,       // display_qty
                None,       // emulation_trigger
                None,       // trigger_instrument_id
                None,       // exec_algorithm_id
                None,       // exec_algorithm_params
                None,       // tags
                None,       // client_order_id
            );
            self.brackets.stop = Some(order.client_order_id());
            self.submit_order(order, None, None, None)?;
        }
        let Some(take) = take_at else {
            return Ok(());
        };
        // On a bar reaching both, it's unknowable which came first, so assume the worse.
        let next = self.bars_ahead.partition_point(|bar| bar.0 <= candle.ts);
        let both = stop_at.is_some_and(|stop| {
            self.bars_ahead
                .get(next)
                .filter(|_| !self.fills_finer)
                .is_some_and(|&(_, _, high, low, _)| {
                    let reaches = |level: f64| low <= level && level <= high;
                    reaches(stop) && reaches(take)
                })
        });
        match (both, self.brackets.take) {
            (true, Some(id)) => {
                self.brackets.take = None;
                self.cancel_order(id, None, None)?;
            }
            (false, None) => {
                let past = self
                    .limit
                    .map_or(0.0, |limit| limit.through_percent / 100.0);
                let price = Price::new(take * (1.0 + side * past), self.price_precision);
                let order = self.limit_order(instrument_id, closing, qty, price, false);
                self.quoted.push((order.client_order_id(), take));
                self.brackets.take = Some(order.client_order_id());
                self.submit_order(order, None, None, None)?;
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holding_what_was_asked_for_means_the_size_and_not_just_the_sign() {
        assert!(meets(0, 1.0, 0.0, 5));
        assert!(meets(1, 1.0, 1.0, 5));
        assert!(meets(-1, 1.0, -1.0, 5));
        // Compounding and rounding leave it near, which is fine.
        assert!(meets(1, 1.0, 0.99, 5));
        // A dust of a short is on the right side, but isn't a short: this once lost 30% of orders.
        assert!(!meets(-1, 1.0, -4.44e-16, 5));
        assert!(!meets(1, 1.0, -1.0, 5));
        assert!(!meets(1, 1.0, 0.0, 5));
        assert!(!meets(0, 1.0, 1.0, 5));
        assert!(!meets(0, 1.0, 0.01, 2));
        assert!(meets(0, 1.0, 0.001, 2), "under half a step counts as flat");
    }
}
