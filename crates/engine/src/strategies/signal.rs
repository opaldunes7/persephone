//! The bridge from "which position do I want?" to Nautilus orders.

use std::{cell::RefCell, fmt, rc::Rc};

use nautilus_common::actor::DataActor;
use nautilus_model::{
    data::{Bar, BarType},
    enums::OrderSide,
    events::{OrderDenied, OrderFilled, OrderRejected},
    identifiers::StrategyId,
    instruments::Instrument,
    types::Quantity,
};
use nautilus_trading::{
    nautilus_strategy,
    strategy::{Strategy, StrategyConfig, StrategyCore},
};
use persephone_core::{Candle, Side, signal::Signal};

use crate::report::RawFill;

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
    /// Shared with the engine thread that runs this strategy; collected after the run.
    fills: Rc<RefCell<Vec<RawFill>>>,
    /// Set when the signal fails; the strategy stops trading and the run reports it.
    failure: Rc<RefCell<Option<String>>>,
    /// Flat instead of short, for spot accounts that can't borrow.
    long_only: bool,
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
            fills,
            failure,
            long_only: false,
            watcher: None,
        }
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

    fn report(&mut self, message: String) {
        if let Some(watcher) = &mut self.watcher {
            watcher.problem(message);
        }
    }

    fn holds(&self, target: i8) -> bool {
        match target {
            1 => self.position > 0.0,
            -1 => self.position < 0.0,
            _ => self.position == 0.0,
        }
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
        self.position += if side == Side::Buy { qty } else { -qty };
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
        self.report(format!("order rejected: {}", event.reason));
    }

    fn on_order_denied(&mut self, event: OrderDenied) {
        self.report(format!("order denied: {}", event.reason));
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
        let wanted = self.signal.borrow_mut().on_bar(&candle);
        if let Some(watcher) = &mut self.watcher {
            watcher.bar(&candle);
        }
        let target = match wanted {
            Ok(Some(target)) if self.long_only => target.max(0),
            Ok(Some(target)) => target,
            Ok(None) => return Ok(()),
            Err(message) => {
                self.report(format!("the strategy failed: {message}"));
                *self.failure.borrow_mut() = Some(message);
                return Ok(());
            }
        };
        // Stopping a live session closes the position; nothing new meanwhile.
        if self.holds(target) || candle.close <= 0.0 || self.is_exiting() {
            return Ok(());
        }

        let size = Quantity::new(self.notional / candle.close, self.size_precision).as_f64();
        let delta = f64::from(target) * size - self.position;
        let qty = Quantity::new(delta.abs(), self.size_precision);
        if qty.is_zero() {
            return Ok(());
        }
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
        self.submit_order(order, None, None, None)
    }
}
