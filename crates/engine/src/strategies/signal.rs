//! The bridge from "which position do I want?" to Nautilus orders.

use std::{cell::RefCell, fmt, rc::Rc};

use nautilus_common::actor::DataActor;
use nautilus_model::{
    data::{Bar, BarType},
    enums::OrderSide,
    events::OrderFilled,
    identifiers::StrategyId,
    types::Quantity,
};
use nautilus_trading::{
    nautilus_strategy,
    strategy::{Strategy, StrategyConfig, StrategyCore},
};
use persephone_core::{Candle, Side};

use crate::report::RawFill;

/// A trading rule: sees each closed bar and returns the position it wants, `1` long, `-1`
/// short or `0` flat, or `None` to keep whatever it has.
pub(crate) trait Signal {
    fn on_bar(&mut self, bar: &Candle) -> Option<i8>;
}

/// Runs a [`Signal`] inside Nautilus. Each position is worth `notional` in the quote currency
/// at entry; reversing long to short (or back) is one order.
pub(crate) struct SignalStrategy {
    core: StrategyCore,
    bar_type: BarType,
    signal: Box<dyn Signal>,
    notional: f64,
    size_precision: u8,
    /// Signed position in base units, kept from fills.
    position: f64,
    /// Shared with the engine thread that runs this strategy; collected after the run.
    fills: Rc<RefCell<Vec<RawFill>>>,
}

impl SignalStrategy {
    pub(crate) fn new(
        bar_type: BarType,
        signal: Box<dyn Signal>,
        notional: f64,
        size_precision: u8,
        fills: Rc<RefCell<Vec<RawFill>>>,
    ) -> Self {
        let config = StrategyConfig {
            strategy_id: Some(StrategyId::from("PERSEPHONE-001")),
            order_id_tag: Some("001".to_string()),
            log_events: false,
            ..Default::default()
        };
        Self {
            core: StrategyCore::new(config),
            bar_type,
            signal,
            notional,
            size_precision,
            position: 0.0,
            fills,
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

nautilus_strategy!(SignalStrategy, {
    fn on_order_filled(&mut self, event: &OrderFilled) {
        let qty = event.last_qty.as_f64();
        let side = match event.order_side {
            OrderSide::Sell => Side::Sell,
            _ => Side::Buy,
        };
        self.position += if side == Side::Buy { qty } else { -qty };
        self.fills.borrow_mut().push(RawFill {
            ts: event.ts_event.as_u64(),
            side,
            price: event.last_px.as_f64(),
            qty,
            fee: event.commission.map_or(0.0, |fee| fee.as_f64()),
        });
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
        self.subscribe_bars(self.bar_type, None, None);
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
        let Some(target) = self.signal.on_bar(&candle) else {
            return Ok(());
        };
        if self.holds(target) || candle.close <= 0.0 {
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
