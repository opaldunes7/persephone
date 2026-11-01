//! A chart's backtest settings, saved with the layout: the stock ones every market shares, and
//! what the engine makes of them.

use std::{fmt, sync::Arc};

use persephone_core::{
    UnixNanos,
    time::{NANOS_PER_DAY, NANOS_PER_MINUTE},
};
use persephone_data::import::Zone;
use persephone_engine::{Direction, FeeRates, LimitOrders, Session, SimSettings, TradingRules};
use serde::{Deserialize, Serialize};

use crate::Field;

/// What a chart's backtests assume beyond its strategy and bars. Each stock section has its
/// own switch, and `stock` switches them all at once; a section that's off leaves things as
/// they are without it (the market's own fee, no slippage, any time, both ways).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub stock: bool,
    /// Whether the panel shows the stock settings or keeps them folded away.
    pub stock_open: bool,
    /// Starting cash, in the market's quote currency. Paper trading starts with it too.
    pub capital: f64,
    pub commission: Section<Commission>,
    /// Ticks every market order fills worse.
    pub slippage: Section<u32>,
    /// Slippage as a percentage of the price, on top of any in ticks.
    pub slippage_percent: Section<f64>,
    /// Sending orders as limits rather than at the market.
    pub limit: Section<LimitChoice>,
    /// A stop and a target, as percentages from the entry, resting while a position's held.
    pub stop_loss_percent: Section<f64>,
    pub take_profit_percent: Section<f64>,
    /// What borrowing costs a short, as a percentage of its value a year.
    pub borrow_percent_per_year: Section<f64>,
    /// How far a market order fills worse, as a percentage, when it's all of its bar's volume.
    pub impact_percent: Section<f64>,
    /// How steeply that grows with an order's share of its bar's volume: 0.5 is the square root.
    pub impact_exponent: f64,
    /// An order over this percentage of its bar's volume, or fewer than this percentage of limits
    /// filling, gets a warning.
    pub volume_warning_percent: f64,
    pub fill_warning_percent: f64,
    /// What cash below zero costs in a portfolio, as a percentage a year.
    pub cash_interest_percent_per_year: Section<f64>,
    /// How many bars before a span strategies warm up on.
    pub warm_up_bars: u32,
    /// How much of a span Keep unseen picks values on, as a percentage.
    pub hold_out_percent: f64,
    /// How many times longer than its unseen stretch a walk-forward window picks on.
    pub walk_ratio: f64,
    /// The most of a bar's volume an order can take, as a percentage.
    pub max_participation_percent: Section<f64>,
    /// What a taker pays on top as a percentage of a bar's range, more when that bar's unusually wide.
    pub range_slippage_percent: Section<f64>,
    /// How many bars back the usual range is judged from.
    pub range_look_back: u32,
    /// A spread assumed for bar data, in ticks.
    pub spread: Section<u32>,
    /// Milliseconds orders take to reach the venue.
    pub latency: Section<u32>,
    /// How much longer orders take on average, drawn for each, and which draws those are.
    pub latency_jitter: Section<u32>,
    pub latency_seed: u32,
    /// The markets the Portfolio test trades together.
    pub portfolio: crate::portfolio::PortfolioSetup,
    pub leverage: Section<f64>,
    pub direction: Section<OneWay>,
    /// Trade the opposite of what the strategy says.
    pub invert: bool,
    /// Size positions from the account as it is now, rather than as it started.
    pub compound: bool,
    pub session: Section<SessionRule>,
    pub precision: Precision,
    /// When orders fill on the bars.
    #[serde(default)]
    pub fill_timing: FillTiming,
    /// How many windows a walk-forward makes.
    pub windows: u32,
    /// How many times Random entries places the trades.
    pub entries: u32,
    /// Plugins added to the backtest, by name. They're only listed for now: no kind of
    /// backtest plugin exists yet.
    pub plugins: Vec<String>,
    /// Which test the panel shows, and what the two beyond the backtest itself are set to.
    pub test: TestKind,
    /// How many times Monte Carlo draws the trades.
    pub runs: u32,
    /// The fall from a peak Monte Carlo counts as ruin, in percent.
    pub ruin: f64,
    /// How many stretches of the market Random blocks backtests.
    pub blocks: u32,
    pub block_length: BlockLength,
    pub optimize: OptimizeSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            stock: true,
            stock_open: true,
            capital: 100_000.0,
            commission: Section::off(Commission::Percent(0.1)),
            slippage: Section::off(1),
            slippage_percent: Section::off(0.01),
            limit: Section::off(LimitChoice {
                offset_percent: 0.05,
                expires_bars: 3,
                through_percent: 0.0,
            }),
            stop_loss_percent: Section::off(2.0),
            take_profit_percent: Section::off(4.0),
            borrow_percent_per_year: Section::off(10.0),
            impact_percent: Section::off(1.0),
            impact_exponent: 0.5,
            volume_warning_percent: 10.0,
            fill_warning_percent: 50.0,
            cash_interest_percent_per_year: Section::off(5.0),
            warm_up_bars: crate::WARM_UP_BARS as u32,
            hold_out_percent: crate::optimize::PICKED_ON * 100.0,
            walk_ratio: crate::walk_forward::RATIO,
            max_participation_percent: Section::off(5.0),
            range_slippage_percent: Section::off(1.0),
            range_look_back: 100,
            spread: Section::off(2),
            latency: Section::off(100),
            latency_jitter: Section::off(50),
            latency_seed: 0,
            portfolio: crate::portfolio::PortfolioSetup::default(),
            leverage: Section::off(2.0),
            direction: Section::off(OneWay::LongOnly),
            invert: false,
            compound: false,
            session: Section::off(SessionRule::default()),
            precision: Precision::Standard,
            fill_timing: FillTiming::Close,
            windows: 5,
            entries: 200,
            plugins: Vec::new(),
            test: TestKind::Backtest,
            runs: 1_000,
            ruin: 50.0,
            blocks: 50,
            block_length: BlockLength::Month,
            optimize: OptimizeSettings::default(),
        }
    }
}

/// What the panel tests a strategy with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TestKind {
    /// One run through the span, which can be played back.
    #[default]
    Backtest,
    /// The backtest's trades, drawn at random many times.
    MonteCarlo,
    /// Backtests of random stretches of the span.
    RandomBlocks,
    /// Backtests across a range of a strategy's parameters.
    Optimize,
    /// Optimizing on a stretch and trying the best on the unseen one after it, sliding along.
    WalkForward,
    /// The backtest's holds placed at random times, to compare it with chance.
    RandomEntries,
    /// Several markets on one account.
    Portfolio,
}

/// What the Optimize test varies and how it scores the results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OptimizeSettings {
    /// The variable to change, and optionally a second one with it.
    pub variables: [Option<Variable>; 2],
    pub score: ScoreBy,
    /// Pick values on the span's first 70%, and check the best on the rest.
    pub hold_out: bool,
}

impl Default for OptimizeSettings {
    fn default() -> Self {
        Self {
            variables: [None, None],
            score: ScoreBy::Backtest,
            hold_out: true,
        }
    }
}

/// A strategy parameter, by name, and the values to try it at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

/// What makes one value better than another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScoreBy {
    /// The return of one backtest.
    #[default]
    Backtest,
    /// The low end of the returns its trades could have given.
    MonteCarlo,
    /// The median return across random stretches of the span.
    RandomBlocks,
}

impl ScoreBy {
    pub const ALL: [Self; 3] = [Self::Backtest, Self::MonteCarlo, Self::RandomBlocks];
}

impl fmt::Display for ScoreBy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Backtest => "Backtest return",
            Self::MonteCarlo => "Monte Carlo",
            Self::RandomBlocks => "Random blocks",
        })
    }
}

/// How long each of Random blocks' stretches is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BlockLength {
    Week,
    #[default]
    Month,
    Quarter,
}

impl BlockLength {
    /// As in "1-month blocks".
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Week => "1-week",
            Self::Month => "1-month",
            Self::Quarter => "3-month",
        }
    }

    #[must_use]
    pub const fn nanos(self) -> UnixNanos {
        let days = match self {
            Self::Week => 7,
            Self::Month => 30,
            Self::Quarter => 91,
        };
        days * NANOS_PER_DAY
    }
}

/// A setting with its own switch; the value stays while it's off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section<T> {
    pub on: bool,
    pub value: T,
}

impl<T> Section<T> {
    const fn off(value: T) -> Self {
        Self { on: false, value }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Commission {
    /// Percent of each fill's value.
    Percent(f64),
    /// A flat amount per order, in the quote currency.
    PerOrder(f64),
    /// Percents of each fill's value: one for orders that rested, which can be below zero for a
    /// rebate, and one for those that took what was there.
    Rates { maker: f64, taker: f64 },
}

/// How orders go out as limits.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimitChoice {
    /// How far from the close it rests, on the better side, as a percentage.
    pub offset_percent: f64,
    /// How many bars it rests before it's cancelled.
    pub expires_bars: u32,
    /// How far past its price the market must trade before it fills, as a percentage, standing in for
    /// the queue ahead of it.
    #[serde(default)]
    pub through_percent: f64,
}

/// Trading one way only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OneWay {
    LongOnly,
    ShortOnly,
}

/// How finely backtests and their playback follow prices inside a bar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Precision {
    /// Orders fill on the chart's bars, and playback reveals each bar as it closes.
    #[default]
    Standard,
    /// Orders fill on ticks or 1-minute bars, and playback builds each bar from them.
    High,
}

/// When an order fills, on the bars.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillTiming {
    /// At the close it was decided on.
    #[default]
    Close,
    /// At the next bar's open.
    NextOpen,
}

/// When positions may be opened: some weekdays between two times of day, in a time zone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionRule {
    /// Monday first.
    pub days: [bool; 7],
    /// Minutes after midnight. Bars closing after `from` and up to `to` are in the session; a
    /// session that ends earlier than it starts runs overnight, and equal times are all day.
    pub from: u32,
    pub to: u32,
    /// `UTC`, an offset like `-05:00`, or an IANA name like `America/New_York`.
    pub zone: String,
    /// Close any position once the session is over.
    pub close_after: bool,
}

impl Default for SessionRule {
    fn default() -> Self {
        Self {
            days: [true, true, true, true, true, false, false],
            from: 9 * 60 + 30,
            to: 16 * 60,
            zone: "America/New_York".into(),
            close_after: false,
        }
    }
}

impl SessionRule {
    /// Whether a bar closing at `ts` falls inside the session.
    #[must_use]
    pub fn contains(&self, zone: &Zone, ts: UnixNanos) -> bool {
        // A close on the stroke of midnight belongs to the day before, at 24:00.
        let (day, nanos) = zone.to_local(ts.saturating_sub(1));
        let nanos = nanos + 1;
        let on = |day: i64| self.days[(day + 3).rem_euclid(7) as usize]; // 1970-01-01 was a Thursday
        let (from, to) = (
            UnixNanos::from(self.from) * NANOS_PER_MINUTE,
            UnixNanos::from(self.to) * NANOS_PER_MINUTE,
        );
        match from.cmp(&to) {
            std::cmp::Ordering::Equal => on(day),
            std::cmp::Ordering::Less => on(day) && nanos > from && nanos <= to,
            std::cmp::Ordering::Greater => {
                (on(day) && nanos > from) || (on(day - 1) && nanos <= to)
            }
        }
    }
}

/// A number without float noise: `100000`, `0.1`.
fn trim(value: f64) -> String {
    let text = format!("{value:.6}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// `09:30` → 570 minutes.
///
/// # Errors
///
/// Returns a message if it isn't a time of day.
pub fn parse_time(text: &str) -> Result<u32, String> {
    let error = || format!("`{text}` isn't a time like 09:30");
    let (hours, minutes) = text.trim().split_once(':').ok_or_else(error)?;
    let (hours, minutes): (u32, u32) = (
        hours.parse().map_err(|_| error())?,
        minutes.parse().map_err(|_| error())?,
    );
    if hours > 24 || minutes > 59 || (hours == 24 && minutes > 0) {
        return Err(error());
    }
    Ok(hours * 60 + minutes)
}

/// 570 → `09:30`.
#[must_use]
pub fn time_text(minutes: u32) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// A switch or choice in the settings section, applied as soon as it's made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edit {
    Stock(bool),
    Commission(bool),
    /// A flat fee per order instead of a percentage.
    PerOrder(bool),
    Slippage(bool),
    SlippagePercent(bool),
    MakerTaker(bool),
    Limit(bool),
    StopLoss(bool),
    TakeProfit(bool),
    Borrow(bool),
    Impact(bool),
    Participation(bool),
    RangeSlippage(bool),
    LatencyJitter(bool),
    AddLeg,
    RemoveLeg(usize),
    LegsUseStrategy(bool),
    CashInterest(bool),
    ClearWeights,
    Spread(bool),
    Latency(bool),
    Leverage(bool),
    Direction(bool),
    Invert(bool),
    Compound(bool),
    OneWay(OneWay),
    Session(bool),
    /// Monday is 0.
    Day(usize),
    CloseAfter(bool),
    Precision(Precision),
    FillTiming(FillTiming),
}

impl Settings {
    pub fn apply(&mut self, edit: Edit) {
        match edit {
            Edit::Stock(on) => self.stock = on,
            Edit::Commission(on) => self.commission.on = on,
            Edit::PerOrder(per_order) => {
                self.commission.value = match (per_order, self.commission.value) {
                    (true, Commission::Percent(_) | Commission::Rates { .. }) => {
                        Commission::PerOrder(1.0)
                    }
                    (false, Commission::PerOrder(_)) => Commission::Percent(0.1),
                    (false, Commission::Rates { taker, .. }) => Commission::Percent(taker),
                    (_, same) => same,
                };
            }
            Edit::Slippage(on) => self.slippage.on = on,
            Edit::SlippagePercent(on) => self.slippage_percent.on = on,
            Edit::MakerTaker(on) => {
                self.commission.value = match (on, self.commission.value) {
                    (true, Commission::Percent(rate)) => Commission::Rates {
                        maker: rate,
                        taker: rate,
                    },
                    (true, Commission::PerOrder(_)) => Commission::Rates {
                        maker: 0.02,
                        taker: 0.05,
                    },
                    (false, Commission::Rates { taker, .. }) => Commission::Percent(taker),
                    (_, same) => same,
                };
            }
            Edit::Limit(on) => self.limit.on = on,
            Edit::StopLoss(on) => self.stop_loss_percent.on = on,
            Edit::TakeProfit(on) => self.take_profit_percent.on = on,
            Edit::Borrow(on) => self.borrow_percent_per_year.on = on,
            Edit::Impact(on) => self.impact_percent.on = on,
            Edit::Participation(on) => self.max_participation_percent.on = on,
            Edit::RangeSlippage(on) => self.range_slippage_percent.on = on,
            Edit::LatencyJitter(on) => self.latency_jitter.on = on,
            Edit::AddLeg => self.portfolio.legs.push(crate::portfolio::LegSetup {
                symbol: String::new(),
                venue: None,
                weight: 0.0,
                strategy: None,
            }),
            Edit::RemoveLeg(leg) => {
                if leg < self.portfolio.legs.len() {
                    self.portfolio.legs.remove(leg);
                }
            }
            Edit::LegsUseStrategy(on) => self.portfolio.use_chart_strategy = on,
            Edit::CashInterest(on) => self.cash_interest_percent_per_year.on = on,
            Edit::ClearWeights => self.portfolio.weights = None,
            Edit::Spread(on) => self.spread.on = on,
            Edit::Latency(on) => self.latency.on = on,
            Edit::Leverage(on) => self.leverage.on = on,
            Edit::Direction(on) => self.direction.on = on,
            Edit::OneWay(way) => self.direction.value = way,
            Edit::Invert(on) => self.invert = on,
            Edit::Compound(on) => self.compound = on,
            Edit::Session(on) => self.session.on = on,
            Edit::Day(day) => {
                if let Some(on) = self.session.value.days.get_mut(day) {
                    *on = !*on;
                }
            }
            Edit::CloseAfter(on) => self.session.value.close_after = on,
            Edit::Precision(precision) => self.precision = precision,
            Edit::FillTiming(timing) => self.fill_timing = timing,
        }
    }

    /// What `field`'s box shows when nothing's been typed in it.
    #[must_use]
    pub fn text(&self, field: Field) -> String {
        match field {
            Field::Capital => trim(self.capital),
            Field::Commission => match self.commission.value {
                Commission::Percent(v) | Commission::PerOrder(v) => trim(v),
                Commission::Rates { maker, .. } => trim(maker),
            },
            Field::Taker => match self.commission.value {
                Commission::Rates { taker, .. } => trim(taker),
                _ => String::new(),
            },
            Field::LimitOffset => trim(self.limit.value.offset_percent),
            Field::LimitBars => self.limit.value.expires_bars.to_string(),
            Field::LimitThrough => trim(self.limit.value.through_percent),
            Field::StopLoss => trim(self.stop_loss_percent.value),
            Field::TakeProfit => trim(self.take_profit_percent.value),
            Field::Borrow => trim(self.borrow_percent_per_year.value),
            Field::Impact => trim(self.impact_percent.value),
            Field::Participation => trim(self.max_participation_percent.value),
            Field::RangeSlippage => trim(self.range_slippage_percent.value),
            Field::LatencyJitter => self.latency_jitter.value.to_string(),
            Field::LatencySeed => self.latency_seed.to_string(),
            Field::RangeLookBack => self.range_look_back.to_string(),
            Field::ImpactExponent => trim(self.impact_exponent),
            Field::VolumeWarning => trim(self.volume_warning_percent),
            Field::FillWarning => trim(self.fill_warning_percent),
            Field::CashInterest => trim(self.cash_interest_percent_per_year.value),
            Field::WarmUp => self.warm_up_bars.to_string(),
            Field::HoldOutShare => trim(self.hold_out_percent),
            Field::WalkRatio => trim(self.walk_ratio),
            Field::LegVenue(leg) => self
                .portfolio
                .legs
                .get(leg)
                .and_then(|leg| leg.venue.clone())
                .unwrap_or_default(),
            Field::LegSymbol(leg) => self
                .portfolio
                .legs
                .get(leg)
                .map_or_else(String::new, |leg| leg.symbol.clone()),
            Field::LegWeight(leg) => self
                .portfolio
                .legs
                .get(leg)
                .map_or_else(String::new, |leg| trim(leg.weight)),
            Field::RebalanceEvery => self
                .portfolio
                .rebalance
                .every_bars
                .map_or_else(String::new, |bars| bars.to_string()),
            Field::RebalanceDrift => self
                .portfolio
                .rebalance
                .drift_percent
                .map_or_else(String::new, trim),
            Field::RebalanceMinTrade => trim(self.portfolio.rebalance.min_trade_value),
            Field::Maintenance => trim(self.portfolio.maintenance_percent),
            Field::Slippage => self.slippage.value.to_string(),
            Field::SlippagePercent => trim(self.slippage_percent.value),
            Field::Spread => self.spread.value.to_string(),
            Field::Latency => self.latency.value.to_string(),
            Field::Leverage => trim(self.leverage.value),
            Field::SessionFrom => time_text(self.session.value.from),
            Field::SessionTo => time_text(self.session.value.to),
            Field::SessionZone => self.session.value.zone.clone(),
            Field::Runs => self.runs.to_string(),
            Field::Ruin => trim(self.ruin),
            Field::Blocks => self.blocks.to_string(),
            Field::Windows => self.windows.to_string(),
            Field::Entries => self.entries.to_string(),
            Field::Min(slot) | Field::Max(slot) | Field::Step(slot) => {
                let Some(variable) = &self.optimize.variables[slot] else {
                    return String::new();
                };
                trim(match field {
                    Field::Min(_) => variable.min,
                    Field::Max(_) => variable.max,
                    _ => variable.step,
                })
            }
        }
    }

    /// Takes what was typed in `field`'s box.
    ///
    /// # Errors
    ///
    /// Returns a message saying what's wrong with it; nothing changes then.
    pub fn set(&mut self, field: Field, text: &str) -> Result<(), String> {
        let text = text.trim();
        let number = |min: f64, max: f64, what: &str| -> Result<f64, String> {
            text.replace(',', "")
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && (min..=max).contains(v))
                .ok_or_else(|| format!("{what} is a number from {min} to {max}"))
        };
        let whole = |max: u32, what: &str| -> Result<u32, String> {
            text.parse::<u32>()
                .ok()
                .filter(|v| *v <= max)
                .ok_or_else(|| format!("{what} is a whole number up to {max}"))
        };
        match field {
            Field::Capital => self.capital = number(1.0, 1e12, "Capital")?,
            Field::Commission => {
                self.commission.value = match self.commission.value {
                    Commission::Percent(_) => {
                        Commission::Percent(number(0.0, 10.0, "A percentage")?)
                    }
                    Commission::PerOrder(_) => Commission::PerOrder(number(0.0, 1e6, "A fee")?),
                    Commission::Rates { taker, .. } => Commission::Rates {
                        maker: number(-1.0, 10.0, "A maker's fee")?,
                        taker,
                    },
                };
            }
            Field::Taker => {
                if let Commission::Rates { maker, .. } = self.commission.value {
                    self.commission.value = Commission::Rates {
                        maker,
                        taker: number(0.0, 10.0, "A taker's fee")?,
                    };
                }
            }
            Field::LimitOffset => {
                self.limit.value.offset_percent = number(0.0001, 50.0, "A limit's offset")?;
            }
            Field::LimitThrough => {
                self.limit.value.through_percent = number(0.0, 10.0, "How far through")?;
            }
            Field::LimitBars => {
                self.limit.value.expires_bars = number(1.0, 10_000.0, "Bars")?.round() as u32;
            }
            Field::StopLoss => self.stop_loss_percent.value = number(0.01, 99.0, "A stop")?,
            Field::TakeProfit => {
                self.take_profit_percent.value = number(0.01, 1_000.0, "A target")?
            }
            Field::Borrow => {
                self.borrow_percent_per_year.value = number(0.0, 1_000.0, "A borrowing rate")?;
            }
            Field::Impact => self.impact_percent.value = number(0.0, 100.0, "Impact")?,
            Field::Participation => {
                self.max_participation_percent.value = number(0.001, 100.0, "A share of volume")?;
            }
            Field::RangeSlippage => {
                self.range_slippage_percent.value = number(0.0, 100.0, "A share of the range")?;
            }
            Field::LatencyJitter => self.latency_jitter.value = whole(600_000, "Jitter")?,
            Field::LatencySeed => self.latency_seed = whole(u32::MAX, "The seed")?,
            Field::RangeLookBack => {
                self.range_look_back = whole(100_000, "Bars to look back")?.max(1);
            }
            Field::ImpactExponent => {
                self.impact_exponent = number(0.1, 2.0, "How steeply impact grows")?;
            }
            Field::VolumeWarning => {
                self.volume_warning_percent = number(0.0, 100.0, "A share of volume")?;
            }
            Field::FillWarning => {
                self.fill_warning_percent = number(0.0, 100.0, "A share of limits")?;
            }
            Field::CashInterest => {
                self.cash_interest_percent_per_year.value =
                    number(0.0, 1_000.0, "An interest rate")?;
            }
            Field::WarmUp => self.warm_up_bars = whole(100_000, "Warm-up bars")?,
            Field::HoldOutShare => {
                self.hold_out_percent = number(10.0, 95.0, "The share to pick on")?;
            }
            Field::WalkRatio => self.walk_ratio = number(0.1, 50.0, "The ratio")?,
            Field::LegVenue(leg) => {
                if let Some(leg) = self.portfolio.legs.get_mut(leg) {
                    leg.venue = (!text.is_empty()).then(|| text.to_uppercase());
                }
            }
            Field::LegSymbol(leg) => {
                if let Some(leg) = self.portfolio.legs.get_mut(leg) {
                    leg.symbol = text.to_uppercase();
                }
            }
            Field::LegWeight(leg) => {
                let weight = number(-100.0, 100.0, "A weight")?;
                if let Some(leg) = self.portfolio.legs.get_mut(leg) {
                    leg.weight = weight;
                }
            }
            Field::RebalanceEvery => {
                self.portfolio.rebalance.every_bars = if text.is_empty() {
                    None
                } else {
                    Some(whole(1_000_000, "Rebalancing every so many bars")?.max(1))
                };
            }
            Field::RebalanceDrift => {
                self.portfolio.rebalance.drift_percent = if text.is_empty() {
                    None
                } else {
                    Some(number(0.0, 1_000.0, "How far a leg may drift")?)
                };
            }
            Field::RebalanceMinTrade => {
                self.portfolio.rebalance.min_trade_value = number(0.0, 1e12, "The smallest trade")?;
            }
            Field::Maintenance => {
                self.portfolio.maintenance_percent = number(0.0, 100.0, "Maintenance margin")?;
            }
            Field::Slippage => self.slippage.value = whole(10_000, "Slippage")?,
            Field::SlippagePercent => {
                self.slippage_percent.value = number(0.0, 10.0, "A percentage")?;
            }
            Field::Spread => self.spread.value = whole(10_000, "The spread")?,
            Field::Latency => self.latency.value = whole(60_000, "Latency")?,
            Field::Leverage => self.leverage.value = number(1.0, 1_000.0, "Leverage")?,
            Field::SessionFrom => self.session.value.from = parse_time(text)?,
            Field::SessionTo => self.session.value.to = parse_time(text)?,
            Field::SessionZone => {
                let zone: Zone = text.parse().map_err(|e| format!("{e:#}"))?;
                self.session.value.zone = zone.to_string();
            }
            Field::Runs => self.runs = number(10.0, 10_000.0, "Runs")?.round() as u32,
            Field::Ruin => self.ruin = number(1.0, 100.0, "Ruin")?,
            Field::Blocks => self.blocks = number(2.0, 1_000.0, "Blocks")?.round() as u32,
            Field::Windows => self.windows = number(1.0, 100.0, "Windows")?.round() as u32,
            Field::Entries => self.entries = number(2.0, 5_000.0, "Runs")?.round() as u32,
            Field::Min(slot) | Field::Max(slot) | Field::Step(slot) => {
                let value = number(-1e12, 1e12, "Min, Max and Step each")?;
                if let Some(variable) = &mut self.optimize.variables[slot] {
                    match field {
                        Field::Min(_) => variable.min = value,
                        Field::Max(_) => variable.max = value,
                        _ => variable.step = value,
                    }
                }
            }
        }
        Ok(())
    }

    /// How many stock sections are switched on.
    #[must_use]
    pub const fn stock_sections_on(&self) -> usize {
        self.commission.on as usize
            + self.slippage.on as usize
            + self.slippage_percent.on as usize
            + self.limit.on as usize
            + self.stop_loss_percent.on as usize
            + self.take_profit_percent.on as usize
            + self.borrow_percent_per_year.on as usize
            + self.impact_percent.on as usize
            + self.cash_interest_percent_per_year.on as usize
            + self.max_participation_percent.on as usize
            + self.range_slippage_percent.on as usize
            + self.latency_jitter.on as usize
            + self.spread.on as usize
            + self.latency.on as usize
            + self.leverage.on as usize
            + self.direction.on as usize
            + self.invert as usize
            + self.compound as usize
            + self.session.on as usize
    }

    fn on<'a, T>(&self, section: &'a Section<T>) -> Option<&'a T> {
        (self.stock && section.on).then_some(&section.value)
    }

    /// The fee rates fills pay: a set percentage, or the market's own.
    #[must_use]
    pub fn fees(&self, market: FeeRates) -> FeeRates {
        match self.on(&self.commission) {
            Some(Commission::Percent(percent)) => FeeRates {
                maker: percent / 100.0,
                taker: percent / 100.0,
            },
            Some(Commission::Rates { maker, taker }) => FeeRates {
                maker: maker / 100.0,
                taker: taker / 100.0,
            },
            _ => market,
        }
    }

    #[must_use]
    pub fn sim(&self) -> SimSettings {
        SimSettings {
            fee_per_order: match self.on(&self.commission) {
                Some(Commission::PerOrder(fee)) => Some(*fee),
                _ => None,
            },
            slippage_ticks: self.on(&self.slippage).copied().unwrap_or(0),
            slippage_percent: self.on(&self.slippage_percent).copied().unwrap_or(0.0),
            spread_ticks: self.on(&self.spread).copied().unwrap_or(0),
            latency_ms: self.on(&self.latency).copied().unwrap_or(0),
            leverage: self.on(&self.leverage).copied().unwrap_or(1.0),
            compound: self.stock && self.compound,
            next_open: self.fill_timing == FillTiming::NextOpen,
            limit: self.on(&self.limit).map(|limit| LimitOrders {
                offset_percent: limit.offset_percent,
                expires_bars: limit.expires_bars,
                through_percent: limit.through_percent,
            }),
            stop_loss_percent: self.on(&self.stop_loss_percent).copied(),
            take_profit_percent: self.on(&self.take_profit_percent).copied(),
            borrow_percent_per_year: self.on(&self.borrow_percent_per_year).copied(),
            impact_percent: self.on(&self.impact_percent).copied().unwrap_or(0.0),
            impact_exponent: self.impact_exponent,
            volume_warning_percent: self.volume_warning_percent,
            fill_warning_percent: self.fill_warning_percent,
            cash_interest_percent_per_year: self.on(&self.cash_interest_percent_per_year).copied(),
            range_slippage_look_back: self.range_look_back,
            max_participation_percent: self.on(&self.max_participation_percent).copied(),
            range_slippage_percent: self
                .on(&self.range_slippage_percent)
                .copied()
                .unwrap_or(0.0),
            latency_jitter_ms: self.on(&self.latency_jitter).copied().unwrap_or(0),
            latency_seed: u64::from(self.latency_seed),
        }
    }

    /// The inverting, direction and session rules, for backtests and live trading alike.
    ///
    /// # Errors
    ///
    /// Returns a message if the session's time zone isn't one.
    pub fn rules(&self) -> Result<TradingRules, String> {
        let direction = match self.on(&self.direction) {
            Some(OneWay::LongOnly) => Direction::LongOnly,
            Some(OneWay::ShortOnly) => Direction::ShortOnly,
            None => Direction::Both,
        };
        let session = match self.on(&self.session) {
            Some(rule) => {
                let zone: Zone = rule.zone.parse().map_err(|e| format!("{e:#}"))?;
                let close_after = rule.close_after;
                let rule = rule.clone();
                Some(Session {
                    contains: Arc::new(move |ts| rule.contains(&zone, ts)),
                    close_after,
                })
            }
            None => None,
        };
        Ok(TradingRules {
            invert: self.stock && self.invert,
            direction,
            session,
        })
    }

    /// The commission as said in the panel: `0.1%` or `2.5 per order`, or the market's.
    #[must_use]
    pub fn describe_commission(&self) -> Option<String> {
        self.on(&self.commission).map(|c| match c {
            Commission::Percent(p) => format!("{p}%"),
            Commission::PerOrder(fee) => format!("{fee} per order"),
            Commission::Rates { maker, taker } => format!("{maker}% maker, {taker}% taker"),
        })
    }
}

#[cfg(test)]
mod tests {
    use persephone_core::time::NANOS_PER_DAY;

    use super::*;

    const MINUTE: UnixNanos = NANOS_PER_MINUTE;

    /// 2024-07-01 (a Monday) at `hh:mm` UTC.
    fn monday(hours: u64, minutes: u64) -> UnixNanos {
        persephone_core::time::days_from_civil(2024, 7, 1) as UnixNanos * NANOS_PER_DAY
            + (hours * 60 + minutes) * MINUTE
    }

    #[test]
    fn sessions_cover_closes_after_the_open_up_to_the_close() {
        let rule = SessionRule {
            zone: "UTC".into(),
            ..SessionRule::default()
        };
        let utc = Zone::Utc;
        assert!(
            !rule.contains(&utc, monday(9, 30)),
            "the bar before the open"
        );
        assert!(rule.contains(&utc, monday(9, 31)));
        assert!(rule.contains(&utc, monday(16, 0)), "the last bar");
        assert!(!rule.contains(&utc, monday(16, 1)));
        assert!(
            !rule.contains(&utc, monday(12, 0) + 5 * NANOS_PER_DAY),
            "Saturday"
        );

        // New York's 09:30 is 13:30 UTC in July.
        let new_york: Zone = "America/New_York".parse().unwrap();
        assert!(SessionRule::default().contains(&new_york, monday(13, 45)));
        assert!(!SessionRule::default().contains(&new_york, monday(9, 45)));

        // Overnight, Sunday 22:00 to Monday 06:00 counts from Sunday.
        let overnight = SessionRule {
            days: [false, false, false, false, false, false, true],
            from: 22 * 60,
            to: 6 * 60,
            zone: "UTC".into(),
            close_after: false,
        };
        assert!(
            overnight.contains(&utc, monday(3, 0)),
            "Monday morning, from Sunday"
        );
        assert!(overnight.contains(&utc, monday(0, 0)), "midnight");
        assert!(
            overnight.contains(&utc, monday(0, 0) - 60 * MINUTE),
            "Sunday 23:00"
        );
        assert!(
            !overnight.contains(&utc, monday(23, 0)),
            "Monday's own night is off"
        );
    }

    #[test]
    fn sections_count_only_when_stock_and_their_own_switch_are_on() {
        let mut settings = Settings::default();
        assert_eq!(settings.sim(), SimSettings::default());
        assert_eq!(
            settings.fees(FeeRates::BINANCE_SPOT),
            FeeRates::BINANCE_SPOT
        );

        settings.slippage.on = true;
        settings.commission = Section {
            on: true,
            value: Commission::PerOrder(2.5),
        };
        settings.direction.on = true;
        let sim = settings.sim();
        assert_eq!((sim.slippage_ticks, sim.fee_per_order), (1, Some(2.5)));
        assert_eq!(settings.rules().unwrap().direction, Direction::LongOnly);

        settings.stock = false;
        assert_eq!(settings.sim(), SimSettings::default(), "all off at once");
        assert_eq!(settings.rules().unwrap().direction, Direction::Both);

        settings.invert = true;
        assert!(!settings.rules().unwrap().invert, "off with the rest");
        settings.stock = true;
        assert!(settings.rules().unwrap().invert);

        settings.stock = true;
        settings.commission.value = Commission::Percent(0.05);
        assert!((settings.fees(FeeRates::ZERO).taker - 0.0005).abs() < 1e-12);
        settings.session.on = true;
        settings.session.value.zone = "Mars/Olympus".into();
        assert!(settings.rules().is_err());
    }

    #[test]
    fn settings_save_and_old_layouts_load() {
        let settings = Settings {
            latency: Section {
                on: true,
                value: 250,
            },
            plugins: vec!["Fill model".into()],
            ..Settings::default()
        };
        let text = ron::to_string(&settings).unwrap();
        assert_eq!(ron::from_str::<Settings>(&text).unwrap(), settings);
        assert_eq!(
            ron::from_str::<Settings>("()").unwrap(),
            Settings::default()
        );
    }

    #[test]
    fn boxes_show_values_and_take_typed_ones() {
        let mut settings = Settings::default();
        assert_eq!(settings.text(Field::Capital), "100000");
        assert_eq!(settings.text(Field::Commission), "0.1");
        assert_eq!(settings.text(Field::SessionFrom), "09:30");
        settings.set(Field::Capital, "25,000").unwrap();
        assert_eq!(settings.capital, 25_000.0);
        assert!(settings.set(Field::Leverage, "0.5").is_err());
        assert!(settings.set(Field::SessionZone, "Nowhere/Land").is_err());
        settings.set(Field::SessionZone, "europe/london").ok();
        settings.set(Field::SessionTo, "17:15").unwrap();
        assert_eq!(settings.session.value.to, 17 * 60 + 15);

        settings.apply(Edit::PerOrder(true));
        assert_eq!(settings.commission.value, Commission::PerOrder(1.0));
        settings.set(Field::Commission, "2.5").unwrap();
        assert_eq!(settings.commission.value, Commission::PerOrder(2.5));
        settings.apply(Edit::Day(6));
        assert!(settings.session.value.days[6], "Sunday on");
    }

    #[test]
    fn test_settings_are_checked_and_old_layouts_get_the_defaults() {
        let mut settings = Settings::default();
        settings.set(Field::Runs, "2,500").unwrap();
        assert_eq!(settings.runs, 2_500);
        assert!(settings.set(Field::Runs, "5").is_err());
        assert!(settings.set(Field::Ruin, "0").is_err());
        assert!(settings.set(Field::Blocks, "1").is_err());
        assert_eq!(settings.text(Field::Blocks), "50");

        let old: Settings = ron::from_str("(stock: false)").unwrap();
        assert_eq!(old.test, TestKind::Backtest);
        assert_eq!(old.runs, 1_000);
        assert_eq!(old.block_length, BlockLength::Month);
        assert!(
            old.optimize.hold_out,
            "keeping some bars unseen is the default"
        );

        settings.test = TestKind::Optimize;
        settings.block_length = BlockLength::Quarter;
        settings.optimize.score = ScoreBy::MonteCarlo;
        settings.optimize.hold_out = false;
        settings.optimize.variables[1] = Some(Variable {
            name: "Slow".into(),
            min: 10.0,
            max: 50.0,
            step: 5.0,
        });
        let text = ron::to_string(&settings).unwrap();
        assert_eq!(ron::from_str::<Settings>(&text).unwrap(), settings);
    }

    #[test]
    fn times_of_day_read_and_print() {
        assert_eq!(parse_time("09:30"), Ok(570));
        assert_eq!(parse_time(" 24:00 "), Ok(1_440));
        assert!(parse_time("9.30").is_err() && parse_time("25:00").is_err());
        assert_eq!(time_text(570), "09:30");
    }
}
