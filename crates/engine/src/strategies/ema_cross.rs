//! Bar-driven dual-EMA crossover that flips between long and short.
//!
//! Nautilus ships an `EmaCross` example, but it trades on quote ticks. Charts work in bars, so
//! this one subscribes to bars and keeps one position open at a time.

use std::{cell::RefCell, fmt, rc::Rc};

use nautilus_common::actor::DataActor;
use nautilus_indicators::{
    average::ema::ExponentialMovingAverage,
    indicator::{Indicator, MovingAverage},
};
use nautilus_model::{
    data::{Bar, BarType},
    enums::{OrderSide, PriceType},
    events::OrderFilled,
    identifiers::StrategyId,
    types::Quantity,
};
use nautilus_trading::{
    nautilus_strategy,
    strategy::{Strategy, StrategyConfig, StrategyCore},
};
use persephone_core::Side;

use crate::report::Fill;

pub(crate) struct EmaCross {
    core: StrategyCore,
    bar_type: BarType,
    trade_size: Quantity,
    fast: ExponentialMovingAverage,
    slow: ExponentialMovingAverage,
    prev_fast_above: Option<bool>,
    in_market: bool,
    /// Shared with the engine thread that runs this strategy; collected after the run.
    fills: Rc<RefCell<Vec<Fill>>>,
}

impl EmaCross {
    pub(crate) fn new(
        bar_type: BarType,
        trade_size: Quantity,
        fast_period: usize,
        slow_period: usize,
        fills: Rc<RefCell<Vec<Fill>>>,
    ) -> Self {
        let config = StrategyConfig {
            strategy_id: Some(StrategyId::from("EMA_CROSS-001")),
            order_id_tag: Some("001".to_string()),
            log_events: false,
            ..Default::default()
        };
        Self {
            core: StrategyCore::new(config),
            bar_type,
            trade_size,
            fast: ExponentialMovingAverage::new(fast_period, Some(PriceType::Last)),
            slow: ExponentialMovingAverage::new(slow_period, Some(PriceType::Last)),
            prev_fast_above: None,
            in_market: false,
            fills,
        }
    }

    /// Enters `side`, reversing any open position in one order.
    fn go(&mut self, side: OrderSide) -> anyhow::Result<()> {
        let size = if self.in_market {
            self.trade_size + self.trade_size
        } else {
            self.trade_size
        };
        let instrument_id = self.bar_type.instrument_id();
        let order = self.order().market(
            instrument_id,
            side,
            size,
            None, // time_in_force
            None, // reduce_only
            None, // quote_quantity
            None, // exec_algorithm_id
            None, // exec_algorithm_params
            None, // tags
            None, // client_order_id
        );
        self.submit_order(order, None, None, None)?;
        self.in_market = true;
        Ok(())
    }
}

nautilus_strategy!(EmaCross, {
    fn on_order_filled(&mut self, event: &OrderFilled) {
        let side = match event.order_side {
            OrderSide::Sell => Side::Sell,
            _ => Side::Buy,
        };
        self.fills.borrow_mut().push(Fill {
            ts: event.ts_event.as_u64(),
            side,
            price: event.last_px.as_f64(),
            qty: event.last_qty.as_f64(),
        });
    }
});

impl fmt::Debug for EmaCross {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmaCross")
            .field("bar_type", &self.bar_type)
            .field("trade_size", &self.trade_size)
            .field("fast_period", &self.fast.period)
            .field("slow_period", &self.slow.period)
            .finish()
    }
}

impl DataActor for EmaCross {
    fn on_start(&mut self) -> anyhow::Result<()> {
        self.subscribe_bars(self.bar_type, None, None);
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        self.unsubscribe_bars(self.bar_type, None, None);
        Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        self.fast.handle_bar(bar);
        self.slow.handle_bar(bar);
        if !self.fast.initialized() || !self.slow.initialized() {
            return Ok(());
        }

        let fast_above = self.fast.value() > self.slow.value();
        match self.prev_fast_above {
            Some(false) if fast_above => self.go(OrderSide::Buy)?,
            Some(true) if !fast_above => self.go(OrderSide::Sell)?,
            _ => {}
        }
        self.prev_fast_above = Some(fast_above);
        Ok(())
    }
}
