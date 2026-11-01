//! Several markets on one account, as the app and Lab both set them up and read them. The running is
//! the engine's; this is what's said to it and what's worked out from what comes back.

use std::{
    collections::BTreeMap,
    fmt,
    fs::{self, File},
    io::{Read as _, Write as _},
    path::Path,
    sync::Arc,
};

use flate2::{Compression, read::GzDecoder, write::GzEncoder};

use persephone_core::{
    BarSpec, Candle, InstrumentSpec, Side, UnixNanos,
    signal::{Note, SignalFactory},
    strategy::StrategySpec,
    time::{DateTime, NANOS_PER_DAY, NANOS_PER_MINUTE, days_from_civil},
};
use persephone_engine::{
    BacktestReport, FeeRates, Fill, LegReport, PortfolioLeg, PortfolioReport, PortfolioRequest,
    Rebalance, spawn_portfolio,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    figures::{self, CostBreakdown},
    market::MarketBars,
    positions::Positions,
    robustness::round_trips,
    settings::Settings,
};

/// What a position needs to stay open unless told otherwise, as a percentage of its value: about what
/// Binance asks of the smallest positions in the biggest markets.
pub const DEFAULT_MAINTENANCE_PERCENT: f64 = 0.5;

/// One market in a portfolio, as it's written down.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LegSetup {
    pub symbol: String,
    /// Where its bars come from, if not Binance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub venue: Option<String>,
    /// With a strategy, what its position size is multiplied by. Without, the signed share of the
    /// capital to hold.
    #[serde(default)]
    pub weight: f64,
    /// As a strategy is written anywhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<Value>,
}

/// How often legs go back to where they should be, as it's written down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RebalanceSetup {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub every_bars: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drift_percent: Option<f64>,
    pub min_trade_value: f64,
}

/// A whole portfolio, as it's written down: what a bundle carries, and what the app keeps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PortfolioSetup {
    pub legs: Vec<LegSetup>,
    /// `[[time, {"BTCUSDT": 0.5, "ETHUSDT": -0.5}], ...]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weights: Option<Value>,
    pub rebalance: RebalanceSetup,
    pub maintenance_percent: f64,
    /// Every leg trades the chart's own strategy, in the app.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub use_chart_strategy: bool,
}

impl Default for PortfolioSetup {
    fn default() -> Self {
        Self {
            legs: Vec::new(),
            weights: None,
            rebalance: RebalanceSetup::default(),
            maintenance_percent: DEFAULT_MAINTENANCE_PERCENT,
            use_chart_strategy: false,
        }
    }
}

/// A time from JSON: `2024-01-31`, `2024-01-31 09:30` (or with a `T`), both UTC, or a whole number of
/// nanoseconds since 1970.
///
/// # Errors
///
/// Returns a message saying what would be right.
pub fn moment(value: &Value) -> Result<UnixNanos, String> {
    if let Some(nanos) = value.as_u64() {
        return Ok(nanos);
    }
    let bad = || {
        format!(
            "expected a time like `2024-01-31` or `2024-01-31 09:30` in UTC, or nanoseconds since 1970, not {value}"
        )
    };
    let text = value.as_str().ok_or_else(bad)?.trim();
    let text = text.strip_suffix('Z').unwrap_or(text);
    let (date, time) = text
        .split_once(['T', ' '])
        .map_or((text, None), |(date, time)| (date, Some(time)));
    let mut parts = date.split('-');
    let mut number = |max: i64| {
        parts
            .next()
            .and_then(|part| part.parse::<i64>().ok())
            .filter(|n| (0..=max).contains(n))
    };
    let (Some(year), Some(month), Some(day)) = (number(9_999), number(12), number(31)) else {
        return Err(bad());
    };
    if year < 1970 || month == 0 || day == 0 {
        return Err(bad());
    }
    let days = days_from_civil(year, month as u32, day as u32) as u64;
    let minutes = match time {
        Some(time) => {
            let mut parts = time.split(':');
            let hours = parts.next().and_then(|p| p.parse::<u64>().ok());
            let minutes = parts.next().and_then(|p| p.parse::<u64>().ok());
            match (hours, minutes) {
                (Some(h), Some(m)) if h <= 24 && m < 60 => h * 60 + m,
                _ => return Err(bad()),
            }
        }
        None => 0,
    };
    Ok(days * NANOS_PER_DAY + minutes * NANOS_PER_MINUTE)
}

/// A weight table from JSON, one weight per symbol in each row, those a row leaves out going flat. A
/// row stamped with a bar's open applies at that bar's close, as position lists do, as a weight
/// worked out from a bar can't trade before it's closed. `{}` on its own is a table that's flat
/// throughout, the candidate a walk-forward can always fall back on.
///
/// # Errors
///
/// Returns a message saying which row is wrong.
pub fn parse_weights(
    value: &Value,
    symbols: &[String],
) -> Result<Vec<(UnixNanos, Vec<f64>)>, String> {
    if value.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(vec![(0, vec![0.0; symbols.len()])]);
    }
    let rows = value
        .as_array()
        .ok_or("`weights` should be a list of [time, {symbol: weight}], or {} to stay flat")?;
    let mut table = Vec::with_capacity(rows.len());
    for (at, row) in rows.iter().enumerate() {
        let (Some(when), Some(weights)) = (row.get(0), row.get(1).and_then(Value::as_object))
        else {
            return Err(format!(
                "`weights`: row {at} should be [time, {{symbol: weight}}], not {row}"
            ));
        };
        let when = moment(when).map_err(|e| format!("`weights`: row {at}: {e}"))?;
        let mut line = vec![0.0; symbols.len()];
        for (symbol, weight) in weights {
            let symbol = symbol.to_uppercase();
            let Some(index) = symbols.iter().position(|known| *known == symbol) else {
                return Err(format!(
                    "`weights`: row {at} names {symbol}, which isn't one of the markets, {symbols:?}"
                ));
            };
            line[index] = weight.as_f64().ok_or_else(|| {
                format!("`weights`: row {at}: {symbol}'s weight isn't a number: {weight}")
            })?;
        }
        table.push((when + 1, line));
    }
    table.sort_by_key(|row| row.0);
    Ok(table)
}

/// A leg's market, wherever it was loaded from.
#[derive(Clone, Debug)]
pub struct LegMarket {
    pub instrument: InstrumentSpec,
    pub bars: BarSpec,
    pub candles: Vec<Candle>,
    pub fees: FeeRates,
    pub funding: Option<Arc<Vec<(UnixNanos, f64)>>>,
}

impl From<MarketBars> for LegMarket {
    fn from(market: MarketBars) -> Self {
        Self {
            instrument: market.instrument,
            bars: market.bars,
            candles: market.candles,
            fees: market.fees,
            funding: market.funding,
        }
    }
}

/// A leg's strategy, and what makes its signal if it isn't built in.
pub type LegStrategy = (StrategySpec, Option<Arc<dyn SignalFactory>>);

/// Everything a run of `setup` needs, with each leg's bars and strategy already found.
///
/// # Errors
///
/// Returns a message if the settings or weights are wrong.
pub fn request(
    setup: &PortfolioSetup,
    markets: Vec<LegMarket>,
    strategies: Vec<Option<LegStrategy>>,
    settings: &Settings,
    trade_from: UnixNanos,
) -> Result<PortfolioRequest, String> {
    let bars = markets
        .first()
        .map(|market| market.bars)
        .ok_or("a portfolio needs at least one market")?;
    let symbols: Vec<String> = markets
        .iter()
        .map(|market| market.instrument.symbol.clone())
        .collect();
    let weights = setup
        .weights
        .as_ref()
        .map(|table| parse_weights(table, &symbols).map(Arc::new))
        .transpose()?;
    // Lists are given by when bars open, as they're seen, and heard by when they close.
    let mut strategies = strategies;
    for ((leg, market), strategy) in setup.legs.iter().zip(&markets).zip(&mut strategies) {
        if let (Some(list), Some(found)) = (
            leg.strategy
                .as_ref()
                .and_then(|written| written.get("positions")),
            strategy.as_mut(),
        ) {
            let opens: Vec<u64> = market.candles.iter().map(|c| c.ts).collect();
            let closes = market.bars.closes(&market.candles);
            let list = Positions::parse(list)?.at_closes(&opens, &closes)?;
            found.1 = Some(Arc::new(list));
        }
    }
    let legs = markets
        .into_iter()
        .zip(strategies)
        .zip(&setup.legs)
        .map(|((market, strategy), leg)| {
            let (strategy, plugin) =
                strategy.map_or((None, None), |(spec, plugin)| (Some(spec), plugin));
            PortfolioLeg {
                fees: settings.fees(market.fees),
                instrument: market.instrument,
                candles: market.candles,
                strategy,
                plugin,
                weight: leg.weight,
                funding: settings.funding(market.funding),
            }
        })
        .collect();
    Ok(PortfolioRequest {
        legs,
        bars,
        weights,
        rebalance: Rebalance {
            every_bars: setup.rebalance.every_bars,
            drift_percent: setup.rebalance.drift_percent,
            min_trade_value: setup.rebalance.min_trade_value,
        },
        starting_balance: settings.capital,
        sim: settings.sim(),
        rules: settings.rules()?,
        trade_from,
        maintenance_percent: setup.maintenance_percent,
    })
}

/// Whether a portfolio has anything to pay.
#[must_use]
pub fn costs_something(request: &PortfolioRequest) -> bool {
    figures::sim_costs_something(&request.sim)
        || request.legs.iter().any(|leg| {
            leg.fees.maker != 0.0
                || leg.fees.taker != 0.0
                || leg.funding.as_ref().is_some_and(|rates| !rates.is_empty())
        })
}

/// The same portfolio with nothing to pay.
#[must_use]
pub fn without_costs(request: &PortfolioRequest) -> PortfolioRequest {
    let mut free = request.clone();
    free.sim = figures::free_sim(&request.sim);
    for leg in &mut free.legs {
        leg.fees = FeeRates::ZERO;
        leg.funding = None;
    }
    free
}

/// What's worked out from a finished run, the same for the app and Lab.
#[derive(Clone, Debug, Default)]
pub struct PortfolioFigures {
    pub net_return: f64,
    /// What it made with nothing to pay, if that was run.
    pub gross_return: Option<f64>,
    /// Fees, slippage, spread, impact, funding and borrowing, in the quote currency.
    pub costs_paid: f64,
    /// That by kind.
    pub costs: CostBreakdown,
    pub sharpe: Option<f64>,
    /// How much the account moved with equal shares of its markets, day by day.
    pub beta: Option<f64>,
    pub by_year: BTreeMap<i64, f64>,
    pub by_year_compounded: BTreeMap<i64, Option<f64>>,
    /// An equal share of the capital held in each market from its first bar in the span.
    pub benchmark_return: f64,
    pub benchmark: Vec<f64>,
    /// That after a perpetual's funding, which is what holding one really made.
    pub benchmark_after_funding: f64,
    /// The weights asked for, held to the end and never rebalanced, with funding paid. `None` when
    /// strategies or a table that switches decide the positions, as there are no one set of weights to
    /// hold. Fair for long and short books, which equal shares isn't.
    pub weights_held_return: Option<f64>,
    /// What trades made by the hour and weekday they started, across the legs.
    pub by_hour: Vec<f64>,
    pub by_weekday: Vec<f64>,
    /// How each market's returns moved with each other's, bar by bar, where both had a bar.
    pub correlation: Vec<Vec<Option<f64>>>,
}

/// A finished run and all that's worked out from it.
#[derive(Clone, Debug, Default)]
pub struct PortfolioRun {
    pub report: PortfolioReport,
    pub figures: PortfolioFigures,
    /// Where on the timeline the span asked for begins.
    pub from_index: usize,
    /// When each moment's bars opened, how bars are named everywhere else.
    pub opens: Vec<UnixNanos>,
    /// What costs came to was worked out.
    pub friction: bool,
    /// Orders filled at next opens.
    pub next_open: bool,
}

/// Runs a portfolio, and again with nothing to pay if `friction` is wanted and it has costs.
///
/// # Errors
///
/// Returns a message if the engine refuses it or fails.
pub fn run(request: PortfolioRequest, friction: bool) -> Result<PortfolioRun, String> {
    let go = |request: PortfolioRequest| {
        spawn_portfolio(request)
            .join()
            .map_err(|_| "the portfolio's thread panicked".to_owned())?
            .map_err(|e| e.to_string())
    };
    let next_open = request.sim.next_open;
    let free = (friction && costs_something(&request)).then(|| without_costs(&request));
    let report = go(request.clone())?;
    let free = free.map(go).transpose()?;
    let from_index = report
        .times
        .partition_point(|time| *time <= request.trade_from);
    let mut opens: Vec<UnixNanos> = request
        .legs
        .iter()
        .flat_map(|leg| leg.candles.iter().map(|candle| candle.ts))
        .collect();
    opens.sort_unstable();
    opens.dedup();
    if opens.len() != report.times.len() {
        opens.clone_from(&report.times);
    }
    let figures = PortfolioFigures::of(
        &report,
        free.as_ref().or(friction.then_some(&report)),
        &request,
        from_index,
        &opens,
    );
    Ok(PortfolioRun {
        report,
        figures,
        from_index,
        opens,
        friction,
        next_open,
    })
}

/// A time as Lab's answers show it: `2024-01-31 09:30`.
fn time_text(ts: UnixNanos) -> String {
    let at = DateTime::from_nanos(ts);
    format!("{} {}", at.date(), at.time())
}

/// Says when what the costs took doesn't explain the gap between the run before costs and after. The
/// run before costs is run again without them, and when weights are kept to a share of the account
/// (rebalancing, or a table) it then trades other sizes, so the gap holds more than costs.
fn gross_gap_warning(run: &PortfolioRun) -> Option<String> {
    let figures = &run.figures;
    let capital = run.report.starting_balance;
    let gross = figures.gross_return?;
    if !run.friction || capital <= 0.0 || figures.costs_paid <= 0.0 {
        return None;
    }
    let gap = (gross - figures.net_return) * capital;
    ((gap - figures.costs_paid).abs() > 0.1 * figures.costs_paid).then(|| {
        format!(
            "Before costs less after is {gap:.2}, but the costs paid were {:.2}. The run without costs traded other sizes, as its account grew differently, so the gap isn't only costs: `total_costs_paid` is what was really paid.",
            figures.costs_paid
        )
    })
}

/// What's listed about a run, and all of it, as both Lab and the app write it down.
#[must_use]
pub fn written(run: &PortfolioRun, run_id: &str, created: u64, args: &Value) -> (Value, Value) {
    let (report, figures, opens, from_index) =
        (&run.report, &run.figures, &run.opens, run.from_index);
    let mut warnings: Vec<String> = report
        .notes
        .iter()
        .filter(|note| note.warning)
        .map(|note| note.text.clone())
        .collect();
    if let Some(ignored) = args
        .get("settings")
        .and_then(|settings| serde_json::from_value::<Settings>(settings.clone()).ok())
        .and_then(|settings| settings.ignored_warning())
    {
        warnings.push(ignored);
    }
    warnings.extend(gross_gap_warning(run));
    let mut summary = json!({
        "kind": "portfolio",
        "fills_on": if run.next_open { "next_open" } else { "bars" },
        "legs": report.legs.iter().map(|leg| leg.symbol.as_str()).collect::<Vec<_>>(),
        "first_bar": opens.get(from_index).copied().map(time_text),
        "last_bar": opens.last().copied().map(time_text),
        "bars": opens.len().saturating_sub(from_index),
        "net_return": figures.net_return,
        "gross_return": figures.gross_return,
        "total_costs_paid": run.friction.then_some(figures.costs_paid),
        "maker_fees": figures.costs.maker_fees,
        "taker_fees": figures.costs.taker_fees,
        "slippage_paid": figures.costs.slippage,
        "spread_paid": figures.costs.spread,
        "range_slippage_paid": figures.costs.range_slippage,
        "impact_paid": figures.costs.impact,
        "cost_rounding": figures.costs.rounding,
        "final_equity": report.equity.last(),
        "max_drawdown": report.max_drawdown.abs(),
        "sharpe": figures.sharpe,
        "beta": figures.beta,
        "by_year": figures.by_year.iter().map(|(year, made)| (year.to_string(), json!(made))).collect::<serde_json::Map<_, _>>(),
        "by_year_compounded": figures.by_year_compounded.iter().map(|(year, made)| (year.to_string(), json!(made))).collect::<serde_json::Map<_, _>>(),
        "benchmark_return": figures.benchmark_return,
        "benchmark_after_funding": figures.benchmark_after_funding,
        "weights_held_return": figures.weights_held_return,
        "excess_return": figures.net_return - figures.benchmark_after_funding,
        "by_hour": figures.by_hour,
        "by_weekday": figures.by_weekday,
        "fees": report.fees,
        "funding_paid": report.funding_paid,
        "borrow_paid": report.borrow_paid,
        "interest_paid": report.interest_paid,
        "total_orders": report.total_orders,
        "turnover": report.turnover,
        "peak_margin_use": report.peak_margin_use,
        "closest_to_liquidation": report.closest_to_liquidation,
        "liquidated_at": report.liquidated_at.map(time_text),
        "warnings": warnings,
    });
    if let Some(label) = args.get("label") {
        summary["label"] = label.clone();
    }
    let legs: Vec<Value> = report
        .legs
        .iter()
        .map(|leg| {
            json!({
                "symbol": leg.symbol,
                "made": leg.contribution.last(),
                "fees": leg.fees,
                "funding_paid": leg.funding_paid,
                "borrow_paid": leg.borrow_paid,
                "fills": leg.fills.len(),
                "orders_worked": leg.orders_worked,
                "bars_to_fill_average": leg.bars_to_fill_average,
                "bars_to_fill_longest": leg.bars_to_fill_longest,
                "unfilled_at_end": leg.unfilled_at_end,
                "missing_bars": leg.missing_bars,
            })
        })
        .collect();
    let whole = json!({
        "run_id": run_id,
        "kind": "portfolio",
        "created": created,
        "request": args,
        "summary": summary,
        "from_index": from_index,
        "times": opens,
        "moments": report.times,
        "starting_balance": report.starting_balance,
        "liquidated_ts": report.liquidated_at,
        "equity": report.equity,
        "benchmark": figures.benchmark,
        "gross_exposure": report.gross_exposure,
        "net_exposure": report.net_exposure,
        "correlation": figures.correlation,
        "legs": legs,
        "contributions": report.legs.iter().map(|leg| &leg.contribution).collect::<Vec<_>>(),
        "values": report.legs.iter().map(|leg| &leg.values).collect::<Vec<_>>(),
        "leg_fills": report.legs.iter().map(|leg| leg.fills.iter().map(|f| json!([
            f.ts, f.bar, if f.side == Side::Buy { "buy" } else { "sell" },
            f.price, f.qty, f.fee, f.range_slip,
        ])).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "notes": report.notes.iter()
            .map(|n| json!({"text": n.text, "warning": n.warning}))
            .collect::<Vec<_>>(),
    });
    (summary, whole)
}

/// Saves a run where Lab keeps its runs, so the app and Lab each see the other's.
///
/// # Errors
///
/// Returns a message if it can't be written.
pub fn save(dir: &Path, whole: &Value, summary: &Value, args: &Value) -> Result<(), String> {
    let run_id = whole["run_id"].as_str().ok_or("the run has no id")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut packed = GzEncoder::new(Vec::new(), Compression::fast());
    packed
        .write_all(whole.to_string().as_bytes())
        .and_then(|()| packed.finish())
        .and_then(|bytes| fs::write(dir.join(format!("{run_id}.json.gz")), bytes))
        .and_then(|()| {
            let brief = json!({
                "run_id": run_id, "created": whole["created"], "summary": summary, "request": args,
            });
            fs::write(dir.join(format!("{run_id}.summary")), brief.to_string())
        })
        .map_err(|e| format!("couldn't save the run: {e}"))
}

/// A saved portfolio run, as a list shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct SavedPortfolio {
    pub id: String,
    pub created: u64,
    pub name: String,
}

impl fmt::Display for SavedPortfolio {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// The portfolio runs saved in `dir`, newest first.
#[must_use]
pub fn list_saved(dir: &Path) -> Vec<SavedPortfolio> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<SavedPortfolio> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let id = name.to_str()?.strip_suffix(".summary")?.to_owned();
            let brief: Value =
                serde_json::from_str(&fs::read_to_string(entry.path()).ok()?).ok()?;
            let summary = &brief["summary"];
            if summary["kind"] != "portfolio" {
                return None;
            }
            let markets: Vec<&str> = summary["legs"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let made = summary["net_return"].as_f64().unwrap_or(0.0) * 100.0;
            let label = summary["label"]
                .as_str()
                .map_or_else(|| markets.join(", "), ToOwned::to_owned);
            let last = summary["last_bar"].as_str().unwrap_or_default();
            let day = last.split(' ').next().unwrap_or_default();
            Some(SavedPortfolio {
                id,
                created: brief["created"].as_u64().unwrap_or(0),
                name: format!("{label} {made:+.1}% to {day}"),
            })
        })
        .collect();
    found.sort_by_key(|saved| std::cmp::Reverse(saved.created));
    found
}

/// A saved run, ready to show. Only what was written down comes back, which is all that's shown.
///
/// # Errors
///
/// Returns a message if it can't be read.
pub fn load_saved(dir: &Path, id: &str) -> Result<PortfolioRun, String> {
    let file = File::open(dir.join(format!("{id}.json.gz")))
        .map_err(|e| format!("couldn't open the run: {e}"))?;
    let mut text = String::new();
    GzDecoder::new(file)
        .read_to_string(&mut text)
        .map_err(|e| format!("couldn't read the run: {e}"))?;
    let run: Value =
        serde_json::from_str(&text).map_err(|e| format!("couldn't read the run: {e}"))?;
    if run["kind"] != "portfolio" {
        return Err(format!("`{id}` isn't a portfolio"));
    }
    Ok(from_written(&run))
}

fn numbers(value: &Value) -> Vec<f64> {
    value.as_array().map_or_else(Vec::new, |line| {
        line.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect()
    })
}

fn stamps(value: &Value) -> Vec<UnixNanos> {
    value.as_array().map_or_else(Vec::new, |line| {
        line.iter().map(|v| v.as_u64().unwrap_or(0)).collect()
    })
}

fn from_written(run: &Value) -> PortfolioRun {
    let summary = &run["summary"];
    let start = run["starting_balance"].as_f64().unwrap_or(0.0);
    let empty = Vec::new();
    let legs = run["legs"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .enumerate()
        .map(|(at, leg)| {
            let contribution = numbers(&run["contributions"][at]);
            LegReport {
                symbol: leg["symbol"].as_str().unwrap_or_default().to_owned(),
                fills: run["leg_fills"][at]
                    .as_array()
                    .unwrap_or(&empty)
                    .iter()
                    .map(|fill| Fill {
                        ts: fill[0].as_u64().unwrap_or(0),
                        bar: fill[1].as_u64().unwrap_or(0) as usize,
                        side: if fill[2] == "buy" {
                            Side::Buy
                        } else {
                            Side::Sell
                        },
                        price: fill[3].as_f64().unwrap_or(f64::NAN),
                        qty: fill[4].as_f64().unwrap_or(f64::NAN),
                        fee: fill[5].as_f64().unwrap_or(0.0),
                        maker: false,
                        range_slip: fill[6].as_f64().unwrap_or(0.0),
                    })
                    .collect(),
                account_line: contribution.iter().map(|made| start + made).collect(),
                contribution,
                values: numbers(&run["values"][at]),
                fees: leg["fees"].as_f64().unwrap_or(0.0),
                funding_paid: leg["funding_paid"].as_f64().unwrap_or(0.0),
                borrow_paid: leg["borrow_paid"].as_f64().unwrap_or(0.0),
                orders_worked: leg["orders_worked"].as_u64().unwrap_or(0) as usize,
                bars_to_fill_average: leg["bars_to_fill_average"].as_f64().unwrap_or(0.0),
                bars_to_fill_longest: leg["bars_to_fill_longest"].as_u64().unwrap_or(0) as usize,
                unfilled_at_end: leg["unfilled_at_end"].as_f64().unwrap_or(0.0),
                missing_bars: leg["missing_bars"].as_u64().unwrap_or(0) as usize,
                ..LegReport::default()
            }
        })
        .collect();
    let report = PortfolioReport {
        times: stamps(&run["moments"]),
        equity: numbers(&run["equity"]),
        starting_balance: start,
        max_drawdown: -summary["max_drawdown"].as_f64().unwrap_or(0.0),
        fees: summary["fees"].as_f64().unwrap_or(0.0),
        funding_paid: summary["funding_paid"].as_f64().unwrap_or(0.0),
        borrow_paid: summary["borrow_paid"].as_f64().unwrap_or(0.0),
        interest_paid: summary["interest_paid"].as_f64().unwrap_or(0.0),
        total_orders: summary["total_orders"].as_u64().unwrap_or(0) as usize,
        legs,
        gross_exposure: numbers(&run["gross_exposure"]),
        net_exposure: numbers(&run["net_exposure"]),
        turnover: summary["turnover"].as_f64().unwrap_or(0.0),
        peak_margin_use: summary["peak_margin_use"].as_f64().unwrap_or(0.0),
        closest_to_liquidation: summary["closest_to_liquidation"].as_f64(),
        liquidated_at: run["liquidated_ts"].as_u64(),
        notes: run["notes"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(|note| Note {
                text: note["text"].as_str().unwrap_or_default().to_owned(),
                warning: note["warning"].as_bool().unwrap_or(false),
            })
            .collect(),
    };
    let figures = PortfolioFigures {
        net_return: summary["net_return"].as_f64().unwrap_or(0.0),
        gross_return: summary["gross_return"].as_f64(),
        costs_paid: summary["total_costs_paid"].as_f64().unwrap_or(0.0),
        costs: {
            let figure = |name: &str| summary[name].as_f64().unwrap_or(0.0);
            CostBreakdown {
                maker_fees: figure("maker_fees"),
                taker_fees: figure("taker_fees"),
                slippage: figure("slippage_paid"),
                spread: figure("spread_paid"),
                range_slippage: figure("range_slippage_paid"),
                impact: figure("impact_paid"),
                funding: figure("funding_paid"),
                borrow: figure("borrow_paid"),
                interest: figure("interest_paid"),
                rounding: figure("cost_rounding"),
            }
        },
        sharpe: summary["sharpe"].as_f64(),
        beta: summary["beta"].as_f64(),
        by_year: summary["by_year"]
            .as_object()
            .map_or_else(BTreeMap::new, |years| {
                years
                    .iter()
                    .filter_map(|(year, made)| Some((year.parse().ok()?, made.as_f64()?)))
                    .collect()
            }),
        by_year_compounded: summary["by_year_compounded"].as_object().map_or_else(
            BTreeMap::new,
            |years| {
                years
                    .iter()
                    .filter_map(|(year, made)| Some((year.parse().ok()?, made.as_f64())))
                    .collect()
            },
        ),
        benchmark_return: summary["benchmark_return"].as_f64().unwrap_or(0.0),
        benchmark_after_funding: summary["benchmark_after_funding"].as_f64().unwrap_or(0.0),
        weights_held_return: summary["weights_held_return"].as_f64(),
        by_hour: numbers(&summary["by_hour"]),
        by_weekday: numbers(&summary["by_weekday"]),
        benchmark: numbers(&run["benchmark"]),
        correlation: run["correlation"].as_array().map_or_else(Vec::new, |rows| {
            rows.iter()
                .map(|row| {
                    row.as_array()
                        .map_or_else(Vec::new, |line| line.iter().map(Value::as_f64).collect())
                })
                .collect()
        }),
    };
    PortfolioRun {
        report,
        figures,
        from_index: run["from_index"].as_u64().unwrap_or(0) as usize,
        opens: stamps(&run["times"]),
        friction: !summary["total_costs_paid"].is_null(),
        next_open: summary["fills_on"] == "next_open",
    }
}

/// As a backtest's report, for the figures that read one.
fn as_backtest(report: &PortfolioReport) -> BacktestReport {
    BacktestReport {
        equity: report.equity.clone(),
        starting_balance: report.starting_balance,
        ..BacktestReport::default()
    }
}

impl PortfolioFigures {
    /// `free` is the same run with nothing to pay, if there is one, and `from_index` where on the
    /// timeline the span asked for begins.
    #[must_use]
    pub fn of(
        report: &PortfolioReport,
        free: Option<&PortfolioReport>,
        request: &PortfolioRequest,
        from_index: usize,
        opens: &[UnixNanos],
    ) -> Self {
        let bars: Vec<(u64, f64)> = opens
            .iter()
            .zip(&report.equity)
            .map(|(&time, &equity)| (time, equity))
            .collect();
        let whole = as_backtest(report);
        let benchmark = benchmark(report, from_index);
        let equal = vec![1.0 / report.legs.len().max(1) as f64; report.legs.len()];
        let (mut by_hour, mut by_weekday) = (vec![0.0; 24], vec![0.0; 7]);
        for leg in &report.legs {
            // A leg's own account, from nothing, as what it's made is all that's asked.
            let (hours, weekdays) = figures::by_time(&round_trips(&BacktestReport {
                fills: leg.fills.clone(),
                equity: leg.contribution.clone(),
                ..BacktestReport::default()
            }));
            for (total, part) in by_hour.iter_mut().zip(hours) {
                *total += part;
            }
            for (total, part) in by_weekday.iter_mut().zip(weekdays) {
                *total += part;
            }
        }
        let costs = cost_breakdown(report, request);
        Self {
            net_return: report.net_return(),
            gross_return: free.map(PortfolioReport::net_return),
            costs_paid: costs.total(),
            costs,
            sharpe: figures::sharpe(&whole, from_index, &bars),
            beta: figures::daily_beta(
                (report.starting_balance, report.starting_balance),
                opens
                    .iter()
                    .zip(&report.equity)
                    .zip(&benchmark)
                    .skip(from_index)
                    .map(|((&time, &equity), &held)| (time, equity, held)),
            ),
            by_year: figures::by_year(&whole, from_index, &bars),
            by_year_compounded: figures::by_year_compounded(&whole, from_index, &bars),
            benchmark_return: benchmark
                .last()
                .map_or(0.0, |last| last / report.starting_balance - 1.0),
            benchmark,
            benchmark_after_funding: held_return(report, request, from_index, &equal)
                .unwrap_or(0.0),
            weights_held_return: first_weights(request)
                .and_then(|weights| held_return(report, request, from_index, &weights)),
            by_hour,
            by_weekday,
            correlation: correlation(report, from_index),
        }
    }
}

/// What a portfolio paid by kind: each leg's, as one market's run works it out, and the interest on
/// cash below zero, which belongs to the account.
fn cost_breakdown(report: &PortfolioReport, request: &PortfolioRequest) -> CostBreakdown {
    let legs = report
        .legs
        .iter()
        .zip(&request.legs)
        .map(|(leg, asked)| {
            let shim = BacktestReport {
                fills: leg.fills.clone(),
                fees: leg.fees,
                funding_paid: leg.funding_paid,
                borrow_paid: leg.borrow_paid,
                ..BacktestReport::default()
            };
            figures::cost_breakdown(
                &shim,
                &request.sim,
                asked.instrument.price_precision,
                &leg.volumes,
            )
        })
        .fold(CostBreakdown::default(), CostBreakdown::plus);
    CostBreakdown {
        interest: report.interest_paid,
        ..legs
    }
}

/// The account holding an equal share of the capital in each market from its first bar in the span,
/// and cash until then.
fn benchmark(report: &PortfolioReport, from_index: usize) -> Vec<f64> {
    let count = report.legs.len().max(1) as f64;
    let share = report.starting_balance / count;
    let firsts: Vec<Option<usize>> = report
        .legs
        .iter()
        .map(|leg| (from_index..leg.real.len()).find(|&at| leg.real[at]))
        .collect();
    (0..report.times.len())
        .map(|at| {
            report
                .legs
                .iter()
                .zip(&firsts)
                .map(|(leg, first)| match first {
                    Some(first) if at >= *first && leg.closes[*first] > 0.0 => {
                        share * leg.closes[at] / leg.closes[*first]
                    }
                    _ => share,
                })
                .sum()
        })
        .collect()
}

/// The weights first asked for, if the book is given by weights.
fn first_weights(request: &PortfolioRequest) -> Option<Vec<f64>> {
    if let Some(table) = &request.weights {
        // A table that switches has no one set of weights: which row came first says nothing.
        let first = table.first()?;
        return table
            .iter()
            .all(|row| row.1 == first.1)
            .then(|| first.1.clone());
    }
    request
        .legs
        .iter()
        .all(|leg| leg.strategy.is_none())
        .then(|| request.legs.iter().map(|leg| leg.weight).collect())
}

/// What these shares of the capital came to, held from each market's first bar in the span, as a share
/// of the capital, with what funding took as each fell due.
fn held_return(
    report: &PortfolioReport,
    request: &PortfolioRequest,
    from_index: usize,
    weights: &[f64],
) -> Option<f64> {
    let mut total = 0.0;
    for ((leg, asked), weight) in report.legs.iter().zip(&request.legs).zip(weights) {
        let Some(first) = (from_index..leg.real.len()).find(|&at| leg.real[at]) else {
            continue;
        };
        let start = *leg.closes.get(first)?;
        if start <= 0.0 {
            continue;
        }
        total += weight * (leg.closes.last()? / start - 1.0);
        for &(when, rate) in asked.funding.as_deref().map_or(&[][..], Vec::as_slice) {
            let at = report.times.partition_point(|time| *time <= when);
            if at > first
                && let Some(close) = leg.closes.get(at - 1)
            {
                total -= weight * close / start * rate;
            }
        }
    }
    Some(total)
}

fn correlation(report: &PortfolioReport, from_index: usize) -> Vec<Vec<Option<f64>>> {
    let returns = |leg: &persephone_engine::LegReport, at: usize| -> Option<f64> {
        let was = *leg.closes.get(at.checked_sub(1)?)?;
        (leg.real[at] && leg.real[at - 1] && was > 0.0).then(|| leg.closes[at] / was - 1.0)
    };
    let pair = |a: &persephone_engine::LegReport, b: &persephone_engine::LegReport| {
        let points: Vec<(f64, f64)> = (from_index..report.times.len())
            .filter_map(|at| Some((returns(a, at)?, returns(b, at)?)))
            .collect();
        pearson(&points)
    };
    report
        .legs
        .iter()
        .map(|a| report.legs.iter().map(|b| pair(a, b)).collect())
        .collect()
}

/// How closely two things move together, from -1 to 1. `None` with too little to say or no movement.
fn pearson(points: &[(f64, f64)]) -> Option<f64> {
    if points.len() < 2 {
        return None;
    }
    let count = points.len() as f64;
    let mean_a = points.iter().map(|p| p.0).sum::<f64>() / count;
    let mean_b = points.iter().map(|p| p.1).sum::<f64>() / count;
    let (mut together, mut var_a, mut var_b) = (0.0, 0.0, 0.0);
    for (a, b) in points {
        together += (a - mean_a) * (b - mean_b);
        var_a += (a - mean_a).powi(2);
        var_b += (b - mean_b).powi(2);
    }
    (var_a > 0.0 && var_b > 0.0).then(|| together / (var_a * var_b).sqrt())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn weights_read_by_symbol_and_what_a_row_leaves_out_goes_flat() {
        let symbols = ["BTCUSDT".to_owned(), "ETHUSDT".to_owned()];
        let table = parse_weights(
            &json!([["2024-01-02", {"btcusdt": 0.5, "ETHUSDT": -0.5}], [1000, {"ETHUSDT": 0.25}]]),
            &symbols,
        )
        .unwrap();
        // In time order, and just after their times, so they apply to that bar's close.
        assert_eq!(table[0], (1001, vec![0.0, 0.25]));
        assert_eq!(table[1].1, vec![0.5, -0.5]);
        let wrong = parse_weights(&json!([[0, {"XRPUSDT": 1.0}]]), &symbols).unwrap_err();
        assert!(wrong.contains("XRPUSDT"), "{wrong}");
        assert!(parse_weights(&json!([[0]]), &symbols).is_err());
        // `{}` alone is flat throughout; any other object isn't a table.
        assert_eq!(
            parse_weights(&json!({}), &symbols).unwrap(),
            vec![(0, vec![0.0; symbols.len()])]
        );
        assert!(parse_weights(&json!({"BTCUSDT": 1.0}), &symbols).is_err());
    }

    #[test]
    fn correlation_is_one_with_itself_and_minus_one_with_its_opposite() {
        let up = [(0.01, 0.02), (0.02, 0.04), (-0.01, -0.02)];
        assert!((pearson(&up).unwrap() - 1.0).abs() < 1e-9);
        let down = [(0.01, -0.02), (0.02, -0.04), (-0.01, 0.02)];
        assert!((pearson(&down).unwrap() + 1.0).abs() < 1e-9);
        assert_eq!(pearson(&[(0.01, 0.02)]), None);
        assert_eq!(pearson(&[(0.01, 0.0), (0.02, 0.0)]), None);
    }

    #[test]
    fn a_run_written_down_reads_back_as_it_was() {
        let leg = LegReport {
            symbol: "BTCUSDT".into(),
            fills: vec![Fill {
                ts: 5,
                bar: 1,
                side: Side::Sell,
                price: 100.0,
                qty: 2.0,
                fee: 0.5,
                maker: false,
                range_slip: 0.25,
            }],
            contribution: vec![0.0, 10.0],
            account_line: vec![1_000.0, 1_010.0],
            values: vec![0.0, 200.0],
            fees: 0.5,
            orders_worked: 1,
            bars_to_fill_average: 2.0,
            bars_to_fill_longest: 2,
            unfilled_at_end: 1.5,
            missing_bars: 3,
            ..LegReport::default()
        };
        let run = PortfolioRun {
            report: PortfolioReport {
                times: vec![10, 20],
                equity: vec![1_000.0, 1_010.0],
                starting_balance: 1_000.0,
                max_drawdown: -0.25,
                interest_paid: 7.0,
                legs: vec![leg],
                gross_exposure: vec![0.0, 0.2],
                net_exposure: vec![0.0, -0.2],
                liquidated_at: Some(20),
                notes: vec![Note {
                    text: "A note".into(),
                    warning: false,
                }],
                ..PortfolioReport::default()
            },
            figures: PortfolioFigures {
                net_return: 0.01,
                gross_return: Some(0.02),
                costs_paid: 3.0,
                costs: CostBreakdown {
                    taker_fees: 2.0,
                    impact: 0.5,
                    interest: 0.5,
                    ..CostBreakdown::default()
                },
                sharpe: Some(1.5),
                beta: Some(0.8),
                by_year: BTreeMap::from([(2024, 0.01)]),
                by_year_compounded: BTreeMap::from([(2024, Some(0.01)), (2025, None)]),
                benchmark_return: 0.5,
                benchmark: vec![1_000.0, 1_500.0],
                benchmark_after_funding: 0.4,
                weights_held_return: Some(0.3),
                by_hour: vec![1.0; 24],
                by_weekday: vec![2.0; 7],
                correlation: vec![vec![Some(1.0)]],
            },
            from_index: 1,
            opens: vec![0, 10],
            friction: true,
            next_open: true,
        };
        let dir = std::env::temp_dir().join(format!("portfolio-run-{}", std::process::id()));
        let (summary, whole) = written(&run, "run-1", 9, &json!({"label": "Mine"}));
        // Before costs less after is 10, but only 3 was paid: it says the gap's more than costs.
        assert!(
            summary["warnings"]
                .to_string()
                .contains("costs paid were 3.00"),
            "{summary}"
        );
        let mut explained = run.clone();
        explained.figures.costs_paid = 10.0;
        let (quiet, _) = written(&explained, "run-1", 9, &json!({}));
        assert!(
            !quiet["warnings"].to_string().contains("costs paid"),
            "{quiet}"
        );
        save(&dir, &whole, &summary, &json!({"label": "Mine"})).unwrap();
        let listed = list_saved(&dir);
        assert_eq!(listed[0].id, "run-1");
        assert!(listed[0].name.contains("Mine"), "{}", listed[0].name);

        let back = load_saved(&dir, "run-1").unwrap();
        let (a, b) = (&run.report, &back.report);
        assert_eq!(a.times, b.times);
        assert_eq!(a.equity, b.equity);
        assert_eq!(a.max_drawdown, b.max_drawdown);
        assert_eq!(a.interest_paid, b.interest_paid);
        assert_eq!(a.liquidated_at, b.liquidated_at);
        assert_eq!(a.notes, b.notes);
        assert_eq!(a.gross_exposure, b.gross_exposure);
        let (x, y) = (&a.legs[0], &b.legs[0]);
        assert_eq!(x.fills, y.fills);
        assert_eq!(x.account_line, y.account_line);
        assert_eq!(x.unfilled_at_end, y.unfilled_at_end);
        assert_eq!(x.missing_bars, y.missing_bars);
        assert_eq!(back.figures.by_year, run.figures.by_year);
        assert_eq!(
            back.figures.by_year_compounded,
            run.figures.by_year_compounded
        );
        assert_eq!(back.figures.correlation, run.figures.correlation);
        assert_eq!(back.figures.sharpe, run.figures.sharpe);
        assert_eq!(back.figures.costs.impact, 0.5);
        assert_eq!(back.figures.beta, Some(0.8));
        assert_eq!(back.figures.costs.taker_fees, 2.0);
        assert_eq!(back.figures.benchmark_after_funding, 0.4);
        assert_eq!(back.figures.weights_held_return, Some(0.3));
        assert_eq!(back.figures.by_weekday, vec![2.0; 7]);
        assert_eq!(back.opens, run.opens);
        assert!(back.next_open && back.friction);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_setup_survives_being_written_down() {
        let setup = PortfolioSetup {
            legs: vec![LegSetup {
                symbol: "BTCUSDT".into(),
                venue: None,
                weight: 0.5,
                strategy: Some(json!("ema-cross")),
            }],
            weights: Some(json!([["2024-01-01", {"BTCUSDT": 1.0}]])),
            rebalance: RebalanceSetup {
                every_bars: Some(24),
                drift_percent: None,
                min_trade_value: 10.0,
            },
            maintenance_percent: 1.0,
            use_chart_strategy: false,
        };
        let written = serde_json::to_string(&setup).unwrap();
        assert_eq!(
            serde_json::from_str::<PortfolioSetup>(&written).unwrap(),
            setup
        );
        let old: PortfolioSetup = serde_json::from_str("{}").unwrap();
        assert_eq!(old.maintenance_percent, DEFAULT_MAINTENANCE_PERCENT);
    }
}
