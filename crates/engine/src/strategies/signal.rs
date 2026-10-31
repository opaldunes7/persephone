//! The bridge from "which position do I want?" to Nautilus orders.

use std::{cell::RefCell, fmt, rc::Rc};

use nautilus_common::actor::DataActor;
use nautilus_model::{
    data::{Bar, BarType},
    enums::OrderSide,
    events::{OrderDenied, OrderFilled, OrderRejected},
    identifiers::{ClientOrderId, StrategyId},
    instruments::Instrument,
    orders::Order as _,
    types::Quantity,
};
use nautilus_trading::{
    nautilus_strategy,
    strategy::{Strategy, StrategyConfig, StrategyCore},
};
use persephone_core::{Candle, Side, UnixNanos, signal::Signal};

use crate::{report::RawFill, settings::TradingRules};

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

/// Where a run didn't do what its strategy asked, with nothing else having said so.
#[derive(Debug, Default)]
pub(crate) struct Missed {
    /// Bars where what's held wasn't what was asked for, and the first of them.
    pub(crate) unmet: usize,
    pub(crate) first_unmet: UnixNanos,
    /// Bars where the position asked for rounded to nothing, and the first of them.
    pub(crate) too_small: usize,
    pub(crate) first_too_small: UnixNanos,
}

impl Missed {
    fn unmet(&mut self, ts: UnixNanos) {
        if self.unmet == 0 {
            self.first_unmet = ts;
        }
        self.unmet += 1;
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
        let value = qty * event.last_px.as_f64();
        self.cash += if side == Side::Buy { -value } else { value };
        self.cash -= event.commission.map_or(0.0, |fee| fee.as_f64());
        if self.position == 0.0 {
            self.entry_price = f64::NAN;
        } else if before == 0.0 || before.is_sign_positive() != self.position.is_sign_positive() {
            self.entry_price = event.last_px.as_f64();
        }
        let fill = RawFill {
            ts: event.ts_event.as_u64(),
            side,
            price: event.last_px.as_f64(),
            qty,
            fee: event.commission.map_or(0.0, |fee| fee.as_f64()),
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
        // Bars are stamped with their close: the moment an order would go out.
        if candle.ts <= self.trade_from {
            return Ok(());
        }
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
        let expected = self.expected();
        let Some(target) = self.rules.target(wanted, sign_of(expected), candle.ts) else {
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
        self.asked = target;
        self.asked_size = size;
        self.last_refused = false;
        let side = if delta > 0.0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };
        let instrument_id = self.bar_type.instrument_id();
        let order = self.order().market(
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
        );
        // Before it goes, as a refusal can come back straight away.
        self.in_flight
            .push((order.client_order_id(), delta.signum() * qty.as_f64()));
        self.submit_order(order, None, None, None)
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
