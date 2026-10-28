//! How the simulated venue fills orders and how long they take to reach it, from
//! [`SimSettings`](crate::SimSettings). These sit on Nautilus's fill and latency hooks, where
//! slippage and latency plugins will sit too.

use nautilus_execution::models::fill::FillModel;
use nautilus_model::{
    data::order::BookOrder,
    enums::{BookType, OrderSide},
    instruments::{Instrument, InstrumentAny},
    orderbook::OrderBook,
    orders::OrderAny,
    types::{Price, Quantity},
};

/// Fills every order at the best price, moved against it by the slippage and, when the data
/// has no spread, by half an assumed spread.
#[derive(Debug)]
pub(crate) struct CostFillModel {
    slippage_ticks: u32,
    spread_ticks: u32,
}

impl CostFillModel {
    pub(crate) fn new(slippage_ticks: u32, spread_ticks: u32) -> Self {
        Self {
            slippage_ticks,
            spread_ticks,
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
    fn is_limit_filled(&mut self) -> anyhow::Result<bool> {
        Ok(true)
    }

    fn is_slipped(&mut self) -> anyhow::Result<bool> {
        Ok(false)
    }

    fn get_orderbook_for_fill_simulation(
        &mut self,
        instrument: &InstrumentAny,
        _order: &OrderAny,
        best_bid: Option<Price>,
        best_ask: Option<Price>,
    ) -> anyhow::Result<Option<OrderBook>> {
        let (Some(bid), Some(ask)) = (best_bid, best_ask) else {
            return Ok(None);
        };
        let (buy_cost, sell_cost) = self.costs(ask > bid);
        let tick = instrument.price_increment().as_f64();
        let precision = instrument.price_precision();
        let ask = Price::new(ask.as_f64() + f64::from(buy_cost) * tick, precision);
        let bid = Price::new(bid.as_f64() - f64::from(sell_cost) * tick, precision);

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
        let model = CostFillModel::new(2, 3);
        assert_eq!(
            model.costs(false),
            (4, 3),
            "a 3-tick spread: 2 above, 1 below"
        );
        assert_eq!(model.costs(true), (2, 2), "quotes bring their own spread");
        assert_eq!(CostFillModel::new(0, 0).costs(false), (0, 0));
    }
}
