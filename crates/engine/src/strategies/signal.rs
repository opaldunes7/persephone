//! The bridge from "which position do I want?" to Nautilus orders.

use std::{cell::RefCell, collections::HashMap, fmt, rc::Rc, sync::Arc};

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
    report::{Decision, LimitOutcome, NANOS_PER_YEAR, RawFill, RawLimit},
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
    quoted: HashMap<ClientOrderId, f64>,
    /// How orders go out as limits, and stops and targets while a position's held.
    limit: Option<LimitOrders>,
    stop_loss: Option<f64>,
    take_profit: Option<f64>,
    /// A stop this far under the best price since entry, as a percentage, and that price.
    trailing: Option<f64>,
    best_since_entry: f64,
    /// Keeping a row for each bar, and what's known of this one so far.
    log_decisions: bool,
    /// Go flat on the last bar a fill can happen.
    close_at_end: bool,
    /// Orders fill at the next bar's open, so the last decision that can fill is a bar earlier.
    next_open: bool,
    now: Decision,
    /// The limit order resting now, if any.
    resting: Option<Resting>,
    /// The stop and target resting now.
    brackets: Brackets,
    /// The most of a bar's volume an order can take, as a share.
    participation: Option<f64>,
    /// An order too big for one bar, with the rest still to go out.
    working: Option<Working>,
    /// What takers pay on top on each bar, in price, and for the bar closed and the bar ahead now.
    range_costs: Arc<Vec<f64>>,
    range_now: (f64, f64),
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
    /// The share of a bar's volume past which an order is worth saying.
    volume_warning: f64,
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
            quoted: HashMap::new(),
            limit: None,
            stop_loss: None,
            take_profit: None,
            trailing: None,
            best_since_entry: f64::NAN,
            log_decisions: false,
            close_at_end: false,
            next_open: false,
            now: Decision {
                ts: 0,
                wanted: None,
                held: 0,
                target: None,
                order: 0.0,
                why: "",
            },
            resting: None,
            brackets: Brackets::default(),
            participation: None,
            working: None,
            range_costs: Arc::default(),
            range_now: (0.0, 0.0),
            bars_ahead: Arc::default(),
            fills_finer: false,
            bar_now: Arc::default(),
            last_candle: None,
            funding: Arc::default(),
            next_funding: 0,
            borrow_percent_per_year: 0.0,
            last_bar: None,
            watch_volume: false,
            volume_warning: 0.1,
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
        volume_warning_percent: f64,
    ) -> Self {
        self.funding = funding;
        self.borrow_percent_per_year = borrow_percent_per_year.unwrap_or(0.0);
        self.watch_volume = impact_percent > 0.0;
        self.volume_warning = volume_warning_percent / 100.0;
        self
    }

    /// Keeps a stop this far from the best price since entry, as a percentage.
    pub(crate) fn trailing(mut self, percent: Option<f64>) -> Self {
        self.trailing = percent;
        self
    }

    /// Keeps a row for each bar.
    pub(crate) fn logging_decisions(mut self, on: bool) -> Self {
        self.log_decisions = on;
        self
    }

    /// Goes flat on the last bar a fill can happen, so nothing's left to value at the last close.
    pub(crate) fn closing_at_end(mut self, on: bool, next_open: bool) -> Self {
        self.close_at_end = on;
        self.next_open = next_open;
        self
    }

    /// What takers pay on top on each bar.
    pub(crate) fn slipping(mut self, range_costs: Arc<Vec<f64>>) -> Self {
        self.range_costs = range_costs;
        self
    }

    /// Works orders over several bars rather than take more than this percentage of one's volume.
    pub(crate) fn participating(mut self, percent: Option<f64>) -> Self {
        self.participation = percent.map(|percent| percent / 100.0);
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
        if self.in_flight.iter().any(|(id, _)| *id == order) {
            self.working = None;
        }
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

/// The stop and target on what's held, and their sizes.
#[derive(Clone, Copy, Debug, Default)]
struct Brackets {
    stop: Option<ClientOrderId>,
    stop_qty: f64,
    stop_level: f64,
    /// Whether the resting stop is the trailing one, as it was nearer than the fixed one.
    stop_trails: bool,
    take: Option<ClientOrderId>,
    take_qty: f64,
}

/// An order going out over several bars.
#[derive(Clone, Copy, Debug)]
struct Working {
    /// The way it's going, and the signed position it's to end at.
    target: i8,
    goal: f64,
    /// How many bars it's been going.
    bars: usize,
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
    /// Orders worked over several bars, how many of those finished and how many bars they took, the
    /// longest, how many were replaced first, and what was left of the last when it was last looked at.
    pub(crate) orders_worked: usize,
    pub(crate) works_done: usize,
    pub(crate) work_bars: usize,
    pub(crate) longest_work: usize,
    pub(crate) cut_short: usize,
    pub(crate) unfilled: f64,
    /// A row for each bar, if asked for.
    pub(crate) decisions: Vec<Decision>,
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
        if let Some(logged) = self
            .resting
            .filter(|resting| resting.id == event.client_order_id)
            .map(|resting| resting.logged)
            && let Some(order) = self.missed.borrow_mut().limits.get_mut(logged)
        {
            order.filled += qty;
        }
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
        // It stays listed, as the rest of a partly filled order pays the same.
        let offered = self.quoted.get(&event.client_order_id).copied();
        let price = offered.unwrap_or_else(|| event.last_px.as_f64());
        let fee = event.commission.map_or(0.0, |fee| fee.as_f64()) * price / event.last_px.as_f64();
        let value = qty * price;
        self.cash += if side == Side::Buy { -value } else { value };
        self.cash -= fee;
        if self.position == 0.0 {
            self.entry_price = f64::NAN;
        } else if before == 0.0 || before.is_sign_positive() != self.position.is_sign_positive() {
            self.entry_price = price;
            self.best_since_entry = price;
        } else if self.position.abs() > before.abs() {
            // Added to, so it's what it all cost on average.
            self.entry_price =
                (before.abs() * self.entry_price + qty * price) / self.position.abs();
        }
        let maker = event.liquidity_side == LiquiditySide::Maker;
        let range_slip = if maker {
            0.0
        } else if self.brackets.stop == Some(event.client_order_id) {
            self.range_now.1
        } else {
            self.range_now.0
        };
        let signed_qty = if side == Side::Buy { qty } else { -qty };
        self.after_fill(event.client_order_id, event.ts_event.as_u64(), signed_qty);
        let fill = RawFill {
            ts: event.ts_event.as_u64(),
            side,
            price,
            qty,
            fee,
            maker,
            range_slip,
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
        // What's handed to Nautilus has far more volume than was traded, so it doesn't cap fills. A
        // strategy should hear what was. Live, there's nothing ahead, and the bar's own is real.
        let candle = Candle {
            volume: self
                .bars_ahead
                .binary_search_by_key(&candle.ts, |bar| bar.0)
                .map_or(candle.volume, |at| self.bars_ahead[at].4),
            ..candle
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
        // The last bar a fill can happen: this close, or the next open filling at the last bar's.
        let bars_left = self.bars_ahead.len().saturating_sub(next);
        let closing = self.close_at_end
            && !self.bars_ahead.is_empty()
            && bars_left == usize::from(self.next_open);
        let wanted = if closing { Some(0) } else { wanted };
        if let Some(bar) = self.bars_ahead.get(next).filter(|_| !self.fills_finer) {
            self.bar_now.set_open(bar.1);
        }
        let volume = next
            .checked_sub(1)
            .and_then(|at| self.bars_ahead.get(at))
            .map_or(0.0, |bar| bar.4);
        self.bar_now.set_volume(volume);
        if !self.range_costs.is_empty() {
            let closed = next.checked_sub(1).and_then(|at| self.range_costs.get(at));
            self.range_now = (
                closed.copied().unwrap_or(0.0),
                self.range_costs.get(next).copied().unwrap_or(0.0),
            );
            self.bar_now
                .set_range_costs(self.range_now.0, self.range_now.1);
        }
        if let Some(share) = self.participation {
            self.bar_now
                .set_room(self.bars_ahead.get(next).map_or(0.0, |bar| bar.4 * share));
        }
        self.last_candle = Some(candle);
        self.charge_holding(&candle);
        if self.position > 0.0 {
            self.best_since_entry = self.best_since_entry.max(candle.high);
        } else if self.position < 0.0 {
            self.best_since_entry = self.best_since_entry.min(candle.low);
        }
        self.now = Decision {
            ts: candle.ts,
            wanted,
            held: self.held(),
            target: None,
            order: 0.0,
            why: "warming up",
        };
        // Bars are stamped with their close: the moment an order would go out.
        if candle.ts > self.trade_from {
            self.decide(&candle, wanted)?;
            self.keep_brackets(&candle)?;
        }
        if self.log_decisions {
            self.missed.borrow_mut().decisions.push(self.now);
        }
        Ok(())
    }
}

impl SignalStrategy {
    /// Acts on what the strategy wants at this close.
    fn decide(&mut self, candle: &Candle, wanted: Option<i8>) -> anyhow::Result<()> {
        // A check on everything below, so it mustn't lean on any of it: what's asked for by now
        // should be held, unless an order is still on its way or was refused.
        if self.in_flight.is_empty()
            && self.working.is_none()
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
        self.now.target = target;
        self.now.why = match (wanted, target) {
            (None, None) => "nothing asked",
            (Some(_), None) => "the rules held it back",
            _ => "already there",
        };
        // An exit that didn't fill as a limit still has to happen.
        let at_market =
            target.is_none() && lapsed.is_some() || target == lapsed && lapsed.is_some();
        let target = target.or(lapsed).map(|target| {
            if self.long_only {
                target.max(0)
            } else {
                target
            }
        });
        // Stopping a live session closes the position; nothing new meanwhile.
        if candle.close <= 0.0 || self.is_exiting() {
            self.now.why = "can't trade now";
            return Ok(());
        }
        // A different wish takes the place of what's still going out; otherwise, on it goes.
        let replaced = self
            .working
            .is_some_and(|working| target.is_some_and(|t| t != working.target));
        if replaced {
            self.stop_working(true);
        }
        if let Some(working) = self.working {
            return self.keep_working(candle, working);
        }
        let Some(target) = target else {
            return Ok(());
        };
        // A newer wish replaces what's resting; the same one leaves it be.
        if let Some(resting) = self.resting {
            if resting.target == target {
                self.now.why = "its limit is still resting";
                return Ok(());
            }
            self.end_resting(LimitOutcome::Replaced)?;
        }
        let expected = self.expected();
        // Part of what's held may be left from what was replaced, so that still has to reach its size.
        if sign_of(expected) == target && !replaced {
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
                self.now.why = "too small for the market's steps";
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
        if self.watch_volume && volume > 0.0 && qty.as_f64() > volume * self.volume_warning {
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
                if self.participation.is_some() {
                    let goal = f64::from(target) * size;
                    self.missed.borrow_mut().unfilled = 0.0;
                    let working = Working {
                        target,
                        goal,
                        bars: 0,
                    };
                    self.working = Some(working);
                    if self.sliced(delta.abs(), volume) < qty.as_f64() {
                        self.missed.borrow_mut().orders_worked += 1;
                    }
                    return self.keep_working(candle, working);
                }
                // What's held is going, so what guards it goes first.
                self.cancel_brackets()?;
                self.market_order(side, qty, false)
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
                self.quoted.insert(order.client_order_id(), quoted.as_f64());
                let mut missed = self.missed.borrow_mut();
                missed.limits.push(RawLimit {
                    placed: candle.ts,
                    side: if delta > 0.0 { Side::Buy } else { Side::Sell },
                    price: quoted.as_f64(),
                    qty: qty.as_f64(),
                    filled: 0.0,
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
        self.now.order = delta.signum() * qty.as_f64();
        self.now.why = if limit.is_some() {
            "a limit went out"
        } else {
            "an order went out"
        };
        self.submit_order(order, None, None, None)
    }

    fn market_order(&mut self, side: OrderSide, qty: Quantity, reduce_only: bool) -> OrderAny {
        let instrument_id = self.bar_type.instrument_id();
        self.order().market(
            instrument_id,
            side,
            qty,
            None,                        // time_in_force
            reduce_only.then_some(true), // reduce_only
            None,                        // quote_quantity
            None,                        // exec_algorithm_id
            None,                        // exec_algorithm_params
            None,                        // tags
            None,                        // client_order_id
        )
    }

    /// As much of `qty` as a bar of `volume` allows, down to the market's steps.
    fn sliced(&self, qty: f64, volume: f64) -> f64 {
        let Some(share) = self.participation else {
            return qty;
        };
        let steps = 10_f64.powi(i32::from(self.size_precision));
        qty.min((volume * share * steps).floor() / steps)
    }

    /// The volume of the bar last closed, what an order goes out against.
    fn last_volume(&self, ts: UnixNanos) -> f64 {
        self.bars_ahead
            .partition_point(|bar| bar.0 <= ts)
            .checked_sub(1)
            .and_then(|at| self.bars_ahead.get(at))
            .map_or(0.0, |bar| bar.4)
    }

    /// Sends the next part of an order too big for one bar.
    fn keep_working(&mut self, candle: &Candle, mut working: Working) -> anyhow::Result<()> {
        let expected = self.expected();
        // Outside the session nothing opens: a way out carries on, and a way in stops where it is.
        if working.target != 0 && self.outside_session(candle.ts) {
            if sign_of(expected) == working.target {
                self.stop_working(true);
                self.asked_size = expected.abs();
                return Ok(());
            }
            working.target = 0;
            working.goal = 0.0;
            self.asked = 0;
        }
        working.bars += 1;
        self.working = Some(working);
        let left = self.in_steps(working.goal - expected);
        if left == 0.0 {
            self.stop_working(false);
            return Ok(());
        }
        let side = if left > 0.0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };
        let slice = self.sliced(left.abs(), self.last_volume(candle.ts));
        let qty = Quantity::new(slice, self.size_precision);
        self.missed.borrow_mut().unfilled = left.abs();
        if qty.is_zero() {
            // No volume to take on this bar; the next may have some.
            self.now.why = "no volume to take on this bar";
            return Ok(());
        }
        if self.in_steps(left.abs() - qty.as_f64()) == 0.0 {
            self.stop_working(false);
        } else {
            self.missed.borrow_mut().unfilled = left.abs() - qty.as_f64();
        }
        self.last_refused = false;
        self.cancel_brackets()?;
        let order = self.market_order(side, qty, false);
        let signed = if left > 0.0 {
            qty.as_f64()
        } else {
            -qty.as_f64()
        };
        self.in_flight.push((order.client_order_id(), signed));
        self.now.order = signed;
        self.now.why = "part of an order went out";
        self.submit_order(order, None, None, None)
    }

    /// Done with what's going out over several bars, whether it all went (`cut_short` if not).
    fn stop_working(&mut self, cut_short: bool) {
        let Some(working) = self.working.take() else {
            return;
        };
        let mut missed = self.missed.borrow_mut();
        missed.unfilled = 0.0;
        if working.bars <= 1 && !cut_short {
            return;
        }
        if cut_short {
            missed.cut_short += 1;
        } else {
            missed.works_done += 1;
            missed.work_bars += working.bars;
            missed.longest_work = missed.longest_work.max(working.bars);
        }
    }

    fn outside_session(&self, ts: UnixNanos) -> bool {
        self.rules
            .session
            .as_ref()
            .is_some_and(|session| !(session.contains)(ts))
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
        // Part of it may have filled, and that's now all that's asked for.
        self.asked_size = self.position.abs();
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
    fn after_fill(&mut self, order: ClientOrderId, filled_at: UnixNanos, filled: f64) {
        let bracket = self.brackets.stop == Some(order) || self.brackets.take == Some(order);
        if bracket {
            let stopped = self.brackets.stop == Some(order);
            if self.log_decisions {
                let row = Decision {
                    ts: filled_at,
                    wanted: None,
                    held: self.held(),
                    target: None,
                    order: filled,
                    why: match (stopped, self.brackets.stop_trails) {
                        (true, true) => "a trailing stop filled",
                        (true, false) => "a stop filled",
                        (false, _) => "a target filled",
                    },
                };
                self.missed.borrow_mut().decisions.push(row);
            }
            // Part of a target filled, and the rest of it still rests.
            if !stopped && self.position != 0.0 {
                return;
            }
            // The position's gone, and with it what guarded it and any exit still waiting. A stop
            // that could only take part of it leaves the rest to go out over the next bars.
            self.asked = 0;
            let _ = self.cancel_brackets_except(order);
            let _ = self.end_resting(LimitOutcome::Cancelled);
            self.stop_working(true);
            if self.position != 0.0 {
                self.start_way_out();
            }
            return;
        }
        // Part of a limit filled, and the rest of it still rests.
        if self.in_flight.iter().any(|(id, _)| *id == order)
            && self.resting.is_some_and(|resting| resting.id == order)
        {
            if let Some(candle) = self.last_candle {
                let _ = self.keep_brackets(&candle);
            }
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

    /// Everything held goes out, over as many bars as it takes.
    fn start_way_out(&mut self) {
        self.working = Some(Working {
            target: 0,
            goal: 0.0,
            bars: 0,
        });
        self.missed.borrow_mut().orders_worked += 1;
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
        if self.stop_loss.is_none() && self.take_profit.is_none() && self.trailing.is_none() {
            return Ok(());
        }
        let held = self.held();
        // On its way out or over, so there's nothing to guard.
        if self.working.is_some_and(|working| working.target != held) {
            return Ok(());
        }
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
        let fixed = self
            .stop_loss
            .map(|percent| self.entry_price * (1.0 - side * percent / 100.0));
        // Whichever is nearer the price is the one that matters.
        let trail = self
            .trailing
            .map(|percent| self.best_since_entry * (1.0 - side * percent / 100.0));
        let stop_at = match (fixed, trail) {
            (Some(fixed), Some(trail)) => Some(if side > 0.0 {
                fixed.max(trail)
            } else {
                fixed.min(trail)
            }),
            (one, other) => one.or(other),
        };
        let trails = trail.is_some() && stop_at == trail;
        let take_at = self
            .take_profit
            .map(|percent| self.entry_price * (1.0 + side * percent / 100.0));
        // Already past the stop at this close, there's nothing to rest: out now.
        // A trailing stop's just moved here too, so it's the same.
        let moved =
            self.trailing.is_some() && stop_at.is_some_and(|at| at != self.brackets.stop_level);
        if (self.brackets.stop.is_none() || moved)
            && stop_at.is_some_and(|at| (candle.close - at) * side <= 0.0)
        {
            self.cancel_brackets()?;
            self.end_resting(LimitOutcome::Cancelled)?;
            self.asked = 0;
            self.stop_working(true);
            self.now.why = if trails {
                "past its trailing stop at this close"
            } else {
                "past its stop at this close"
            };
            if self.participation.is_some() {
                self.start_way_out();
                if let Some(working) = self.working {
                    return self.keep_working(candle, working);
                }
            }
            self.now.order = -side * qty.as_f64();
            let order = self.market_order(closing, qty, true);
            self.in_flight
                .push((order.client_order_id(), -side * qty.as_f64()));
            return self.submit_order(order, None, None, None);
        }
        // A stop takes what a bar's volume allows, and that changes with the bars.
        let stop_qty = Quantity::new(
            self.sliced(self.position.abs(), self.last_volume(candle.ts)),
            self.size_precision,
        );
        if let Some(id) = self.brackets.stop.filter(|_| {
            !stop_qty.is_zero() && (stop_qty.as_f64() != self.brackets.stop_qty || moved)
        }) {
            self.brackets.stop = None;
            self.cancel_order(id, None, None)?;
        }
        if let Some(id) = self
            .brackets
            .take
            .filter(|_| qty.as_f64() != self.brackets.take_qty)
        {
            self.brackets.take = None;
            self.cancel_order(id, None, None)?;
        }
        if let (Some(at), None, false) = (stop_at, self.brackets.stop, stop_qty.is_zero()) {
            let order = self.order().stop_market(
                instrument_id,
                closing,
                stop_qty,
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
            self.brackets.stop_qty = stop_qty.as_f64();
            self.brackets.stop_level = at;
            self.brackets.stop_trails = trails;
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
                self.quoted.insert(order.client_order_id(), take);
                self.brackets.take = Some(order.client_order_id());
                self.brackets.take_qty = qty.as_f64();
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
