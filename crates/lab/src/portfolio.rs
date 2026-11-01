//! Several markets on one account, through tools. The running is the engine's, and the figures are
//! `persephone_research::portfolio`'s, which the app reads too.

use std::{fs, sync::Arc, sync::atomic::Ordering};

use persephone_engine::PortfolioRequest;
use persephone_research::{
    portfolio::{self, LegSetup, PortfolioSetup, RebalanceSetup},
    portfolio_tests::{self, Candidate, PickBy, ShiftedJob, ShiftedPortfolioJob, WalkJob},
    robustness::pool,
};
use serde_json::{Value, json};

use crate::{
    Lab,
    data::MarketArgs,
    jobs::{Job, check_id},
    new_id, now_ms,
    runs::{histogram_json, report_from, settings_of, walk_answer},
    time_text,
    tools::{check_args, limit_of, page_of, text, tools},
};

const DEFAULT_ROWS: usize = 100;
const MOST_RUNS: usize = 5_000;
const COLUMNS: [&str; 18] = [
    "index",
    "run_id",
    "label",
    "net_return",
    "gross_return",
    "total_costs_paid",
    "excess_return",
    "fees",
    "funding_paid",
    "max_drawdown",
    "sharpe",
    "benchmark_return",
    "turnover",
    "peak_margin_use",
    "closest_to_liquidation",
    "liquidated_at",
    "warnings",
    "error",
];

fn setup_of(args: &Value, weights: Option<Value>) -> Result<PortfolioSetup, String> {
    let legs = args
        .get("legs")
        .and_then(Value::as_array)
        .filter(|legs| !legs.is_empty())
        .ok_or("`legs` should list the markets, as [{\"symbol\": \"BTCUSDT\", \"weight\": 0.5}]")?;
    let legs = legs
        .iter()
        .map(|leg| {
            if let Some(object) = leg.as_object()
                && let Some(name) = object.keys().find(|name| {
                    !matches!(name.as_str(), "symbol" | "venue" | "weight" | "strategy")
                })
            {
                return Err(format!("`legs`: there's no `{name}` on a leg"));
            }
            Ok(LegSetup {
                symbol: text(leg, "symbol")
                    .map_err(|e| format!("`legs`: {e}"))?
                    .to_uppercase(),
                venue: leg
                    .get("venue")
                    .and_then(Value::as_str)
                    .map(str::to_uppercase),
                weight: leg.get("weight").and_then(Value::as_f64).unwrap_or(0.0),
                strategy: leg.get("strategy").cloned(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    if weights.is_none()
        && legs
            .iter()
            .any(|leg| leg.strategy.is_none() && leg.weight == 0.0)
    {
        return Err("`legs`: a leg with no strategy and no weight table needs a weight, or it would never trade".into());
    }
    let rebalance: RebalanceSetup = match args.get("rebalance") {
        Some(rule) => {
            if let Some(name) = rule.as_object().and_then(|rule| {
                rule.keys().find(|name| {
                    !matches!(
                        name.as_str(),
                        "every_bars" | "drift_percent" | "min_trade_value"
                    )
                })
            }) {
                return Err(format!("`rebalance`: there's no `{name}`"));
            }
            serde_json::from_value(rule.clone()).map_err(|e| format!("`rebalance`: {e}"))?
        }
        None => RebalanceSetup::default(),
    };
    Ok(PortfolioSetup {
        legs,
        weights,
        rebalance,
        maintenance_percent: args
            .get("maintenance_percent")
            .and_then(Value::as_f64)
            .unwrap_or(portfolio::DEFAULT_MAINTENANCE_PERCENT),
        use_chart_strategy: false,
    })
}

fn weights_of(args: &Value) -> Result<Option<Value>, String> {
    match args.get("weights") {
        None => Ok(None),
        Some(Value::String(path)) => {
            let read = fs::read_to_string(path)
                .map_err(|e| format!("`weights`: couldn't read {path}: {e}"))?;
            serde_json::from_str(&read)
                .map(Some)
                .map_err(|e| format!("`weights`: {path} isn't JSON: {e}"))
        }
        Some(table) => Ok(Some(table.clone())),
    }
}

fn workers() -> usize {
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .saturating_sub(1)
        .max(1)
}

impl Lab {
    fn prepare_portfolio(&self, args: &Value) -> Result<PortfolioRequest, String> {
        let weights = weights_of(args)?;
        let setup = setup_of(args, weights)?;
        let settings = settings_of(args)?;
        let mut markets = Vec::with_capacity(setup.legs.len());
        let mut strategies = Vec::with_capacity(setup.legs.len());
        let mut trade_from = 0;
        for leg in &setup.legs {
            let mut market_args = json!({"symbol": leg.symbol});
            for shared in ["bars", "from", "to", "settings"] {
                if let Some(value) = args.get(shared) {
                    market_args[shared] = value.clone();
                }
            }
            if let Some(venue) = &leg.venue {
                market_args["venue"] = json!(venue);
            }
            let market_args = MarketArgs::parse(&market_args)?;
            trade_from = market_args.from.unwrap_or(0);
            markets.push(
                self.load_market(&market_args)
                    .map_err(|e| format!("{}: {e}", leg.symbol))?
                    .into(),
            );
            strategies.push(
                leg.strategy
                    .as_ref()
                    .map(|strategy| self.resolve(strategy))
                    .transpose()
                    .map_err(|e| format!("{}: {e}", leg.symbol))?,
            );
        }
        portfolio::request(&setup, markets, strategies, &settings, trade_from)
    }

    pub(crate) fn run_portfolio(&self, args: &Value) -> Result<Value, String> {
        let request = self.prepare_portfolio(args)?;
        let friction = args
            .get("friction")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let found = portfolio::run(request, friction)?;
        let run_id = new_id("run");
        let (summary, run) = portfolio::written(&found, &run_id, now_ms(), args);
        portfolio::save(&self.paths.lab().join("runs"), &run, &summary, args)?;
        self.remember_run(&run_id, Arc::new(run));
        Ok(json!({"run_id": run_id, "summary": summary}))
    }

    pub(crate) fn run_portfolios(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let from_file = match args.get("runs").and_then(Value::as_str) {
            Some(path) => {
                let read = fs::read_to_string(path)
                    .map_err(|e| format!("`runs`: couldn't read {path}: {e}"))?;
                Some(
                    serde_json::from_str::<Value>(&read)
                        .map_err(|e| format!("`runs`: {path} isn't JSON: {e}"))?,
                )
            }
            None => None,
        };
        let items = from_file
            .as_ref()
            .or_else(|| args.get("runs"))
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or("`runs` should be a list of run_portfolio's arguments, or a JSON file's path")?;
        if items.len() > MOST_RUNS {
            return Err(format!(
                "`runs`: that's {} portfolios, more than the {MOST_RUNS} allowed in a call",
                items.len()
            ));
        }
        let shared = args.get("defaults").and_then(Value::as_object);
        let calls: Vec<Value> = items
            .iter()
            .map(|item| {
                let own = item
                    .as_object()
                    .ok_or_else(|| format!("`runs`: {item} isn't an object"))?;
                let mut call = shared.cloned().unwrap_or_default();
                call.extend(own.clone());
                Ok(Value::Object(call))
            })
            .collect::<Result<_, String>>()?;
        let workers = workers();
        job.total.store(calls.len(), Ordering::Relaxed);
        let all = tools();
        let tool = all
            .iter()
            .find(|tool| tool.name == "run_portfolio")
            .ok_or("run_portfolio is missing")?;
        let results = pool(
            &calls,
            workers,
            &job.stop,
            &|| {
                job.done.fetch_add(1, Ordering::Relaxed);
            },
            &|call| {
                let result = check_args(tool, call).and_then(|()| self.run_portfolio(call));
                self.log_call("run_portfolio", call, &result);
                result
            },
        );
        let (mut succeeded, mut failed, mut not_run) = (Vec::new(), Vec::new(), 0);
        for (index, result) in results.into_iter().enumerate() {
            match result {
                Some(Ok(run)) => succeeded.push((index, run)),
                Some(Err(error)) => failed.push((index, error)),
                None => not_run += 1,
            }
        }
        let net = |run: &Value| run["summary"]["net_return"].as_f64().unwrap_or(f64::MIN);
        succeeded.sort_by(|(_, a), (_, b)| net(b).total_cmp(&net(a)));
        let total = calls.len();
        let (done, errors) = (succeeded.len(), failed.len());
        let first_error = failed
            .first()
            .map(|(index, error)| json!({"index": index, "error": error}));
        let mut rows: Vec<Value> = succeeded
            .iter()
            .map(|(index, run)| {
                let summary = &run["summary"];
                json!([
                    index,
                    run["run_id"],
                    summary["label"],
                    summary["net_return"],
                    summary["gross_return"],
                    summary["total_costs_paid"],
                    summary["excess_return"],
                    summary["fees"],
                    summary["funding_paid"],
                    summary["max_drawdown"],
                    summary["sharpe"],
                    summary["benchmark_return"],
                    summary["turnover"],
                    summary["peak_margin_use"],
                    summary["closest_to_liquidation"],
                    summary["liquidated_at"],
                    summary["warnings"],
                    null,
                ])
            })
            .collect();
        rows.extend(failed.iter().map(|(index, error)| {
            let mut row = vec![Value::Null; COLUMNS.len()];
            row[0] = json!(index);
            row[COLUMNS.len() - 1] = json!(error);
            Value::Array(row)
        }));
        rows.truncate(limit_of(args, DEFAULT_ROWS));
        Ok(json!({
            "columns": COLUMNS,
            "rows": rows,
            "total": total,
            "succeeded": done,
            "failed": errors,
            "not_run": not_run,
            "first_error": first_error,
        }))
    }

    /// The same run's book at other times, for a portfolio or one market.
    pub(crate) fn run_shifted(&self, args: &Value, job: &Arc<Job>) -> Result<Value, String> {
        let id = text(args, "run_id")?;
        let run = self.load_run(id)?;
        let draws = args
            .get("runs")
            .and_then(Value::as_u64)
            .unwrap_or(200)
            .clamp(1, MOST_RUNS as u64) as usize;
        let seed = args.get("seed").and_then(Value::as_u64).unwrap_or(0);
        job.total.store(draws, Ordering::Relaxed);
        let progress = |done| job.done.store(done, Ordering::Relaxed);
        let found = if run["kind"] == "portfolio" {
            let saved = portfolio::load_saved(&self.paths.lab().join("runs"), id)?;
            ShiftedPortfolioJob {
                report: Arc::new(saved.report),
                request: self.prepare_portfolio(&run["request"])?,
                from_index: saved.from_index,
                runs: draws,
                seed,
                workers: workers(),
                stop: Arc::clone(&job.stop),
            }
            .run(&progress)?
        } else {
            let (request, from_index, _) = self.prepare(&run["request"])?;
            ShiftedJob {
                report: Arc::new(report_from(&run)),
                request,
                from_index,
                runs: draws,
                seed,
                workers: workers(),
                stop: Arc::clone(&job.stop),
            }
            .run(&progress)?
        };
        Ok(json!({
            "runs": found.runs,
            "changes": found.trades,
            "warning": (found.trades < 2).then_some("It only changed once, so moving it says almost nothing about its timing."),
            "actual_return": found.actual_return,
            "actual_fell": found.actual_fell,
            "share_matching_return": found.share_matching_return,
            "share_matching_fall": found.share_matching_fall,
            "median": found.median,
            "low": found.low,
            "high": found.high,
            "return_chart": histogram_json(&found.returns, found.actual_return),
            "fall_chart": histogram_json(&found.falls, found.actual_fell),
        }))
    }

    /// A portfolio's account returns, drawn again in blocks.
    pub(crate) fn run_portfolio_monte_carlo(&self, args: &Value) -> Result<Value, String> {
        let saved = self.saved_portfolio(args)?;
        let number =
            |name: &str, default: u64| args.get(name).and_then(Value::as_u64).unwrap_or(default);
        let found = portfolio_tests::monte_carlo(
            &saved.report,
            saved.from_index,
            number("runs", 1_000).clamp(1, 100_000) as usize,
            number("block_bars", 100) as usize,
            args.get("ruin_percent")
                .and_then(Value::as_f64)
                .unwrap_or(50.0)
                / 100.0,
            number("seed", 0),
        )?;
        let mut answer = json!({
            "runs": found.runs,
            "block_bars": found.block_bars,
            "actual_return": found.actual_return,
            "actual_fell": found.actual_fell,
            "median": found.median,
            "low": found.low,
            "high": found.high,
            "ruin_share": found.ruin_share,
            "losing_share": found.losing_share,
            "share_matching_return": found.returns.iter().filter(|r| **r >= found.actual_return).count() as f64 / found.runs.max(1) as f64,
            "return_chart": histogram_json(&found.returns, found.actual_return),
            "fall_chart": histogram_json(&found.falls, found.actual_fell),
            "stride": found.stride,
            "median_line": found.median_line,
            "low_line": found.low_line,
            "high_line": found.high_line,
            "actual_line": found.actual_line,
        });
        if args
            .get("include_lines")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            answer["lines"] = json!(found.lines);
        }
        Ok(answer)
    }

    /// Picking among setups on one stretch, and trading the pick on the unseen one after.
    pub(crate) fn run_portfolio_walk_forward(
        &self,
        args: &Value,
        job: &Arc<Job>,
    ) -> Result<Value, String> {
        let from_file = match args.get("candidates").and_then(Value::as_str) {
            Some(path) => {
                let read = fs::read_to_string(path)
                    .map_err(|e| format!("`candidates`: couldn't read {path}: {e}"))?;
                Some(
                    serde_json::from_str::<Value>(&read)
                        .map_err(|e| format!("`candidates`: {path} isn't JSON: {e}"))?,
                )
            }
            None => None,
        };
        let items = from_file
            .as_ref()
            .or_else(|| args.get("candidates"))
            .and_then(Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or(
                "`candidates` should be a list of run_portfolio's arguments, or a JSON file's path",
            )?;
        let shared = args.get("defaults").and_then(Value::as_object);
        let mut candidates = Vec::with_capacity(items.len());
        for (at, item) in items.iter().enumerate() {
            let own = item
                .as_object()
                .ok_or_else(|| format!("`candidates`: {item} isn't an object"))?;
            let mut call = shared.cloned().unwrap_or_default();
            call.extend(own.clone());
            let call = Value::Object(call);
            candidates.push(Candidate {
                label: call
                    .get("label")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("Setup {}", at + 1), ToOwned::to_owned),
                values: Vec::new(),
                request: self.prepare_portfolio(&call)?,
            });
        }
        let settings = settings_of(&Value::Object(shared.cloned().unwrap_or_default()))?;
        let walk = WalkJob {
            candidates,
            windows: args
                .get("windows")
                .and_then(Value::as_u64)
                .unwrap_or(5)
                .max(1) as usize,
            ratio: args
                .get("train_ratio")
                .and_then(Value::as_f64)
                .filter(|ratio| *ratio > 0.0)
                .unwrap_or(settings.walk_ratio),
            warm_up: settings.warm_up_bars as usize,
            pick_by: match args.get("pick_by").and_then(Value::as_str) {
                None | Some("return") => PickBy::Return,
                Some("sharpe") => PickBy::Sharpe,
                Some(other) => return Err(format!("`pick_by`: `{other}` isn't return or sharpe")),
            },
            workers: workers(),
            stop: Arc::clone(&job.stop),
        };
        job.total.store(walk.total(), Ordering::Relaxed);
        let found = walk.run(&|done| job.done.store(done, Ordering::Relaxed))?;
        Ok(walk_answer(&found))
    }

    fn saved_portfolio(&self, args: &Value) -> Result<portfolio::PortfolioRun, String> {
        let id = text(args, "run_id")?;
        check_id(id)?;
        portfolio::load_saved(&self.paths.lab().join("runs"), id)
    }

    fn load_portfolio(&self, args: &Value) -> Result<Arc<Value>, String> {
        let id = text(args, "run_id")?;
        let run = self.load_run(id)?;
        if run["kind"] != "portfolio" {
            return Err(format!(
                "`{id}` is a backtest of one market; get_equity and the like read it"
            ));
        }
        Ok(run)
    }

    pub(crate) fn get_portfolio(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_portfolio(args)?;
        Ok(json!({
            "run_id": run["run_id"],
            "summary": run["summary"],
            "legs": run["legs"],
            "correlation": run["correlation"],
            "notes": run["notes"],
        }))
    }

    pub(crate) fn get_portfolio_series(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_portfolio(args)?;
        let empty = Vec::new();
        let times = run["times"].as_array().unwrap_or(&empty);
        let legs = run["legs"].as_array().unwrap_or(&empty);
        let names: Vec<String> = legs
            .iter()
            .flat_map(|leg| {
                let symbol = leg["symbol"].as_str().unwrap_or_default();
                [format!("{symbol}_value"), format!("{symbol}_made")]
            })
            .collect();
        let mut columns = vec![
            "ts",
            "time",
            "equity",
            "benchmark",
            "gross_exposure",
            "net_exposure",
        ];
        columns.extend(names.iter().map(String::as_str));
        // The bars before the span only warm strategies up.
        let from = run["from_index"].as_u64().unwrap_or(0) as usize;
        let total = times.len().saturating_sub(from);
        Ok(page_of(&columns, total, args, DEFAULT_ROWS, |row| {
            let at = from + row;
            let mut row = vec![
                times[at].clone(),
                json!(times[at].as_u64().map(time_text)),
                run["equity"][at].clone(),
                run["benchmark"][at].clone(),
                run["gross_exposure"][at].clone(),
                run["net_exposure"][at].clone(),
            ];
            for leg in 0..legs.len() {
                row.push(run["values"][leg][at].clone());
                row.push(run["contributions"][leg][at].clone());
            }
            Value::Array(row)
        }))
    }

    pub(crate) fn get_portfolio_fills(&self, args: &Value) -> Result<Value, String> {
        let run = self.load_portfolio(args)?;
        let symbol = text(args, "symbol")?.to_uppercase();
        let empty = Vec::new();
        let legs = run["legs"].as_array().unwrap_or(&empty);
        let leg = legs
            .iter()
            .position(|leg| leg["symbol"] == symbol.as_str())
            .ok_or_else(|| format!("`symbol`: {symbol} isn't one of this run's markets"))?;
        let fills = run["leg_fills"][leg].as_array().unwrap_or(&empty);
        Ok(page_of(
            &["ts", "bar", "side", "price", "size", "fee", "range_slip"],
            fills.len(),
            args,
            DEFAULT_ROWS,
            |at| fills[at].clone(),
        ))
    }
}
