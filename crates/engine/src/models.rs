//! How the simulated venue fills orders and how long they take to reach it, from
//! [`SimSettings`](crate::SimSettings). These sit on Nautilus's fill and latency hooks, where
//! slippage and latency plugins will sit too.

use std::{
    cell::Cell,
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use nautilus_core::DurationNanos;
use nautilus_execution::models::{fill::FillModel, latency::LatencyModel};
use nautilus_model::{
    data::order::BookOrder,
    enums::{BookType, OrderSide, OrderType},
    identifiers::InstrumentId,
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
    impact_exponent: f64,
    /// Resting limits fill only as far as `bars` has room for.
    limited: bool,
    bars: Arc<BarNow>,
    /// Each market's own, when there are several on the venue.
    bars_by_market: HashMap<InstrumentId, Arc<BarNow>>,
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
    /// How much resting limits may still fill on the bar being worked through, when that's limited.
    room: AtomicU64,
    /// What a market order and a stop pay on top in price, from the ranges of the bar just closed and
    /// the bar being worked through.
    range_closed: AtomicU64,
    range_ahead: AtomicU64,
}

impl BarNow {
    pub(crate) fn set_open(&self, open: f64) {
        self.open.store(open.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn set_volume(&self, volume: f64) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn set_range_costs(&self, closed: f64, ahead: f64) {
        self.range_closed.store(closed.to_bits(), Ordering::Relaxed);
        self.range_ahead.store(ahead.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn set_room(&self, room: f64) {
        self.room.store(room.to_bits(), Ordering::Relaxed);
    }

    /// Takes as much of `wanted` as there's room for this bar, and says how much that was. A bar
    /// arrives as several prices, each of which may reach a limit, so one takes what the last left.
    fn take_room(&self, wanted: f64) -> f64 {
        let room = f64::from_bits(self.room.load(Ordering::Relaxed));
        let taken = wanted.min(room).max(0.0);
        self.set_room(room - taken);
        taken
    }
}

/// How far an order of `qty` fills worse on a bar of `volume`, as a share of the price.
#[must_use]
pub fn impact_share(impact_percent: f64, exponent: f64, qty: f64, volume: f64) -> f64 {
    if impact_percent > 0.0 && volume > 0.0 {
        impact_percent / 100.0 * (qty / volume).powf(exponent)
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
        impact_exponent: f64,
        limited: bool,
        bars: Arc<BarNow>,
    ) -> Self {
        Self {
            slippage_ticks,
            spread_ticks,
            slippage_share: slippage_percent / 100.0,
            impact_share: impact_percent,
            impact_exponent,
            limited,
            bars,
            bars_by_market: HashMap::new(),
        }
    }

    /// Reads each market's bar from its own, for a venue with several.
    pub(crate) fn by_market(mut self, bars: HashMap<InstrumentId, Arc<BarNow>>) -> Self {
        self.bars_by_market = bars;
        self
    }

    fn bars_of(&self, id: &InstrumentId) -> &BarNow {
        self.bars_by_market.get(id).unwrap_or(&self.bars)
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
                let depth = if self.limited {
                    let room = self.bars.take_room(order.leaves_qty().as_f64());
                    Quantity::new(room, instrument.size_precision())
                } else {
                    Quantity::from_mantissa_exponent(10_000_000_000, 0, instrument.size_precision())
                };
                // With no room left there's nothing to fill against, and it keeps resting.
                if depth.is_zero() {
                    return Ok(Some(OrderBook::new(instrument.id(), BookType::L2_MBP)));
                }
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
        // A stop goes in the bar it's reached in. Its conditions are the venue's and not something the
        // strategy sees, so that's no look ahead. A market order goes at a close, off the bar just over.
        let bars = self.bars_of(&instrument.id());
        let range_cost = f64::from_bits(
            if order.order_type() == OrderType::StopMarket {
                &bars.range_ahead
            } else {
                &bars.range_closed
            }
            .load(Ordering::Relaxed),
        );
        let volume = f64::from_bits(bars.volume.load(Ordering::Relaxed));
        let impact = impact_share(
            self.impact_share,
            self.impact_exponent,
            order.quantity().as_f64(),
            volume,
        );
        let share = self.slippage_share + impact;
        let ask = Price::new(
            ask + f64::from(buy_cost) * tick + ask * share + range_cost,
            precision,
        );
        let bid = Price::new(
            bid - f64::from(sell_cost) * tick - bid * share - range_cost,
            precision,
        );

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

/// What a taker pays on top on each bar, in price: `percent` of its range, times how many times wider
/// it is than the median of the `look_back` before it, when that's wider at all.
#[must_use]
pub fn range_costs(
    percent: f64,
    look_back: u32,
    ranges: impl IntoIterator<Item = f64>,
) -> Vec<f64> {
    let look_back = look_back.max(1) as usize;
    let mut order: Vec<f64> = Vec::with_capacity(look_back + 1);
    let mut recent = std::collections::VecDeque::with_capacity(look_back + 1);
    ranges
        .into_iter()
        .map(|range| {
            let usual = if order.is_empty() {
                0.0
            } else if order.len() % 2 == 1 {
                order[order.len() / 2]
            } else {
                (order[order.len() / 2 - 1] + order[order.len() / 2]) / 2.0
            };
            let wider = if usual > 0.0 {
                (range / usual).max(1.0)
            } else {
                1.0
            };
            let at = order.partition_point(|&other| other < range);
            order.insert(at, range);
            recent.push_back(range);
            if recent.len() > look_back
                && let Some(gone) = recent.pop_front()
            {
                let at = order.partition_point(|&other| other < gone);
                order.remove(at);
            }
            percent / 100.0 * range * wider
        })
        .collect()
}

/// A fixed latency plus a random extra, drawn afresh for each order from an exponential distribution,
/// so most are quick and a few are slow. The same seed gives the same draws.
#[derive(Debug)]
pub(crate) struct JitteredLatency {
    base_nanos: u64,
    average_extra_nanos: f64,
    state: Cell<u64>,
}

impl JitteredLatency {
    pub(crate) fn new(latency_ms: u32, jitter_ms: u32, seed: u64) -> Self {
        Self {
            base_nanos: u64::from(latency_ms) * 1_000_000,
            average_extra_nanos: f64::from(jitter_ms) * 1_000_000.0,
            state: Cell::new(seed),
        }
    }

    /// The next of the draws, by SplitMix64, as a share between 0 and 1.
    fn draw(&self) -> f64 {
        let state = self.state.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.state.set(state);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1_u64 << 53) as f64
    }

    fn next(&self) -> DurationNanos {
        let extra = -(1.0 - self.draw()).ln() * self.average_extra_nanos;
        DurationNanos::new(self.base_nanos + extra as u64)
    }
}

impl LatencyModel for JitteredLatency {
    fn get_insert_latency(&self) -> DurationNanos {
        self.next()
    }

    fn get_update_latency(&self) -> DurationNanos {
        self.next()
    }

    fn get_delete_latency(&self) -> DurationNanos {
        self.next()
    }

    fn get_base_latency(&self) -> DurationNanos {
        DurationNanos::new(self.base_nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wide_bar_pays_for_its_width_twice() {
        // 1% of each range: the first has nothing to compare with, and the next match their median.
        let costs = range_costs(1.0, 100, [1.0, 1.0, 1.0, 10.0, 1.0]);
        assert!((costs[0] - 0.01).abs() < 1e-12);
        assert!((costs[2] - 0.01).abs() < 1e-12);
        assert!(
            (costs[3] - 1.0).abs() < 1e-12,
            "10 wide, 10 times the usual"
        );
        assert!(
            (costs[4] - 0.01).abs() < 1e-12,
            "one wide bar doesn't move a median"
        );
    }

    #[test]
    fn narrow_bars_pay_only_their_range_and_the_median_looks_back_100() {
        let costs = range_costs(100.0, 100, [2.0, 1.0]);
        assert!((costs[1] - 1.0).abs() < 1e-12);
        let mut ranges = vec![1.0; 100];
        ranges.extend(vec![2.0; 100]);
        ranges.push(2.0);
        let costs = range_costs(100.0, 100, ranges);
        assert!((costs[100] - 4.0).abs() < 1e-12, "against 1s");
        assert!(
            (costs[200] - 2.0).abs() < 1e-12,
            "the 1s have left the window"
        );
    }

    #[test]
    fn jitter_repeats_by_seed_and_averages_what_it_says() {
        let draws = |seed| {
            let model = JitteredLatency::new(10, 50, seed);
            (0..20_000)
                .map(|_| model.get_insert_latency().as_u64())
                .collect::<Vec<_>>()
        };
        assert_eq!(draws(1), draws(1));
        assert_ne!(draws(1), draws(2));
        let all = draws(1);
        assert!(all.iter().all(|&nanos| nanos >= 10_000_000));
        let average = all.iter().sum::<u64>() as f64 / all.len() as f64 / 1e6;
        assert!((average - 60.0).abs() < 2.0, "{average}");
    }

    #[test]
    fn costs_split_the_spread_and_add_the_slippage() {
        let model = CostFillModel::new(2, 3, 0.0, 0.0, 0.5, false, Arc::default());
        assert_eq!(
            model.costs(false),
            (4, 3),
            "a 3-tick spread: 2 above, 1 below"
        );
        assert_eq!(model.costs(true), (2, 2), "quotes bring their own spread");
        assert_eq!(
            CostFillModel::new(0, 0, 0.0, 0.0, 0.5, false, Arc::default()).costs(false),
            (0, 0)
        );
    }
}
