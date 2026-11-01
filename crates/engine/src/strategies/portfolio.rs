//! Several markets on one account: the Nautilus strategy behind [`run_portfolio`].
//!
//! Each moment on the shared timeline it waits for every leg's bar, so that nothing is ever worked
//! out from prices of different times. Then it settles what's due, checks the margin, hears the
//! signals, and sends what moves each leg towards where it should be.
//!
//! [`run_portfolio`]: crate::run_portfolio

use std::{cell::RefCell, fmt, rc::Rc, sync::Arc};

use nautilus_common::actor::DataActor;
use nautilus_model::{
    data::{Bar, BarType},
    enums::OrderSide,
    events::{OrderDenied, OrderFilled, OrderRejected},
    identifiers::{ClientOrderId, InstrumentId, StrategyId},
    orders::Order as _,
    types::Quantity,
};
use nautilus_trading::{
    nautilus_strategy,
    strategy::{Strategy, StrategyConfig, StrategyCore},
};
use persephone_core::{Candle, Side, UnixNanos, signal::Signal};

use crate::{
    models::BarNow,
    report::{NANOS_PER_YEAR, RawFill},
    settings::TradingRules,
};

/// A leg's bar at one moment on the timeline.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Step {
    pub(crate) candle: Candle,
    /// There was a bar. Otherwise this one carries the last close at no volume, so everything can be
    /// valued at this moment, but nothing can trade on it.
    pub(crate) real: bool,
    /// What a taker pays on top, in price, going out at this bar's close.
    pub(crate) range_cost: f64,
}

/// How a leg decides what it wants.
pub(crate) enum Wants {
    /// A signal's long, short or flat, each worth this share of the capital.
    Signal(Box<dyn Signal>, f64),
    /// Always this share of the capital, or of the account if rebalanced, signed.
    Hold(f64),
    /// What the weight table says, as a share of the account.
    Table,
}

/// One market in the portfolio.
pub(crate) struct Leg {
    pub(crate) id: InstrumentId,
    pub(crate) bar_type: BarType,
    pub(crate) size_precision: u8,
    pub(crate) wants: Wants,
    /// Timeline positions, `None` before its first bar.
    pub(crate) steps: Arc<Vec<Option<Step>>>,
    pub(crate) funding: Arc<Vec<(UnixNanos, f64)>>,
    /// Shorts here pay to borrow; perpetuals pay funding instead.
    pub(crate) borrows: bool,
    pub(crate) bar_now: Arc<BarNow>,
    pub(crate) next_funding: usize,
    pub(crate) position: f64,
    pub(crate) entry_price: f64,
    pub(crate) in_flight: Vec<(ClientOrderId, f64)>,
    /// The share of the capital or account it should be, signed.
    pub(crate) share: f64,
    /// The share has changed, so the position it means needs working out at the next real bar.
    pub(crate) stale: bool,
    /// The signed position it's working towards.
    pub(crate) goal: f64,
    /// How many bars an order too big for its bar has been going.
    pub(crate) working_bars: usize,
    /// What the bar last really there traded and costs a taker, in price, for orders going out on it.
    pub(crate) last_volume: f64,
    pub(crate) range_now: f64,
}

impl Leg {
    fn in_steps(&self, value: f64) -> f64 {
        let steps = 10_f64.powi(i32::from(self.size_precision));
        (value * steps).round() / steps
    }

    fn expected(&self) -> f64 {
        let coming: f64 = self.in_flight.iter().map(|(_, left)| left).sum();
        self.in_steps(self.position + coming)
    }
}

/// How often legs go back to what they should be.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rebalance {
    /// Every this many bars.
    pub every_bars: Option<u32>,
    /// Whenever a leg is this many percentage points of the account from where it should be.
    pub drift_percent: Option<f64>,
    /// Trades worth less than this aren't made, in the quote currency.
    pub min_trade_value: f64,
}

/// What each leg's fills were and how the run went, gathered as it goes.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    pub(crate) fills: Vec<Vec<RawFill>>,
    pub(crate) refused: (usize, Option<String>),
    pub(crate) failure: Option<String>,
    pub(crate) liquidated_at: Option<UnixNanos>,
    /// The most of the account margin took, and the least it ever had over what's needed to stay open.
    pub(crate) peak_margin_use: f64,
    pub(crate) closest_to_liquidation: Option<f64>,
    pub(crate) orders_worked: Vec<usize>,
    pub(crate) work_bars: Vec<usize>,
    pub(crate) longest_work: Vec<usize>,
    pub(crate) works_done: Vec<usize>,
    pub(crate) unfilled: Vec<f64>,
    /// Orders replaced before all of them had gone out.
    pub(crate) cut_short: usize,
    pub(crate) too_small: usize,
    pub(crate) over_volume: usize,
    /// Table rows that gave a weight to a leg with no bars yet.
    pub(crate) early_weights: Vec<usize>,
}

pub(crate) struct Settings {
    pub(crate) starting_balance: f64,
    pub(crate) leverage: f64,
    pub(crate) compound: bool,
    /// As a share of a position's value.
    pub(crate) maintenance: f64,
    pub(crate) participation: Option<f64>,
    pub(crate) watch_volume: bool,
    /// The share of a bar's volume past which an order is worth a warning.
    pub(crate) volume_warning: f64,
    pub(crate) borrow_percent_per_year: f64,
    pub(crate) interest_percent_per_year: f64,
    /// Orders fill at the next bar's open, so they wait until there's one to meet.
    pub(crate) next_open: bool,
    pub(crate) rules: TradingRules,
    pub(crate) trade_from: UnixNanos,
    pub(crate) rebalance: Rebalance,
    /// Weights by when they apply, one for each leg.
    pub(crate) table: Option<crate::portfolio::WeightTable>,
}

pub(crate) struct PortfolioStrategy {
    core: StrategyCore,
    legs: Vec<Leg>,
    settings: Settings,
    /// The moments, and how many legs have a bar at each.
    times: Arc<Vec<UnixNanos>>,
    expected_bars: Arc<Vec<usize>>,
    at: Option<usize>,
    arrived: usize,
    /// What's come in and gone out, starting from nothing.
    cash: f64,
    last_step: Option<UnixNanos>,
    next_row: usize,
    bars_since_rebalance: u32,
    liquidated: bool,
    outcome: Rc<RefCell<Outcome>>,
}

impl fmt::Debug for PortfolioStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PortfolioStrategy")
            .field("legs", &self.legs.len())
            .field("cash", &self.cash)
            .finish_non_exhaustive()
    }
}

impl PortfolioStrategy {
    pub(crate) fn new(
        legs: Vec<Leg>,
        settings: Settings,
        times: Arc<Vec<UnixNanos>>,
        expected_bars: Arc<Vec<usize>>,
        outcome: Rc<RefCell<Outcome>>,
    ) -> Self {
        Self {
            core: StrategyCore::new(StrategyConfig {
                strategy_id: Some(StrategyId::from("PERSEPHONE-002")),
                order_id_tag: Some("002".to_string()),
                log_events: false,
                ..Default::default()
            }),
            legs,
            settings,
            times,
            expected_bars,
            at: None,
            arrived: 0,
            cash: 0.0,
            last_step: None,
            next_row: 0,
            bars_since_rebalance: 0,
            liquidated: false,
            outcome,
        }
    }

    fn step_of(&self, leg: usize, at: usize) -> Option<Step> {
        self.legs[leg].steps.get(at).copied().flatten()
    }

    /// Cash plus every position at this moment's closes, as the report values it.
    fn account(&self, at: usize) -> f64 {
        let held: f64 = (0..self.legs.len())
            .filter_map(|leg| Some(self.legs[leg].position * self.step_of(leg, at)?.candle.close))
            .sum();
        self.settings.starting_balance + self.cash + held
    }

    /// Everything falling due across the legs goes into the one cash balance at this moment.
    fn settle(&mut self, at: usize) {
        let years = self.last_step.map_or(0.0, |last| {
            self.times[at].saturating_sub(last) as f64 / NANOS_PER_YEAR
        });
        let borrow = self.settings.borrow_percent_per_year;
        let mut paid = 0.0;
        for index in 0..self.legs.len() {
            let Some(step) = self.step_of(index, at) else {
                continue;
            };
            let leg = &mut self.legs[index];
            while let Some(&(when, rate)) = leg.funding.get(leg.next_funding) {
                if when >= self.times[at] {
                    break;
                }
                paid += leg.position * step.candle.open * rate;
                leg.next_funding += 1;
            }
            if leg.borrows && leg.position < 0.0 {
                paid += -leg.position * step.candle.open * borrow / 100.0 * years;
            }
        }
        let cash = self.settings.starting_balance + self.cash;
        if cash < 0.0 {
            paid += -cash * self.settings.interest_percent_per_year / 100.0 * years;
        }
        self.cash -= paid;
        self.last_step = Some(self.times[at]);
    }

    /// Whether the account has fallen to what its positions need to stay open, at the worst each of
    /// them reached in this bar: which came first can't be known from bars, so this assumes the worse.
    fn margin_breached(&self, at: usize) -> bool {
        let mut worst = self.settings.starting_balance + self.cash;
        let (mut maintenance, mut needed) = (0.0, 0.0);
        for (index, leg) in self.legs.iter().enumerate() {
            let Some(step) = self.step_of(index, at) else {
                continue;
            };
            let adverse = if leg.position > 0.0 {
                step.candle.low
            } else {
                step.candle.high
            };
            worst += leg.position * adverse;
            maintenance += (leg.position * adverse).abs() * self.settings.maintenance;
            needed += (leg.position * step.candle.close).abs() / self.settings.leverage;
        }
        let account = self.account(at);
        let mut outcome = self.outcome.borrow_mut();
        if account > 0.0 {
            outcome.peak_margin_use = outcome.peak_margin_use.max(needed / account);
        }
        if maintenance <= 0.0 {
            return false;
        }
        let room = worst / maintenance;
        outcome.closest_to_liquidation = Some(
            outcome
                .closest_to_liquidation
                .map_or(room, |closest| closest.min(room)),
        );
        worst <= maintenance
    }

    /// The account is out of margin: everything goes, all of it at once. Limits on a bar's volume are
    /// for what a strategy chooses to do, and a venue closing a position doesn't wait.
    fn liquidate(&mut self, at: usize) -> anyhow::Result<()> {
        self.liquidated = true;
        self.outcome.borrow_mut().liquidated_at = Some(self.times[at]);
        for index in 0..self.legs.len() {
            self.legs[index].goal = 0.0;
            self.legs[index].share = 0.0;
            self.legs[index].stale = false;
            self.legs[index].working_bars = 0;
            self.outcome.borrow_mut().unfilled[index] = 0.0;
            if let Some(step) = self.step_of(index, at).filter(|step| step.real) {
                self.take_up_bar(index, step);
            }
            let held = self.legs[index].expected();
            if held != 0.0 {
                self.send(index, -held)?;
            }
        }
        Ok(())
    }

    /// Tells each signal of its bar. Bars carried over aren't real, so they aren't heard.
    fn hear_signals(&mut self, at: usize, candles: &[Option<Candle>]) {
        let trading = self.times[at] > self.settings.trade_from && !self.liquidated;
        for (index, candle) in candles.iter().enumerate() {
            let Some(candle) = *candle else {
                continue;
            };
            let expected = self.legs[index].expected();
            let leg = &mut self.legs[index];
            let wanted = match &mut leg.wants {
                Wants::Signal(signal, _) => {
                    signal.holding(sign_of(leg.position), leg.entry_price);
                    signal.on_bar(&candle)
                }
                _ => Ok(None),
            };
            let wanted = match wanted {
                Ok(wanted) => wanted,
                Err(message) => {
                    self.outcome.borrow_mut().failure = Some(message);
                    return;
                }
            };
            if !trading {
                continue;
            }
            let held = sign_of(expected);
            let target = self.settings.rules.target(wanted, held, self.times[at]);
            let leg = &mut self.legs[index];
            match &leg.wants {
                Wants::Signal(_, each) => {
                    if let Some(target) = target {
                        leg.share = f64::from(target) * each;
                        // Still going the way it was told to, it's left to get on with it.
                        if sign_of(leg.goal) != target {
                            leg.stale = true;
                        }
                    }
                }
                Wants::Hold(weight) => {
                    if leg.share != *weight {
                        leg.share = *weight;
                        leg.stale = true;
                    }
                }
                Wants::Table => {}
            }
        }
    }

    /// Takes up what the weight table says by now. A row states the whole book.
    fn read_table(&mut self, at: usize) {
        let Some(table) = self.settings.table.clone() else {
            return;
        };
        while let Some((when, weights)) = table.get(self.next_row) {
            if *when > self.times[at] {
                break;
            }
            for (index, weight) in weights.iter().enumerate() {
                let Some(leg) = self.legs.get_mut(index) else {
                    continue;
                };
                leg.share = *weight;
                leg.stale = true;
                if *weight != 0.0 && leg.steps[at].is_none() {
                    self.outcome.borrow_mut().early_weights[index] += 1;
                }
            }
            self.next_row += 1;
        }
    }

    /// Marks legs that are due to go back to where they should be.
    fn check_rebalance(&mut self, at: usize) {
        let rule = self.settings.rebalance;
        self.bars_since_rebalance += 1;
        let every = rule
            .every_bars
            .is_some_and(|bars| self.bars_since_rebalance >= bars);
        if every {
            self.bars_since_rebalance = 0;
        }
        let account = self.account(at);
        // One leg straying takes the whole book back, or what's sold wouldn't pay for what's bought.
        let drifted = rule.drift_percent.is_some_and(|percent| {
            account > 0.0
                && (0..self.legs.len()).any(|index| {
                    self.step_of(index, at).is_some_and(|step| {
                        let leg = &self.legs[index];
                        (leg.position * step.candle.close / account - leg.share).abs() * 100.0
                            > percent
                    })
                })
        });
        if every || drifted {
            for leg in &mut self.legs {
                if leg.share != 0.0 || leg.position != 0.0 {
                    leg.stale = true;
                }
            }
        }
    }

    /// What the share a leg should be comes to, in the market's steps, at this close.
    fn work_out_goals(&mut self, at: usize) {
        let account = self.account(at);
        let by_account = self.settings.table.is_some()
            || self.settings.compound
            || self.settings.rebalance.every_bars.is_some()
            || self.settings.rebalance.drift_percent.is_some();
        let capital = if by_account {
            account.max(0.0)
        } else {
            self.settings.starting_balance
        };
        for index in 0..self.legs.len() {
            let Some(step) = self.step_of(index, at).filter(|step| step.real) else {
                continue;
            };
            if !self.legs[index].stale || step.candle.close <= 0.0 {
                continue;
            }
            let min_trade = self.settings.rebalance.min_trade_value;
            let leg = &mut self.legs[index];
            leg.stale = false;
            let size = Quantity::new(
                leg.share.abs() * capital / step.candle.close,
                leg.size_precision,
            )
            .as_f64();
            let goal = leg.share.signum() * size;
            if size == 0.0 && leg.share != 0.0 {
                self.outcome.borrow_mut().too_small += 1;
            }
            let change = (goal - leg.expected()).abs() * step.candle.close;
            if change >= min_trade || goal == 0.0 {
                leg.goal = goal;
                // What was still going out has been replaced by this.
                if std::mem::take(&mut leg.working_bars) > 0 {
                    self.outcome.borrow_mut().cut_short += 1;
                }
            }
        }
    }

    /// As much of `qty` as a bar of `volume` allows, down to the market's steps.
    fn sliced(&self, leg: usize, qty: f64, volume: f64) -> f64 {
        let Some(share) = self.settings.participation else {
            return qty;
        };
        let steps = 10_f64.powi(i32::from(self.legs[leg].size_precision));
        qty.min((volume * share * steps).floor() / steps)
    }

    /// Moves each leg towards its goal, those coming down first, as they free margin for the others.
    fn trade(&mut self, at: usize) -> anyhow::Result<()> {
        let mut order: Vec<usize> = (0..self.legs.len()).collect();
        order.sort_by_key(|&index| {
            let leg = &self.legs[index];
            u8::from(leg.goal.abs() >= leg.expected().abs())
        });
        for index in order {
            let Some(step) = self.step_of(index, at) else {
                continue;
            };
            if step.real {
                self.take_up_bar(index, step);
            }
            // At a close, it trades on a bar that's there. At the next open, it has to have one coming.
            let can_go = if self.settings.next_open {
                self.legs[index]
                    .steps
                    .get(at + 1)
                    .is_none_or(|next| next.is_some_and(|next| next.real))
            } else {
                step.real
            };
            if !can_go {
                continue;
            }
            let left = {
                let leg = &self.legs[index];
                leg.in_steps(leg.goal - leg.expected())
            };
            let volume = self.legs[index].last_volume;
            if left == 0.0 {
                continue;
            }
            let slice = self.sliced(index, left.abs(), volume);
            let precision = self.legs[index].size_precision;
            let qty = Quantity::new(slice, precision).as_f64();
            let working = self.legs[index].working_bars > 0;
            let mut outcome = self.outcome.borrow_mut();
            if !working && qty < left.abs() {
                outcome.orders_worked[index] += 1;
            }
            if self.settings.watch_volume
                && volume > 0.0
                && qty > volume * self.settings.volume_warning
            {
                outcome.over_volume += 1;
            }
            outcome.unfilled[index] = left.abs() - qty;
            drop(outcome);
            if working || qty < left.abs() {
                self.legs[index].working_bars += 1;
            }
            if qty > 0.0 {
                self.send(index, left.signum() * qty)?;
            }
            if qty >= left.abs() {
                self.finish_work(index);
            }
        }
        Ok(())
    }

    /// Tells the fill model about a bar that was really there, which it goes by until the next.
    fn take_up_bar(&mut self, index: usize, step: Step) {
        let leg = &mut self.legs[index];
        leg.last_volume = step.candle.volume;
        leg.range_now = step.range_cost;
        leg.bar_now.set_volume(step.candle.volume);
        leg.bar_now.set_range_costs(step.range_cost, 0.0);
    }

    fn finish_work(&mut self, index: usize) {
        let bars = std::mem::take(&mut self.legs[index].working_bars);
        if bars > 0 {
            let mut outcome = self.outcome.borrow_mut();
            outcome.works_done[index] += 1;
            outcome.work_bars[index] += bars;
            outcome.longest_work[index] = outcome.longest_work[index].max(bars);
            outcome.unfilled[index] = 0.0;
        }
    }

    fn send(&mut self, index: usize, signed: f64) -> anyhow::Result<()> {
        let side = if signed > 0.0 {
            OrderSide::Buy
        } else {
            OrderSide::Sell
        };
        let id = self.legs[index].id;
        let qty = Quantity::new(signed.abs(), self.legs[index].size_precision);
        let order = self.order().market(
            id, side, qty, None, // time_in_force
            None, // reduce_only
            None, // quote_quantity
            None, // exec_algorithm_id
            None, // exec_algorithm_params
            None, // tags
            None, // client_order_id
        );
        self.legs[index]
            .in_flight
            .push((order.client_order_id(), signed));
        self.submit_order(order, None, None, None)
    }

    /// Every leg's bar for this moment is in: everything is worked out at the one time.
    fn step(&mut self, at: usize, candles: &[Option<Candle>]) -> anyhow::Result<()> {
        self.settle(at);
        self.hear_signals(at, candles);
        if self.outcome.borrow().failure.is_some() || self.liquidated {
            return Ok(());
        }
        if self.margin_breached(at) {
            return self.liquidate(at);
        }
        if self.times[at] <= self.settings.trade_from {
            return Ok(());
        }
        self.read_table(at);
        self.check_rebalance(at);
        self.work_out_goals(at);
        self.trade(at)
    }

    fn refuse(&mut self, order: ClientOrderId, message: String) {
        for leg in &mut self.legs {
            if leg.in_flight.iter().any(|(id, _)| *id == order) {
                leg.in_flight.retain(|(id, _)| *id != order);
                // Trying again each bar would only say the same: it stays until told otherwise.
                leg.goal = leg.expected();
                leg.working_bars = 0;
            }
        }
        let mut outcome = self.outcome.borrow_mut();
        outcome.refused.0 += 1;
        outcome.refused.1.get_or_insert(message);
    }
}

fn sign_of(value: f64) -> i8 {
    if value > 0.0 {
        1
    } else if value < 0.0 {
        -1
    } else {
        0
    }
}

nautilus_strategy!(PortfolioStrategy, {
    fn on_order_filled(&mut self, event: &OrderFilled) {
        let Some(index) = self
            .legs
            .iter()
            .position(|leg| leg.id == event.instrument_id)
        else {
            return;
        };
        let qty = event.last_qty.as_f64();
        let price = event.last_px.as_f64();
        let side = match event.order_side {
            OrderSide::Sell => Side::Sell,
            _ => Side::Buy,
        };
        let signed = if side == Side::Buy { qty } else { -qty };
        let fee = event.commission.map_or(0.0, |fee| fee.as_f64());
        self.cash -= signed * price + fee;
        let leg = &mut self.legs[index];
        let before = leg.position;
        leg.position = leg.in_steps(leg.position + signed);
        if let Some(at) = leg
            .in_flight
            .iter()
            .position(|(id, _)| *id == event.client_order_id)
        {
            leg.in_flight[at].1 -= signed;
            if leg.in_steps(leg.in_flight[at].1) == 0.0 {
                leg.in_flight.remove(at);
            }
        }
        if leg.position == 0.0 {
            leg.entry_price = f64::NAN;
        } else if before == 0.0 || before.is_sign_positive() != leg.position.is_sign_positive() {
            leg.entry_price = price;
        } else if leg.position.abs() > before.abs() {
            leg.entry_price = (before.abs() * leg.entry_price + qty * price) / leg.position.abs();
        }
        let range_slip = self.legs[index].range_now;
        self.outcome.borrow_mut().fills[index].push(RawFill {
            ts: event.ts_event.as_u64(),
            side,
            price,
            qty,
            fee,
            maker: false,
            range_slip,
        });
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

impl DataActor for PortfolioStrategy {
    fn on_start(&mut self) -> anyhow::Result<()> {
        let types: Vec<BarType> = self.legs.iter().map(|leg| leg.bar_type).collect();
        for bar_type in types {
            self.subscribe_bars(bar_type, None, None);
        }
        Ok(())
    }

    fn on_stop(&mut self) -> anyhow::Result<()> {
        let types: Vec<BarType> = self.legs.iter().map(|leg| leg.bar_type).collect();
        for bar_type in types {
            self.unsubscribe_bars(bar_type, None, None);
        }
        Ok(())
    }

    fn on_bar(&mut self, bar: &Bar) -> anyhow::Result<()> {
        let ts = bar.ts_event.as_u64();
        let Ok(at) = self.times.binary_search(&ts) else {
            return Ok(());
        };
        if self.at != Some(at) {
            self.at = Some(at);
            self.arrived = 0;
        }
        self.arrived += 1;
        if self.arrived < self.expected_bars[at] || self.outcome.borrow().failure.is_some() {
            return Ok(());
        }
        let candles: Vec<Option<Candle>> = (0..self.legs.len())
            .map(|leg| {
                self.step_of(leg, at)
                    .filter(|step| step.real)
                    .map(|step| Candle { ts, ..step.candle })
            })
            .collect();
        self.step(at, &candles)
    }
}
