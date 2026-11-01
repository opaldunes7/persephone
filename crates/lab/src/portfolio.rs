//! Several markets on one account, through tools. The running is the engine's, and the figures are
//! `persephone_research::portfolio`'s, which the app reads too.

use std::{fs, sync::Arc, sync::atomic::Ordering};

use persephone_engine::PortfolioRequest;
use persephone_research::{
    portfolio::{self, LegSetup, PortfolioSetup, RebalanceSetup},
    robustness::pool,
};
use serde_json::{Value, json};

use crate::{
    Lab,
    data::MarketArgs,
    jobs::Job,
    new_id, now_ms,
    runs::settings_of,
    time_text,
    tools::{check_args, limit_of, page_of, text, tools},
};

const DEFAULT_ROWS: usize = 100;
const MOST_RUNS: usize = 5_000;
const COLUMNS: [&str; 17] = [
    "index",
    "run_id",
    "label",
    "net_return",
    "gross_return",
    "total_costs_paid",
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
        let workers = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .saturating_sub(1)
            .max(1);
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
