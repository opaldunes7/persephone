//! How the simulated venue fills orders and how long they take to reach it, from
//! [`SimSettings`](crate::SimSettings). These sit on Nautilus's fill and latency hooks, where
//! slippage and latency plugins will sit too.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use nautilus_execution::models::fill::FillModel;
use nautilus_model::{
    data::order::BookOrder,
    enums::{BookType, OrderSide, OrderType},
    instruments::{Instrument, InstrumentAny},
    orderbook::OrderBook,
    orders::{Order as _, OrderAny},
    types::{Price, Quantity},
};

/// Fills every order at the best price, moved against it by the slippage and, when the data
/// has no spread, by half an assumed spread.
#[derive(Debug)]
pub(crate) struct CostFillModel {
    slippage_ticks: u32,
    spread_ticks: u32,
    /// Slippage as a share of the price, on top of the ticks.
    slippage_share: f64,
    /// How far a market order fills worse at all of a bar's volume, as a percentage.
    impact_share: f64,
    bars: Arc<BarNow>,
}

/// What the fill model is told about the bar being worked through, by the strategy, as it isn't told
/// itself. Kept as bits, to share without a lock.
#[derive(Debug, Default)]
pub(crate) struct BarNow {
    /// A bar arrives as jumps between its open, high, low and close, so a stop passed on the way
    /// would otherwise fill at the far end of a jump. It should fill at its level, unless the bar
    /// opened beyond that.
    open: AtomicU64,
    /// The volume of the bar last closed, what an order goes out against.
    volume: AtomicU64,
}

impl BarNow {
    pub(crate) fn set_open(&self, open: f64) {
        self.open.store(open.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn set_volume(&self, volume: f64) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
    }
}

/// How far an order of `qty` fills worse on a bar of `volume`, as a share of the price.
#[must_use]
pub fn impact_share(impact_percent: f64, qty: f64, volume: f64) -> f64 {
    if impact_percent > 0.0 && volume > 0.0 {
        impact_percent / 100.0 * (qty / volume).sqrt()
    } else {
        0.0
    }
}

impl CostFillModel {
    pub(crate) fn new(
        slippage_ticks: u32,
        spread_ticks: u32,
        slippage_percent: f64,
        impact_percent: f64,
        bars: Arc<BarNow>,
    ) -> Self {
        Self {
            slippage_ticks,
            spread_ticks,
            slippage_share: slippage_percent / 100.0,
            impact_share: impact_percent,
            bars,
        }
    }

    /// Where a stop that's been reached fills, before slippage: its level, or the open if the bar
    /// opened beyond it.
    fn stop_price(&self, side: OrderSide, level: f64) -> f64 {
        let open = f64::from_bits(self.bars.open.load(Ordering::Relaxed));
        if !open.is_finite() || open <= 0.0 {
            return level;
        }
        match side {
            OrderSide::Sell => level.min(open),
            _ => level.max(open),
        }
    }

    /// Ticks a buy and a sell fill worse than the best ask and bid.
    fn costs(&self, spread_in_data: bool) -> (u32, u32) {
        let spread = if spread_in_data { 0 } else { self.spread_ticks };
        (
            self.slippage_ticks + spread.div_ceil(2),
            self.slippage_ticks + spread / 2,
        )
    }
}

impl FillModel for CostFillModel {
    /// Asked only when price is exactly at an order's limit. Never: a touch doesn't fill, as those
    /// ahead in the queue may have taken all there was. Past the limit, it isn't asked.
    fn is_limit_filled(&mut self) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn is_slipped(&mut self) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn get_orderbook_for_fill_simulation(
        &mut self,
        instrument: &InstrumentAny,
        order: &OrderAny,
        best_bid: Option<Price>,
        best_ask: Option<Price>,
    ) -> anyhow::Result<Option<OrderBook>> {
        let (Some(bid), Some(ask)) = (best_bid, best_ask) else {
            return Ok(None);
        };
        let (buy_cost, sell_cost) = self.costs(ask > bid);
        let tick = instrument.price_increment().as_f64();
        let precision = instrument.price_precision();
        let (mut ask, mut bid) = (ask.as_f64(), bid.as_f64());
        match (order.order_type(), order.price(), order.trigger_price()) {
            // A resting limit gets its own price: what it offered, however far the market went past.
            // It pays no slippage, as it was there first.
            (OrderType::Limit, Some(limit), _) => {
                let mut book = OrderBook::new(instrument.id(), BookType::L2_MBP);
                let depth = Quantity::from_mantissa_exponent(
                    10_000_000_000,
                    0,
                    instrument.size_precision(),
                );
                let book_side = if order.order_side() == OrderSide::Buy {
                    OrderSide::Sell
                } else {
                    OrderSide::Buy
                };
                book.add(BookOrder::new(book_side, limit, depth, 1), 0, 0, 0.into());
                return Ok(Some(book));
            }
            (OrderType::StopMarket, _, Some(level)) => {
                let filled = self.stop_price(order.order_side(), level.as_f64());
                if order.order_side() == OrderSide::Sell {
                    bid = filled;
                } else {
                    ask = filled;
                }
            }
            _ => {}
        }
        let volume = f64::from_bits(self.bars.volume.load(Ordering::Relaxed));
        let impact = impact_share(self.impact_share, order.quantity().as_f64(), volume);
        let share = self.slippage_share + impact;
        let ask = Price::new(ask + f64::from(buy_cost) * tick + ask * share, precision);
        let bid = Price::new(bid - f64::from(sell_cost) * tick - bid * share, precision);

        // As deep as Nautilus's own models: every order fills in full at one price.
        let depth =
            Quantity::from_mantissa_exponent(10_000_000_000, 0, instrument.size_precision());
        let mut book = OrderBook::new(instrument.id(), BookType::L2_MBP);
        if bid.is_positive() {
            book.add(
                BookOrder::new(OrderSide::Buy, bid, depth, 1),
                0,
                0,
                0.into(),
            );
        }
        book.add(
            BookOrder::new(OrderSide::Sell, ask, depth, 2),
            0,
            0,
            0.into(),
        );
        Ok(Some(book))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_split_the_spread_and_add_the_slippage() {
        let model = CostFillModel::new(2, 3, 0.0, 0.0, Arc::default());
        assert_eq!(
            model.costs(false),
            (4, 3),
            "a 3-tick spread: 2 above, 1 below"
        );
        assert_eq!(model.costs(true), (2, 2), "quotes bring their own spread");
        assert_eq!(
            CostFillModel::new(0, 0, 0.0, 0.0, Arc::default()).costs(false),
            (0, 0)
        );
    }
}
