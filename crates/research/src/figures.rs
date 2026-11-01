//! What a backtest comes to beyond its report: the figures Lab's summaries and the app's statistics
//! both show, worked out here so they can't drift apart.

use std::collections::BTreeMap;

use persephone_core::{
    Side,
    time::{DateTime, civil_from_days},
};
use persephone_engine::{
    BacktestReport, BacktestRequest, FeeRates, LimitOutcome, SimSettings, impact_share,
};

use crate::robustness::{RoundTrip, round_trips};

const NANOS_PER_DAY: u64 = 86_400_000_000_000;

/// What the market did from the first close that could trade to the last.
#[must_use]
pub fn buy_and_hold(bars: &[(u64, f64)], from_index: usize) -> Option<f64> {
    let first = bars.get(from_index)?.1;
    let last = bars.last()?.1;
    (first > 0.0).then(|| last / first - 1.0)
}

/// Buying at the first close that could trade and holding to the end, with no costs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Benchmark {
    pub return_: f64,
    pub sharpe: Option<f64>,
    pub max_drawdown: f64,
    /// The return once a perpetual's funding has been paid on it. What holding one really made.
    pub return_after_funding: f64,
}

/// The same figures a run gets, to compare it with.
#[must_use]
pub fn benchmark(
    capital: f64,
    bars: &[(u64, f64)],
    from_index: usize,
    funding: &[(u64, f64)],
) -> Option<Benchmark> {
    let span = bars.get(from_index..)?;
    let first = span.first().map(|(_, close)| *close).filter(|c| *c > 0.0)?;
    let mut peak = 0.0_f64;
    let mut fall = 0.0_f64;
    for &(_, close) in span {
        peak = peak.max(close);
        fall = fall.max(1.0 - close / peak);
    }
    // As a share of the capital: the position's value when each falls due, times its rate.
    let paid: f64 = funding
        .iter()
        .filter_map(|&(when, rate)| {
            let at = span.partition_point(|bar| bar.0 <= when).checked_sub(1)?;
            Some(span[at].1 / first * rate)
        })
        .sum();
    let return_ = buy_and_hold(bars, from_index)?;
    Some(Benchmark {
        return_,
        return_after_funding: return_ - paid,
        sharpe: daily_sharpe(
            capital,
            span.iter()
                .map(|&(ts, close)| (ts, capital * close / first)),
        ),
        max_drawdown: fall,
    })
}

/// A run's Sharpe ratio, on the same terms as [`daily_sharpe`].
#[must_use]
pub fn sharpe(report: &BacktestReport, from_index: usize, bars: &[(u64, f64)]) -> Option<f64> {
    let points = bars.iter().zip(&report.equity).skip(from_index);
    daily_sharpe(
        report.starting_balance,
        points.map(|(&(ts, _), &equity)| (ts, equity)),
    )
}

/// The value at each UTC day's last point, after `start` for the day before the first, as a run's
/// daily figures are worked out.
fn daily_values(start: f64, points: impl Iterator<Item = (u64, f64)>) -> Vec<f64> {
    let mut days = vec![start];
    let mut last_day = None;
    for (ts, value) in points {
        let day = ts / NANOS_PER_DAY;
        if last_day == Some(day) {
            if let Some(last) = days.last_mut() {
                *last = value;
            }
        } else {
            days.push(value);
            last_day = Some(day);
        }
    }
    days
}

/// Each day's return, from one day's last value to the next's. A day starting at nothing is
/// skipped, as nothing can return on it.
#[must_use]
pub fn daily_returns(start: f64, points: impl Iterator<Item = (u64, f64)>) -> Vec<f64> {
    daily_values(start, points)
        .windows(2)
        .filter(|pair| pair[0] > 0.0)
        .map(|pair| pair[1] / pair[0] - 1.0)
        .collect()
}

/// The Sharpe ratio of daily returns, with no risk-free rate, over 365 days a year as crypto trades
/// every day. A day's value is its last bar's equity. `None` if there are too few days to say, or
/// nothing ever moved.
fn daily_sharpe(start: f64, points: impl Iterator<Item = (u64, f64)>) -> Option<f64> {
    sharpe_of_daily(&daily_returns(start, points))
}

/// The annualised Sharpe ratio of daily returns, as [`daily_sharpe`] gives it.
#[must_use]
pub fn sharpe_of_daily(returns: &[f64]) -> Option<f64> {
    if returns.len() < 2 {
        return None;
    }
    let count = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / count;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (count - 1.0);
    (variance > 0.0).then(|| mean / variance.sqrt() * 365.0_f64.sqrt())
}

/// How much an account moves with a market, day by day: the covariance of their daily returns over
/// the market's variance. 1 moves as the market does, 0 owes it nothing, and below 0 moves against
/// it. Each point is a time with the account's and the market's value then, each starting the day
/// before the first at its `start`. `None` with too few days, or a market that never moved.
#[must_use]
pub fn daily_beta(
    starts: (f64, f64),
    points: impl Iterator<Item = (u64, f64, f64)> + Clone,
) -> Option<f64> {
    let account = daily_values(starts.0, points.clone().map(|(ts, a, _)| (ts, a)));
    let market = daily_values(starts.1, points.map(|(ts, _, m)| (ts, m)));
    let pairs: Vec<(f64, f64)> = account
        .windows(2)
        .zip(market.windows(2))
        .filter(|(a, m)| a[0] > 0.0 && m[0] > 0.0)
        .map(|(a, m)| (a[1] / a[0] - 1.0, m[1] / m[0] - 1.0))
        .collect();
    if pairs.len() < 2 {
        return None;
    }
    let count = pairs.len() as f64;
    let (mean_a, mean_m) = pairs
        .iter()
        .fold((0.0, 0.0), |(a, m), (x, y)| (a + x / count, m + y / count));
    let covariance: f64 = pairs
        .iter()
        .map(|(a, m)| (a - mean_a) * (m - mean_m))
        .sum::<f64>();
    let variance: f64 = pairs.iter().map(|(_, m)| (m - mean_m).powi(2)).sum::<f64>();
    (variance > 0.0).then(|| covariance / variance)
}

/// The chance that a Sharpe ratio picked as the best of `trials` tries is really above nothing,
/// allowing for how many were tried, how much their Sharpe ratios differed, how long it ran and
/// how far its returns were from normal: the deflated Sharpe ratio of Bailey and López de Prado
/// (2014). Sharpe ratios here are per period, not annualised. `kurtosis` is 3 for normal returns.
/// `None` with fewer than two periods, or returns too wild for the formula.
#[must_use]
pub fn deflated_sharpe(
    observed: f64,
    trial_variance: f64,
    trials: u64,
    periods: usize,
    skew: f64,
    kurtosis: f64,
) -> Option<f64> {
    const EULER: f64 = std::f64::consts::EULER_GAMMA;
    if periods < 2 {
        return None;
    }
    // What the best of that many tries would be expected to show with no skill at all.
    let expected_best = if trials > 1 {
        let n = trials as f64;
        trial_variance.max(0.0).sqrt()
            * ((1.0 - EULER) * normal_quantile(1.0 - 1.0 / n)
                + EULER * normal_quantile(1.0 - 1.0 / (n * std::f64::consts::E)))
    } else {
        0.0
    };
    let spread = 1.0 - skew * observed + (kurtosis - 1.0) / 4.0 * observed * observed;
    (spread > 0.0).then(|| {
        normal_cdf((observed - expected_best) * ((periods - 1) as f64).sqrt() / spread.sqrt())
    })
}

/// The deflated Sharpe ratio of a run picked from others, from annualised Sharpe ratios as runs
/// report them and the picked run's daily returns.
///
/// `trials` is how many variants it was picked from, if more than `tried` holds: the ones tried
/// before this call count too. With fewer than two in `tried` to see how much they differ, their
/// spread is taken as what chance alone gives a Sharpe ratio over this many days, 1 / (days - 1),
/// as if none of them had any skill.
#[must_use]
pub fn deflated_sharpe_of(picked: f64, tried: &[f64], trials: u64, daily: &[f64]) -> Option<f64> {
    let per_day = 365.0_f64.sqrt();
    let count = daily.len() as f64;
    if daily.len() < 2 {
        return None;
    }
    let mean = daily.iter().sum::<f64>() / count;
    let moment = |power: i32| daily.iter().map(|r| (r - mean).powi(power)).sum::<f64>() / count;
    let variance = moment(2);
    let (skew, kurtosis) = if variance > 0.0 {
        (
            moment(3) / variance.powf(1.5),
            moment(4) / (variance * variance),
        )
    } else {
        (0.0, 3.0)
    };
    let tried: Vec<f64> = tried.iter().map(|sharpe| sharpe / per_day).collect();
    let trial_mean = tried.iter().sum::<f64>() / tried.len().max(1) as f64;
    let trial_variance = if tried.len() > 1 {
        tried.iter().map(|s| (s - trial_mean).powi(2)).sum::<f64>() / (tried.len() - 1) as f64
    } else {
        1.0 / (count - 1.0)
    };
    deflated_sharpe(
        picked / per_day,
        trial_variance,
        trials.max(tried.len() as u64).max(1),
        daily.len(),
        skew,
        kurtosis,
    )
}

/// The standard normal distribution's cumulative probability, to about 1e-7.
fn normal_cdf(x: f64) -> f64 {
    // Abramowitz and Stegun 7.1.26, for erf.
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-z * z).exp();
    if x >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

/// Where the standard normal distribution reaches `p`: Acklam's approximation, to about 1e-9.
fn normal_quantile(p: f64) -> f64 {
    const A: [f64; 6] = [
        -3.969_683_028_665_376e1,
        2.209_460_984_245_205e2,
        -2.759_285_104_469_687e2,
        1.383_577_518_672_69e2,
        -3.066_479_806_614_716e1,
        2.506_628_277_459_239,
    ];
    const B: [f64; 5] = [
        -5.447_609_879_822_406e1,
        1.615_858_368_580_409e2,
        -1.556_989_798_598_866e2,
        6.680_131_188_771_972e1,
        -1.328_068_155_288_572e1,
    ];
    const C: [f64; 6] = [
        -7.784_894_002_430_293e-3,
        -3.223_964_580_411_365e-1,
        -2.400_758_277_161_838,
        -2.549_732_539_343_734,
        4.374_664_141_464_968,
        2.938_163_982_698_783,
    ];
    const D: [f64; 4] = [
        7.784_695_709_041_462e-3,
        3.224_671_290_700_398e-1,
        2.445_134_137_142_996,
        3.754_408_661_907_416,
    ];
    let p = p.clamp(1e-300, 1.0 - 1e-16);
    let tail = |q: f64| {
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    };
    if p < 0.024_25 {
        tail((-2.0 * p.ln()).sqrt())
    } else if p > 1.0 - 0.024_25 {
        -tail((-2.0 * (1.0 - p).ln()).sqrt())
    } else {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    }
}

/// A run's daily returns from the first close that could trade, as its Sharpe ratio is worked out.
#[must_use]
pub fn run_daily_returns(report: &BacktestReport, from_index: usize, times: &[u64]) -> Vec<f64> {
    daily_returns(
        report.starting_balance,
        times
            .iter()
            .zip(&report.equity)
            .skip(from_index)
            .map(|(&ts, &equity)| (ts, equity)),
    )
}

/// A run's beta against its own market, held from the first close that could trade.
#[must_use]
pub fn beta(report: &BacktestReport, from_index: usize, bars: &[(u64, f64)]) -> Option<f64> {
    let first = bars.get(from_index)?.1;
    daily_beta(
        (report.starting_balance, first),
        bars.iter()
            .zip(&report.equity)
            .skip(from_index)
            .map(|(&(ts, close), &equity)| (ts, equity, close)),
    )
}

/// The share of the span's bars spent in a position.
#[must_use]
pub fn exposure(report: &BacktestReport, from_index: usize) -> f64 {
    let bars = report.equity.len().saturating_sub(from_index);
    if bars == 0 {
        return 0.0;
    }
    let mut position = 0.0;
    let mut fills = report.fills.iter().peekable();
    let mut held = 0;
    for bar in from_index..report.equity.len() {
        while let Some(fill) = fills.next_if(|fill| fill.bar < bar) {
            position += if fill.side == Side::Buy {
                fill.qty
            } else {
                -fill.qty
            };
        }
        // Rounding can leave dust where a position closed.
        if f64::abs(position) > 1e-12 {
            held += 1;
        }
    }
    f64::from(held) / bars as f64
}

/// What each UTC year added, as a fraction of the starting capital, so they add up to the net return.
#[must_use]
pub fn by_year(
    report: &BacktestReport,
    from_index: usize,
    bars: &[(u64, f64)],
) -> BTreeMap<i64, f64> {
    let capital = report.starting_balance;
    let mut years = BTreeMap::new();
    let mut before = capital;
    for (&(ts, _), &equity) in bars.iter().zip(&report.equity).skip(from_index) {
        let (year, _, _) = civil_from_days((ts / NANOS_PER_DAY) as i64);
        *years.entry(year).or_insert(0.0) += (equity - before) / capital;
        before = equity;
    }
    years
}

/// What each UTC year returned on what the account held going into it, so they compound to the net
/// return rather than add up to it. A year that started with nothing left reads `None`.
#[must_use]
pub fn by_year_compounded(
    report: &BacktestReport,
    from_index: usize,
    bars: &[(u64, f64)],
) -> BTreeMap<i64, Option<f64>> {
    let mut years: BTreeMap<i64, (f64, f64)> = BTreeMap::new();
    let mut before = report.starting_balance;
    for (&(ts, _), &equity) in bars.iter().zip(&report.equity).skip(from_index) {
        let (year, _, _) = civil_from_days((ts / NANOS_PER_DAY) as i64);
        years.entry(year).or_insert((before, before)).1 = equity;
        before = equity;
    }
    years
        .into_iter()
        .map(|(year, (start, end))| (year, (start > 0.0).then(|| end / start - 1.0)))
        .collect()
}

/// A share of chance runs that did as well, made fair for having picked the best of `variants`
/// tries: the chance that at least one of that many tries of no worth would have done as well. With
/// one try it's the share itself.
#[must_use]
pub fn adjusted_for_variants(share: f64, variants: u64) -> f64 {
    let share = share.clamp(0.0, 1.0);
    1.0 - (1.0 - share).powf(variants.max(1) as f64)
}

/// Winnings over losses across a run's trades, if it has any losses.
#[must_use]
pub fn profit_factor(report: &BacktestReport) -> Option<f64> {
    let trades = round_trips(report);
    let gains: f64 = trades.iter().map(|t| t.profit.max(0.0)).sum();
    let losses: f64 = trades.iter().map(|t| (-t.profit).max(0.0)).sum();
    (losses > 0.0).then(|| gains / losses)
}

/// Said when a logic tree's rules can't be true for at least 5% of the span, which trades or not is
/// easy to miss, as a rule that isn't ready is just false. Less is usual and not worth saying.
#[must_use]
pub fn look_back_warning(look_back: usize, before: usize, bars: usize) -> Option<String> {
    let unready = look_back.saturating_sub(before);
    let span = bars.saturating_sub(before);
    (unready > 0 && unready * 20 >= span).then(|| {
        format!(
            "A look-back here needs about {look_back} bars, with {before} before the span, so rules on it can't be true for about its first {unready} bars of {span}."
        )
    })
}

/// What costs did to a run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Friction {
    /// The return with no fees, slippage or spread.
    pub gross_return: f64,
    /// Fees, slippage, spread, impact, funding and borrowing, in the quote currency.
    pub costs_paid: f64,
    /// The fees on orders that rested, below zero if they earned a rebate, and on those that took.
    pub maker_fees: f64,
    pub taker_fees: f64,
    /// `costs_paid` by kind.
    pub costs: CostBreakdown,
    /// The cost per fill that would bring the costless run to nothing, in basis points of the value
    /// traded. `None` if it traded nothing.
    pub break_even_bp: Option<f64>,
}

/// `report`'s friction, given the same run with no costs, which is `report` itself if it had none.
#[must_use]
pub fn friction(
    report: &BacktestReport,
    free: &BacktestReport,
    sim: &SimSettings,
    price_precision: u8,
    volumes: &[f64],
) -> Friction {
    let costs = cost_breakdown(report, sim, price_precision, volumes);
    Friction {
        maker_fees: costs.maker_fees,
        taker_fees: costs.taker_fees + 0.0,
        gross_return: free
            .equity
            .last()
            .map_or(0.0, |last| last / free.starting_balance - 1.0),
        costs_paid: costs.total(),
        costs,
        break_even_bp: break_even_bp(free),
    }
}

/// Whether a backtest has anything to pay: fees, slippage or spread.
#[must_use]
pub fn costs_something(request: &BacktestRequest) -> bool {
    request.fees.maker != 0.0
        || request.fees.taker != 0.0
        || sim_costs_something(&request.sim)
        || request
            .funding
            .as_ref()
            .is_some_and(|rates| !rates.is_empty())
}

/// Whether the settings themselves charge anything.
#[must_use]
pub fn sim_costs_something(sim: &SimSettings) -> bool {
    sim.fee_per_order.is_some()
        || sim.slippage_ticks > 0
        || sim.slippage_percent > 0.0
        || sim.spread_ticks > 0
        || sim.impact_percent > 0.0
        || sim.range_slippage_percent > 0.0
        || sim.borrow_percent_per_year.is_some()
        || sim.cash_interest_percent_per_year.is_some()
}

/// The same settings charging nothing.
#[must_use]
pub fn free_sim(sim: &SimSettings) -> SimSettings {
    SimSettings {
        fee_per_order: None,
        slippage_ticks: 0,
        slippage_percent: 0.0,
        spread_ticks: 0,
        impact_percent: 0.0,
        range_slippage_percent: 0.0,
        borrow_percent_per_year: None,
        cash_interest_percent_per_year: None,
        ..*sim
    }
}

/// The same backtest with nothing to pay.
#[must_use]
pub fn without_costs(request: &BacktestRequest) -> BacktestRequest {
    let mut free = request.clone();
    free.fees = FeeRates::ZERO;
    free.sim = free_sim(&request.sim);
    free.funding = None;
    free
}

/// What a run paid, by kind, in the quote currency. They add up to `total`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CostBreakdown {
    /// Commission, on resting orders and on those that took. Below zero for a rebate.
    pub maker_fees: f64,
    pub taker_fees: f64,
    /// The set slippage: so many ticks, and a percentage of the price.
    pub slippage: f64,
    /// Half the assumed spread, crossed on each order that took.
    pub spread: f64,
    /// Slippage from a bar's range, the volatility setting.
    pub range_slippage: f64,
    /// What an order's size moved the price, against its bar's volume.
    pub impact: f64,
    pub funding: f64,
    pub borrow: f64,
    /// On cash below zero, in a portfolio.
    pub interest: f64,
    /// Prices are rounded to the market's tick, so the parts of a fill's price don't quite add up to
    /// how far it moved. This is the difference, so the parts add up to the total.
    pub rounding: f64,
}

impl CostBreakdown {
    #[must_use]
    pub fn fees(&self) -> f64 {
        self.maker_fees + self.taker_fees
    }

    /// What the price moved against orders that took: slippage, spread, range and impact.
    #[must_use]
    pub fn price_moved(&self) -> f64 {
        self.slippage + self.spread + self.range_slippage + self.impact + self.rounding
    }

    #[must_use]
    pub fn total(&self) -> f64 {
        self.fees() + self.price_moved() + self.funding + self.borrow + self.interest
    }

    /// The two added together, as for a portfolio's legs.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self {
            maker_fees: self.maker_fees + other.maker_fees,
            taker_fees: self.taker_fees + other.taker_fees,
            slippage: self.slippage + other.slippage,
            spread: self.spread + other.spread,
            range_slippage: self.range_slippage + other.range_slippage,
            impact: self.impact + other.impact,
            funding: self.funding + other.funding,
            borrow: self.borrow + other.borrow,
            interest: self.interest + other.interest,
            rounding: self.rounding + other.rounding,
        }
    }
}

/// All a run paid, by kind: fees, funding and borrowing as they were charged, and what slippage,
/// spread, range and impact took from each order that didn't rest, worked back from its price as the
/// fill model moved it. A market's own quotes bring their own spread, which isn't known here, so the
/// assumed one is counted as if it was charged.
#[must_use]
pub fn cost_breakdown(
    report: &BacktestReport,
    sim: &SimSettings,
    price_precision: u8,
    volumes: &[f64],
) -> CostBreakdown {
    let tick = 10_f64.powi(-i32::from(price_precision));
    let mut costs = CostBreakdown {
        funding: report.funding_paid,
        borrow: report.borrow_paid,
        ..CostBreakdown::default()
    };
    for fill in &report.fills {
        if fill.maker {
            costs.maker_fees += fill.fee;
            continue;
        }
        costs.taker_fees += fill.fee;
        let (spread, sign) = if fill.side == Side::Buy {
            (sim.spread_ticks.div_ceil(2), 1.0)
        } else {
            (sim.spread_ticks / 2, -1.0)
        };
        let volume = volumes.get(fill.bar).copied().unwrap_or(0.0);
        let impact = impact_share(sim.impact_percent, sim.impact_exponent, fill.qty, volume);
        let share = sim.slippage_percent / 100.0 + impact;
        let ticks = f64::from(sim.slippage_ticks + spread) * tick;
        // The price the market met, before the fill model moved it.
        let met = (fill.price - sign * (ticks + fill.range_slip)) / (1.0 + sign * share);
        let moved = fill.qty * (fill.price - met) * sign;
        let slippage =
            fill.qty * (f64::from(sim.slippage_ticks) * tick + met * sim.slippage_percent / 100.0);
        let spread = fill.qty * f64::from(spread) * tick;
        let range = fill.qty * fill.range_slip;
        let impact = fill.qty * met * impact;
        costs.slippage += slippage;
        costs.spread += spread;
        costs.range_slippage += range;
        costs.impact += impact;
        costs.rounding += moved - (slippage + spread + range + impact);
    }
    // No -0.0 for a run with nothing of a kind.
    costs.maker_fees += 0.0;
    costs
}

/// All a run paid, as [`cost_breakdown`] adds it up.
#[must_use]
pub fn costs_paid(
    report: &BacktestReport,
    sim: &SimSettings,
    price_precision: u8,
    volumes: &[f64],
) -> f64 {
    cost_breakdown(report, sim, price_precision, volumes).total()
}

/// What a backtest with no costs made, per fill, as basis points of the value traded.
fn break_even_bp(free: &BacktestReport) -> Option<f64> {
    let traded: f64 = free.fills.iter().map(|fill| fill.qty * fill.price).sum();
    (traded > 0.0).then(|| free.net_pnl() / traded * 10_000.0)
}

/// What the account would be worth holding the market with `share` of the capital since `first`: the
/// same size of position a strategy takes, so the two can be compared on one scale.
#[must_use]
pub fn market_equity(capital: f64, share: f64, first: f64, close: f64) -> f64 {
    capital * (1.0 + share * (close / first - 1.0))
}

/// What trades made by the UTC hour and the weekday (Monday first) they started, adding up to all they
/// made.
#[must_use]
pub fn by_time(trades: &[RoundTrip]) -> (Vec<f64>, Vec<f64>) {
    let mut hours = vec![0.0; 24];
    let mut weekdays = vec![0.0; 7];
    for trade in trades {
        let at = DateTime::from_nanos(trade.entry_ts);
        hours[at.hour as usize % 24] += trade.profit;
        // 1970-01-01 was a Thursday.
        let day = trade.entry_ts / NANOS_PER_DAY + 3;
        weekdays[(day % 7) as usize] += trade.profit;
    }
    (hours, weekdays)
}

/// How many bars a chart of a spread has, in the app and from Lab alike.
pub const HISTOGRAM_BINS: usize = 40;

/// How values are spread: `counts[i]` of them lie in the `i`th of equal steps from `lo` to `hi`.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub lo: f64,
    pub hi: f64,
    pub counts: Vec<usize>,
}

impl Histogram {
    /// Over `bins` steps, wide enough to reach `also`, so a mark there is always on the chart.
    #[must_use]
    pub fn of(values: &[f64], bins: usize, also: f64) -> Self {
        let bins = bins.max(1);
        let (lo, hi) = values
            .iter()
            .chain(std::iter::once(&also))
            .fold((f64::MAX, f64::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
        // All the same still needs some width to draw.
        let (lo, hi) = if hi > lo {
            (lo, hi)
        } else {
            (lo - 0.005, hi + 0.005)
        };
        let mut counts = vec![0; bins];
        for value in values {
            let at = ((value - lo) / (hi - lo) * bins as f64) as usize;
            counts[at.min(bins - 1)] += 1;
        }
        Self { lo, hi, counts }
    }

    /// Where `value` lies across it, from 0 to 1.
    #[must_use]
    pub fn share_of(&self, value: f64) -> f64 {
        ((value - self.lo) / (self.hi - self.lo)).clamp(0.0, 1.0)
    }
}

/// How limit orders that filled did against those that missed: whether the market fills the bad ones.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LimitSummary {
    pub filled: usize,
    pub missed: usize,
    /// Fills that went less than a hundredth of a percent past their price: the ones most likely not
    /// to have filled at all, behind others in the queue.
    pub barely_filled: usize,
    /// How far the market moved each order's way, on average, as percentages, from the close it went
    /// out at. `None` without any to go on.
    pub filled_move_1: Option<f64>,
    pub missed_move_1: Option<f64>,
    pub filled_move_24: Option<f64>,
    pub missed_move_24: Option<f64>,
}

impl LimitSummary {
    #[must_use]
    pub fn of(report: &BacktestReport) -> Self {
        let log = &report.limit_log;
        let is_fill = |outcome: LimitOutcome| outcome == LimitOutcome::Filled;
        let is_miss = |outcome: LimitOutcome| {
            matches!(outcome, LimitOutcome::Lapsed | LimitOutcome::Replaced)
        };
        let average = |wanted: &dyn Fn(LimitOutcome) -> bool, bars_on: bool| {
            let moves: Vec<f64> = log
                .iter()
                .filter(|row| wanted(row.outcome))
                .filter_map(|row| {
                    if bars_on {
                        row.move_24_percent
                    } else {
                        row.move_1_percent
                    }
                })
                .collect();
            (!moves.is_empty()).then(|| moves.iter().sum::<f64>() / moves.len() as f64)
        };
        Self {
            filled: log.iter().filter(|row| is_fill(row.outcome)).count(),
            missed: log.iter().filter(|row| is_miss(row.outcome)).count(),
            barely_filled: log
                .iter()
                .filter(|row| is_fill(row.outcome) && row.reached_percent < 0.01)
                .count(),
            filled_move_1: average(&is_fill, false),
            missed_move_1: average(&is_miss, false),
            filled_move_24: average(&is_fill, true),
            missed_move_24: average(&is_miss, true),
        }
    }

    /// Said when orders that missed did clearly better than those that filled, whatever the fill rate:
    /// a result built on the rest is being paid to take the bad side.
    #[must_use]
    pub fn warning(&self) -> Option<String> {
        let (filled, missed) = (self.filled_move_1?, self.missed_move_1?);
        (self.filled >= 20 && self.missed >= 20 && missed - filled >= 0.05).then(|| {
            format!(
                "Limit orders that missed moved {missed:+.2}% their way a bar later, and those that filled {filled:+.2}%. Fills are the worse side of the bargain here, so test with `through_percent`, and see `get_limit_orders`."
            )
        })
    }
}

/// A finished run's figures, to show beside it.
#[derive(Clone, Debug, PartialEq)]
pub struct RunFigures {
    /// Flat to flat, so a reversal is two. Not fills.
    pub trades: usize,
    pub win_rate: Option<f64>,
    pub sharpe: Option<f64>,
    pub profit_factor: Option<f64>,
    pub exposure: f64,
    pub by_year: BTreeMap<i64, f64>,
    pub by_year_compounded: BTreeMap<i64, Option<f64>>,
    /// How much it moved with its market, day by day.
    pub beta: Option<f64>,
    pub benchmark: Option<Benchmark>,
    /// The share of limit orders that filled, if any went out.
    pub fill_rate: Option<f64>,
}

impl RunFigures {
    #[must_use]
    pub fn of(
        report: &BacktestReport,
        from_index: usize,
        bars: &[(u64, f64)],
        funding: &[(u64, f64)],
    ) -> Self {
        let trades = round_trips(report);
        let winners = trades.iter().filter(|t| t.profit > 0.0).count();
        Self {
            trades: trades.len(),
            win_rate: (!trades.is_empty()).then(|| winners as f64 / trades.len() as f64),
            sharpe: sharpe(report, from_index, bars),
            profit_factor: profit_factor(report),
            exposure: exposure(report, from_index),
            by_year: by_year(report, from_index, bars),
            by_year_compounded: by_year_compounded(report, from_index, bars),
            beta: beta(report, from_index, bars),
            benchmark: benchmark(report.starting_balance, bars, from_index, funding),
            fill_rate: (report.limit_orders > 0)
                .then(|| report.limits_filled as f64 / report.limit_orders as f64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn costs_split_into_what_each_took_and_add_up_to_the_total() {
        let sim = SimSettings {
            slippage_ticks: 2,
            spread_ticks: 3,
            slippage_percent: 0.5,
            impact_percent: 1.0,
            impact_exponent: 1.0,
            ..SimSettings::default()
        };
        // 10 bought on a bar of 100 traded, where the market met 100: 0.5% and 0.1% impact on that,
        // 2 ticks of slippage and 2 of half a 3-tick spread, and 0.30 of range.
        let met: f64 = 100.0;
        let price = met * (1.0 + 0.005 + 0.001) + 4.0 * 0.01 + 0.3;
        let fill = |side, price: f64, maker| persephone_engine::Fill {
            ts: 0,
            bar: 0,
            side,
            price,
            qty: 10.0,
            fee: 1.5,
            maker,
            range_slip: if maker { 0.0 } else { 0.3 },
        };
        let report = BacktestReport {
            fills: vec![fill(Side::Buy, price, false), fill(Side::Sell, 101.0, true)],
            fees: 3.0,
            funding_paid: 0.7,
            borrow_paid: 0.2,
            ..BacktestReport::default()
        };
        let costs = cost_breakdown(&report, &sim, 2, &[100.0]);
        let near = |a: f64, b: f64| (a - b).abs() < 1e-9;
        assert!(near(costs.slippage, 10.0 * (0.02 + 0.5)), "{costs:?}");
        assert!(near(costs.spread, 10.0 * 0.02), "{costs:?}");
        assert!(near(costs.range_slippage, 3.0), "{costs:?}");
        assert!(near(costs.impact, 10.0 * met * 0.001), "{costs:?}");
        assert!(near(costs.rounding, 0.0), "{costs:?}");
        assert!(near(costs.maker_fees, 1.5) && near(costs.taker_fees, 1.5));
        assert!(near(costs.funding, 0.7) && near(costs.borrow, 0.2));
        let total = 3.0 + 5.2 + 0.2 + 3.0 + 1.0 + 0.7 + 0.2;
        assert!(near(costs.total(), total), "{costs:?}");
        assert!(near(costs_paid(&report, &sim, 2, &[100.0]), total));
    }

    #[test]
    fn beta_is_how_far_an_account_moves_with_its_market() {
        // A market up and down by turns over days, and accounts at twice it, against it, and flat.
        let day = NANOS_PER_DAY;
        let closes = [100.0, 104.0, 99.0, 103.0, 101.0, 106.0];
        let bars: Vec<(u64, f64)> = closes
            .iter()
            .enumerate()
            .map(|(at, close)| (at as u64 * day, *close))
            .collect();
        let with = |times: f64| -> Vec<f64> {
            let mut value = 1_000.0;
            let mut line = vec![value];
            for pair in closes.windows(2) {
                value *= 1.0 + times * (pair[1] / pair[0] - 1.0);
                line.push(value);
            }
            line
        };
        let of = |equity: Vec<f64>| {
            beta(
                &BacktestReport {
                    starting_balance: 1_000.0,
                    equity,
                    ..BacktestReport::default()
                },
                0,
                &bars,
            )
            .unwrap()
        };
        assert!((of(with(2.0)) - 2.0).abs() < 1e-9);
        assert!((of(with(-0.5)) + 0.5).abs() < 1e-9);
        assert!(of(vec![1_000.0; 6]).abs() < 1e-12);
        // A market that never moves says nothing.
        let still: Vec<(u64, f64)> = bars.iter().map(|(ts, _)| (*ts, 100.0)).collect();
        assert_eq!(
            beta(
                &BacktestReport {
                    starting_balance: 1_000.0,
                    equity: with(1.0),
                    ..BacktestReport::default()
                },
                0,
                &still
            ),
            None
        );
    }

    #[test]
    fn the_deflated_sharpe_ratio_matches_the_papers_example() {
        // Bailey and López de Prado's: 2.5 a year over 1,250 days, the best of 100 whose Sharpe
        // ratios varied by 0.5 a year, with skew -3 and kurtosis 10. They give 0.9004.
        let days = 250.0_f64;
        let found = deflated_sharpe(2.5 / days.sqrt(), 0.5 / days, 100, 1_250, -3.0, 10.0).unwrap();
        assert!((found - 0.900_397).abs() < 1e-4, "{found}");
        // One a year over 1,000 days, best of 11 varying by 0.25, normal returns: 0.6226.
        let year = 365.0_f64;
        let found = deflated_sharpe(1.0 / year.sqrt(), 0.25 / year, 11, 1_000, 0.0, 3.0).unwrap();
        assert!((found - 0.622_593).abs() < 1e-4, "{found}");
        // More tries take more away.
        let fewer = deflated_sharpe(0.1, 0.002, 10, 500, 0.0, 3.0).unwrap();
        let more = deflated_sharpe(0.1, 0.002, 1_000, 500, 0.0, 3.0).unwrap();
        assert!(more < fewer);
        assert!((normal_quantile(0.975) - 1.959_964).abs() < 1e-6);
        assert!((normal_cdf(1.959_964) - 0.975).abs() < 1e-6);
    }

    #[test]
    fn variants_tried_before_count_toward_the_deflated_sharpe_ratio() {
        // A year of daily returns, a little skewed, from a fixed pattern.
        let daily: Vec<f64> = (0..365)
            .map(|day| 0.001 + 0.01 * ((day * 37 % 101) as f64 / 101.0 - 0.5))
            .collect();
        let count = daily.len() as f64;
        let mean = daily.iter().sum::<f64>() / count;
        let moment = |p: i32| daily.iter().map(|r| (r - mean).powi(p)).sum::<f64>() / count;
        let (skew, kurtosis) = (
            moment(3) / moment(2).powf(1.5),
            moment(4) / moment(2).powi(2),
        );
        let per_day = 365.0_f64.sqrt();
        let tried = [1.5, 1.0, 0.5];
        let mean_tried = tried.iter().sum::<f64>() / 3.0 / per_day;
        let variance = tried
            .iter()
            .map(|s| (s / per_day - mean_tried).powi(2))
            .sum::<f64>()
            / 2.0;
        let by_hand =
            |trials| deflated_sharpe(1.5 / per_day, variance, trials, 365, skew, kurtosis).unwrap();
        let own = deflated_sharpe_of(1.5, &tried, 0, &daily).unwrap();
        assert!((own - by_hand(3)).abs() < 1e-12);
        assert_eq!(
            deflated_sharpe_of(1.5, &tried, 2, &daily),
            Some(own),
            "never fewer"
        );
        let after_135 = deflated_sharpe_of(1.5, &tried, 135, &daily).unwrap();
        assert!((after_135 - by_hand(135)).abs() < 1e-12);
        assert!(after_135 < own, "{after_135} against {own}");

        // One run alone, said to be the best of many: chance's spread over a year of days.
        let alone = deflated_sharpe_of(1.5, &[1.5], 135, &daily).unwrap();
        let chance = deflated_sharpe(1.5 / per_day, 1.0 / 364.0, 135, 365, skew, kurtosis).unwrap();
        assert!((alone - chance).abs() < 1e-12);
        let one = deflated_sharpe_of(1.5, &[1.5], 1, &daily).unwrap();
        assert!(alone < one);
    }

    #[test]
    fn a_share_matching_grows_with_the_variants_tried() {
        assert!((adjusted_for_variants(0.03, 1) - 0.03).abs() < 1e-12);
        assert!((adjusted_for_variants(0.03, 0) - 0.03).abs() < 1e-12);
        // 1 - 0.97^11: the agent's 3% after its 11 look-backs.
        assert!((adjusted_for_variants(0.03, 11) - (1.0 - 0.97_f64.powi(11))).abs() < 1e-12);
        assert!(adjusted_for_variants(0.03, 11) > 0.28);
        assert_eq!(adjusted_for_variants(1.0, 5), 1.0);
    }

    #[test]
    fn years_compound_to_the_whole_return_where_by_year_adds_up_to_it() {
        // 2023-12-31 and 2024-01-01, 2024-12-31 and 2025-01-01, at noon.
        let days = [19_722_u64, 19_723, 20_088, 20_089];
        let bars: Vec<(u64, f64)> = days
            .iter()
            .map(|day| (day * NANOS_PER_DAY + NANOS_PER_DAY / 2, 1.0))
            .collect();
        let report = BacktestReport {
            starting_balance: 100.0,
            equity: vec![110.0, 110.0, 132.0, 66.0],
            ..BacktestReport::default()
        };
        let compounded = by_year_compounded(&report, 0, &bars);
        let close = |a: Option<f64>, b: f64| (a.unwrap() - b).abs() < 1e-12;
        assert!(close(compounded[&2023], 0.1));
        assert!(close(compounded[&2024], 0.2), "{compounded:?}");
        assert!(close(compounded[&2025], -0.5));
        let whole: f64 = compounded.values().map(|r| 1.0 + r.unwrap()).product();
        assert!((whole - 0.66).abs() < 1e-12);
        // Where by_year is shares of the capital: 2024 made 22 of 100.
        assert!((by_year(&report, 0, &bars)[&2024] - 0.22).abs() < 1e-12);
    }

    #[test]
    fn by_hour_and_weekday_add_up_to_all_that_was_made() {
        let trade = |entry_ts, profit| RoundTrip {
            long: true,
            qty: 1.0,
            entry_ts,
            entry_bar: 0,
            entry_price: 1.0,
            exit_ts: entry_ts,
            exit_bar: 0,
            exit_price: 1.0,
            profit,
            fees: 0.0,
            open: false,
        };
        // 2024-01-01 09:30 was a Monday, and 2024-01-06 23:00 a Saturday.
        let monday = 1_704_101_400_000_000_000;
        let saturday = 1_704_582_000_000_000_000;
        let (hours, weekdays) = by_time(&[
            trade(monday, 5.0),
            trade(saturday, -2.0),
            trade(monday, 1.0),
        ]);
        assert_eq!(hours[9], 6.0);
        assert_eq!(hours[23], -2.0);
        assert_eq!(weekdays[0], 6.0);
        assert_eq!(weekdays[5], -2.0);
        assert_eq!(hours.iter().sum::<f64>(), 4.0);
        assert_eq!(weekdays.iter().sum::<f64>(), 4.0);
    }

    #[test]
    fn a_histogram_counts_everything_even_at_its_top_edge() {
        let values = [-0.1, 0.0, 0.0, 0.05, 0.1];
        let found = Histogram::of(&values, 4, 0.0);
        assert_eq!(found.counts.iter().sum::<usize>(), values.len());
        assert_eq!(
            *found.counts.last().unwrap(),
            2,
            "the top one is in the last"
        );
        assert_eq!((found.lo, found.hi), (-0.1, 0.1));
        assert!((found.share_of(0.0) - 0.5).abs() < 1e-12);

        // A mark beyond every value widens it.
        let found = Histogram::of(&values, 4, 0.3);
        assert_eq!(found.hi, 0.3);
        assert_eq!(found.share_of(0.3), 1.0);

        // All the same doesn't divide by nothing.
        let found = Histogram::of(&[0.2, 0.2], 4, 0.2);
        assert_eq!(found.counts.iter().sum::<usize>(), 2);
        assert!(found.hi > found.lo);
    }

    #[test]
    fn holding_a_perpetual_pays_its_funding_at_what_it_was_worth() {
        const DAY: u64 = 86_400_000_000_000;
        let bars = [(0, 100.0), (DAY, 200.0), (2 * DAY, 200.0)];
        // Half a percent while at 100, and 1% while at twice that, on the same position.
        let funding = [(10, 0.005), (DAY + 10, 0.01)];
        let held = benchmark(1_000.0, &bars, 0, &funding).unwrap();
        assert!((held.return_ - 1.0).abs() < 1e-12);
        assert!((held.return_after_funding - (1.0 - 0.005 - 0.02)).abs() < 1e-12);
        assert_eq!(
            benchmark(1_000.0, &bars, 0, &[])
                .unwrap()
                .return_after_funding,
            1.0
        );
    }

    #[test]
    fn the_market_line_holds_the_same_size_as_the_strategy() {
        // A tenth of 1,000 in a market up from 100 to 150 makes 50.
        assert!((market_equity(1_000.0, 0.1, 100.0, 150.0) - 1_050.0).abs() < 1e-9);
        assert!((market_equity(1_000.0, 1.0, 100.0, 50.0) - 500.0).abs() < 1e-9);
    }
}
