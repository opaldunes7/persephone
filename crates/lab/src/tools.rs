//! The list of tools, and the one place every way into Lab calls them through.

use std::{fs::OpenOptions, io::Write as _, sync::Arc};

use serde_json::{Map, Value, json};

use crate::{Lab, now_ms};

/// One argument.
#[derive(Clone, Copy, Debug)]
pub struct Arg {
    pub name: &'static str,
    /// A JSON type, or `time` for what [`crate::parse_time`] reads.
    pub kind: &'static str,
    pub what: &'static str,
    pub required: bool,
}

const fn arg(name: &'static str, kind: &'static str, what: &'static str) -> Arg {
    Arg {
        name,
        kind,
        what,
        required: true,
    }
}

const fn opt(name: &'static str, kind: &'static str, what: &'static str) -> Arg {
    Arg {
        name,
        kind,
        what,
        required: false,
    }
}

/// One tool.
#[derive(Clone, Debug)]
pub struct Tool {
    pub name: &'static str,
    pub what: &'static str,
    pub args: Vec<Arg>,
    /// What's always in its answer, once finished.
    pub returns: &'static [&'static str],
    /// Slow enough to run as a job, with `wait_seconds`.
    pub job: bool,
}

impl Tool {
    /// Its arguments as JSON Schema.
    #[must_use]
    pub fn input_schema(&self) -> Value {
        let mut properties = serde_json::Map::new();
        let args = self.args.iter().chain(self.job.then_some(&WAIT));
        for arg in args {
            let kind = if arg.kind == "time" {
                json!(["string", "integer"])
            } else if arg.kind == "list_or_path" {
                json!(["array", "string"])
            } else if arg.kind == "list" {
                json!("array")
            } else {
                json!(arg.kind)
            };
            let mut schema = json!({"type": kind, "description": arg.what});
            // Once is enough for an agent to learn its shape, and the tool list is read every session.
            if arg.name == "settings" && self.name == "run_backtest" {
                schema["properties"] = settings_schema();
            }
            properties.insert(arg.name.to_owned(), schema);
        }
        let required: Vec<&str> = self
            .args
            .iter()
            .filter(|arg| arg.required)
            .map(|arg| arg.name)
            .collect();
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    }
}

/// `settings`' fields, so an agent has their exact shape from the tool list alone.
fn settings_schema() -> Value {
    let section = |value: Value, what: &str| {
        json!({
            "type": "object",
            "description": what,
            "properties": {"on": {"type": "boolean"}, "value": value},
        })
    };
    let number = json!({"type": "number"});
    let whole = json!({"type": "integer"});
    json!({
        "stock": {"type": "boolean", "description": "false turns the sections below off at once."},
        "capital": {"type": "number", "description": "Starting cash, in the quote currency."},
        "commission": section(
            json!({"type": "object", "description": "{\"percent\": 0.1} of each fill, or {\"per-order\": 1.5} flat."}),
            "Fees instead of the market's own.",
        ),
        "slippage": section(whole.clone(), "Ticks every market order fills worse."),
        "slippage_percent": section(number.clone(), "Percent of the price every market order fills worse, as 0.01 for 1 basis point."),
        "limit": section(
            json!({"type": "object", "description": "{\"offset_percent\": 0.05, \"expires_bars\": 3, \"through_percent\": 0.01}"}),
            "Send orders as limits: how far from the close they rest, and how many bars before they're cancelled. They fill only when price trades through them, by through_percent if given, standing in for the queue ahead.",
        ),
        "stop_loss_percent": section(number.clone(), "A stop this far from the entry, resting from the bar after it."),
        "trailing_stop_percent": section(number.clone(), "A real stop this far from the best price since entry, moved at each close, so it only tightens. With stop_loss_percent too, whichever is nearer goes first."),
        "decision_log": {"type": "boolean", "description": "Keep a row for each bar, read with get_decisions: what was asked, what was held, what the rules made of it, and why no order went out. Off by default, as it's large."},
        "take_profit_percent": section(number.clone(), "A target this far from the entry, resting from the bar after it."),
        "borrow_percent_per_year": section(number.clone(), "What borrowing costs a short, as a percentage of its value a year."),
        "impact_percent": section(number.clone(), "How far a market order fills worse when it's all of its bar's volume; it moves with the square root of its share."),
        "max_gross_percent": section(number.clone(), "Portfolios only: the most gross exposure, long plus short, as a percentage of the account times the leverage, as 100 for no more than the account allows. New or growing orders that would take the book past it are cut, all by the same share, and `orders_capped` counts them; what's held isn't sold to get under it unless the book is rebalanced. Off by default, when legs sized one by one can stack past the account."),
        "max_participation_percent": section(number.clone(), "The most of a bar's volume an order can take. What's left goes out on the next bars at their prices, a resting limit fills no more than this of each bar, and a stop takes this and leaves the rest to the closes after."),
        "range_slippage_percent": section(number.clone(), "What a market order or stop pays on top, as a percentage of a bar's high to low, times how many times wider that bar is than the median of the last 100. A market order reads the bar just closed, and a stop the bar it fills in. Sweep it with run_backtests to stress a strategy."),
        "latency_jitter": section(whole.clone(), "Milliseconds orders take on top of latency on average, drawn for each from an exponential distribution. Only fills on ticks can show it."),
        "latency_seed": {"type": "integer", "description": "Which draws of jitter a run uses, so it can be repeated."},
        "range_look_back": {"type": "integer", "description": "How many bars back range_slippage_percent judges a bar's usual range from. Default 100."},
        "impact_exponent": {"type": "number", "description": "How steeply impact grows with an order's share of its bar's volume. 0.5, the default, is the square root; 1 is in proportion."},
        "volume_warning_percent": {"type": "number", "description": "With impact on, orders over this share of their bar's volume get a warning. Default 10."},
        "fill_warning_percent": {"type": "number", "description": "Fewer than this share of limit orders filling gets a warning, from 5 orders up. Default 50."},
        "cash_interest_percent_per_year": section(number.clone(), "What cash below zero costs in a portfolio, as a percentage a year. A book holding more than the account borrows free without it."),
        "warm_up_bars": {"type": "integer", "description": "How many bars before `from` strategies warm up on. Default 500. Raise it for look-backs longer than that."},
        "hold_out_percent": {"type": "number", "description": "How much of the span an optimization with hold_out picks on, as a percentage. Default 70."},
        "walk_ratio": {"type": "number", "description": "How many times longer than its unseen stretch a walk-forward window picks on. Default 3. train_ratio on the call wins."},
        "no_costs": {"type": "boolean", "description": "Nothing to pay at all: no fees, funding, slippage, spread, impact, borrowing or interest, whatever else says. stock: false leaves the market's own fees and funding on."},
        "compound": {"type": "boolean", "description": "Size positions from the account as it is now, rather than as it started."},
        "fill_timing": {"enum": ["close", "next_open"], "description": "When orders fill on the bars: at the close they were decided on, or at the next bar's open. next_open needs time bars, and can't go with latency or precision high."},
        "spread": section(whole.clone(), "Ticks of spread assumed for bars and trades."),
        "latency": section(whole, "Milliseconds before orders reach the venue."),
        "leverage": section(number, "A multiple of position size, 1 or more."),
        "direction": section(
            json!({"enum": ["long-only", "short-only"]}),
            "Trade one way; the other side's signals close instead.",
        ),
        "invert": {"type": "boolean", "description": "Take the opposite of every signal."},
        "session": section(
            json!({"type": "object", "properties": {
                "days": {"type": "array", "description": "Seven booleans, Monday first."},
                "from": {"type": "integer", "description": "Minutes after midnight."},
                "to": {"type": "integer", "description": "Minutes after midnight."},
                "zone": {"type": "string", "description": "UTC, -05:00 or America/New_York."},
                "close_after": {"type": "boolean", "description": "Close when it ends."},
            }}),
            "When positions may open.",
        ),
        "precision": {"enum": ["standard", "high"], "description": "high fills on ticks or 1-minute bars."},
    })
}

const LABEL: Arg = opt(
    "label",
    "string",
    "A name for grouping runs, saved with the run for finding it with list_runs.",
);
const INCLUDE_LINES: Arg = opt(
    "include_lines",
    "boolean",
    "Add every line the app's fan draws to `chart`, not just the few that sum it up. Costs more to read.",
);
const FRICTION: Arg = opt(
    "friction",
    "boolean",
    "false leaves out gross_return, total_costs_paid and break_even_cost_per_fill, which take a second backtest with costs off. Default true.",
);
const WAIT: Arg = opt(
    "wait_seconds",
    "number",
    "How long to wait before answering with a job to check on instead. Default 30.",
);

const SYMBOL: Arg = arg("symbol", "string", "The market's symbol, like BTCUSDT.");
const VENUE: Arg = opt(
    "venue",
    "string",
    "The venue of a market in the local store, from list_markets. Without one it's Binance spot.",
);
const BARS: Arg = opt(
    "bars",
    "string",
    "How bars are cut: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M; or 100t for every 100 ticks and 2.5v by volume, on local markets. Default 1h.",
);
const FROM: Arg = opt(
    "from",
    "time",
    "Start, in UTC: 2024-01-31 or 2024-01-31 09:30.",
);
const TO: Arg = opt("to", "time", "End (not included), in UTC.");
const CURSOR: Arg = opt(
    "cursor",
    "string",
    "Where to carry on, from the last page's next_cursor.",
);
const LIMIT: Arg = opt(
    "limit",
    "integer",
    "The most to return in this page, up to 50,000. Default 100, to keep answers small.",
);
const STRATEGY: Arg = arg(
    "strategy",
    "object",
    "{\"builtin\": \"ema-cross\"}, {\"plugin\": \"id\"}, {\"logic\": {...tree...}} or {\"positions\": [[ts, 1], [ts, 0]]} (or a JSON file's path), each with optional \"params\": {\"name\": value}.",
);
const SETTINGS: Arg = opt(
    "settings",
    "object",
    "Costs and rules, as in run_backtest; only what differs from the defaults is needed. get_settings gives the defaults as exact JSON.",
);
const RUN_ID: Arg = arg("run_id", "string", "A run, from run_backtest or list_runs.");

fn market_args() -> Vec<Arg> {
    vec![SYMBOL, VENUE, BARS, FROM, TO]
}

fn test_args() -> Vec<Arg> {
    let mut args = vec![STRATEGY];
    args.extend(market_args());
    args.push(SETTINGS);
    args
}

fn with(mut args: Vec<Arg>, more: &[Arg]) -> Vec<Arg> {
    args.extend_from_slice(more);
    args
}

/// Every tool there is.
#[must_use]
pub fn tools() -> Vec<Tool> {
    let tool = |name, what, args, returns| Tool {
        name,
        what,
        args,
        returns,
        job: false,
    };
    let job = |name, what, args, returns| Tool {
        name,
        what,
        args,
        returns,
        job: true,
    };
    vec![
        tool(
            "get_guide",
            "How to use Lab, written for agents. Read this first: without a topic it lists the topics, each short. They include backtests (how fills, sizing and warm-up work), logic, settings, units and data.",
            vec![opt(
                "topic",
                "string",
                "A section's name, or `all` for the whole guide.",
            )],
            &["guide", "topics"],
        ),
        tool(
            "get_settings",
            "The default settings as exact JSON, ready to edit and pass as `settings`.",
            vec![],
            &["settings"],
        ),
        tool(
            "list_markets",
            "Markets in the local store. Binance spot symbols work anywhere a symbol does, without being listed.",
            vec![],
            &["local", "binance"],
        ),
        tool(
            "get_instrument",
            "A market's details: quote currency, precision and fees.",
            vec![SYMBOL, VENUE],
            &[
                "symbol",
                "venue",
                "base",
                "quote",
                "price_precision",
                "size_precision",
                "fees",
            ],
        ),
        tool(
            "get_funding",
            "A perpetual's funding rates, in pages: when each was paid, and the rate as a fraction. A long pays its value times a positive rate, and a short receives it.",
            vec![
                SYMBOL,
                opt("from", "time", "Start, in UTC. Default a year back."),
                opt("to", "time", "End (not included), in UTC."),
                CURSOR,
                LIMIT,
            ],
            &["columns", "rows", "next_cursor"],
        ),
        tool(
            "get_news",
            "Headlines the app has collected from public feeds, newest first, in pages: when each was published, its source, title, how much it may matter, and its link. Only from when the app started collecting (`collected_since`); feeds keep their last few dozen items only, so older spans have none. Times are when each was published, not when the news broke.",
            vec![
                opt("from", "time", "Start, in UTC."),
                opt("to", "time", "End (not included), in UTC."),
                opt("query", "string", "Only those whose title has these words."),
                opt(
                    "symbol",
                    "string",
                    "The market they're judged against: one naming it is at least medium.",
                ),
                opt(
                    "importance",
                    "string",
                    "The least: low (all, the default), medium or high.",
                ),
                CURSOR,
                LIMIT,
            ],
            &["columns", "rows", "next_cursor", "total", "collected_since"],
        ),
        tool(
            "get_history_range",
            "When a market's history starts and ends: the opens of its first and last closed bars, \
             and when that last bar closed.",
            vec![SYMBOL, VENUE, BARS],
            &[
                "first",
                "last",
                "last_close",
                "first_ts",
                "last_ts",
                "last_close_ts",
            ],
        ),
        tool(
            "get_bars",
            "A market's bars, in pages, oldest first. Without `from`, its latest bars.",
            with(market_args(), &[CURSOR, LIMIT]),
            &["columns", "rows", "next_cursor"],
        ),
        tool(
            "get_ticks",
            "A local market's trades or quotes, in pages.",
            vec![
                SYMBOL,
                arg("venue", "string", "The local market's venue."),
                arg("from", "time", "Start, in UTC."),
                TO,
                CURSOR,
                LIMIT,
            ],
            &["kind", "columns", "rows", "next_cursor"],
        ),
        job(
            "export_data",
            "Writes bars or ticks to a file, for large pulls and training outside Persephone.",
            with(
                market_args(),
                &[
                    opt(
                        "ticks",
                        "boolean",
                        "Ticks instead of bars, on a local market.",
                    ),
                    opt("format", "string", "csv (the default) or parquet."),
                    opt(
                        "path",
                        "string",
                        "Where to write it. Default: Lab's exports folder.",
                    ),
                ],
            ),
            &["path", "rows", "format", "columns"],
        ),
        job(
            "import_data",
            "Imports CSV files of ticks or bars into the local store, working out their layout as the app does.",
            vec![
                arg("files", "array", "Paths of the files."),
                SYMBOL,
                arg("venue", "string", "The venue to file them under."),
                opt(
                    "base",
                    "string",
                    "Base currency; worked out from the symbol if left out.",
                ),
                opt(
                    "quote",
                    "string",
                    "Quote currency; worked out from the symbol if left out.",
                ),
                opt("fee", "number", "Fee rate per fill (0.001 is 0.1%)."),
            ],
            &["market", "imported", "skipped", "problems"],
        ),
        job(
            "fetch_archive",
            "Downloads Binance's monthly or daily trade archives into the local store.",
            vec![
                SYMBOL,
                arg(
                    "from",
                    "string",
                    "First period: 2024-01 for a month, 2024-01-31 for a day.",
                ),
                opt("to", "string", "Last period. Default: just `from`."),
                opt(
                    "all_trades",
                    "boolean",
                    "Every trade, not the aggregated ones.",
                ),
            ],
            &["market", "imported", "skipped", "files"],
        ),
        tool(
            "list_indicators",
            "Built-in and plugin indicators, with their parameters and outputs.",
            vec![],
            &["indicators", "plugin_errors"],
        ),
        job(
            "compute_indicator",
            "An indicator's values over a market's bars.",
            with(
                market_args(),
                &[
                    arg("indicator", "string", "An id from list_indicators."),
                    opt(
                        "params",
                        "object",
                        "{\"name\": value}; the rest keep their defaults.",
                    ),
                    CURSOR,
                    LIMIT,
                ],
            ),
            &["columns", "rows", "next_cursor", "total"],
        ),
        tool(
            "list_models",
            "ONNX models in the models folder.",
            vec![],
            &["models", "folder"],
        ),
        tool(
            "run_model",
            "Runs an ONNX model on an input.",
            vec![
                arg("model", "string", "A name from list_models."),
                arg("shape", "array", "The input's shape, like [1, 4]."),
                arg("data", "array", "The input's numbers, row by row."),
            ],
            &["outputs"],
        ),
        tool(
            "list_strategies",
            "Built-in and plugin strategies, with their parameters.",
            vec![],
            &["strategies", "plugin_errors"],
        ),
        tool(
            "describe_strategy",
            "One strategy's parameters, with their defaults and limits. Works for a logic tree too.",
            vec![STRATEGY],
            &["name", "description", "params", "values"],
        ),
        tool(
            "validate_logic",
            "Checks a logic tree without running it, saying where it's wrong and what would be right. Also says what a tree can be made of.",
            vec![opt(
                "logic",
                "object",
                "The tree. Without one, this only describes what's allowed.",
            )],
            &["valid", "error", "allowed"],
        ),
        tool(
            "install_plugin",
            "Adds a plugin to the plugins folder, where the app sees it too: a compiled one by `path`, or a program answering over a socket by `name` and `address`. Won't replace one of the same name.",
            vec![
                opt("path", "string", "A compiled plugin's .wasm file."),
                opt(
                    "name",
                    "string",
                    "What to call a program answering over a socket.",
                ),
                opt(
                    "address",
                    "string",
                    "Where it listens, as tcp://127.0.0.1:7878 or unix:///tmp/model.sock. It has to be running. The guide's `extending` topic has what it must answer.",
                ),
            ],
            &["id", "kind", "path"],
        ),
        job(
            "run_backtest",
            "Backtests any strategy on any span with any settings. The run is saved, so its details can be read by run_id. Returns and falls are fractions, so 0.05 is 5%.",
            with(test_args(), &[LABEL, FRICTION]),
            &["run_id", "summary"],
        ),
        job(
            "run_portfolio",
            "Backtests several markets on one account: they share cash and margin, and everything is worked out on one timeline. Each leg runs its own strategy at a weight, holds its weight, or follows a weight table. The guide's `portfolios` topic has how.",
            vec![
                arg(
                    "legs",
                    "list",
                    "The markets: [{\"symbol\": \"BTCUSDT\", \"weight\": 0.5, \"strategy\": ..., \"venue\": ...}]. With a strategy, weight scales its position size, and it must be above 0. Without one, it's the signed share of the capital to hold. A symbol ending -PERP pays funding.",
                ),
                opt(
                    "weights",
                    "list_or_path",
                    "A weight table instead of strategies: [[time, {\"BTCUSDT\": 0.5, \"ETHUSDT\": -0.5}], ...], or a JSON file holding one. Each row states the whole book as signed shares of the account, and a market it leaves out goes flat. A row stamped with a bar's open applies at that bar's close.",
                ),
                opt(
                    "rebalance",
                    "object",
                    "{\"every_bars\": 24, \"drift_percent\": 5, \"min_trade_value\": 100}: go back to the weights every so many bars, or when a leg is this many points of the account away, skipping trades worth less than this. Rebalancing sizes from the account as it is now.",
                ),
                opt(
                    "maintenance_percent",
                    "number",
                    "What a position needs to stay open, as a percentage of its value. Everything is liquidated at once when the account falls to it. Default 0.5.",
                ),
                opt(
                    "bars",
                    "string",
                    "A timeframe every leg shares, as 1h or 1d. Default 1h.",
                ),
                opt("from", "time", "Where the span starts."),
                opt("to", "time", "Where it ends."),
                SETTINGS,
                LABEL,
                FRICTION,
            ],
            &["run_id", "summary"],
        ),
        job(
            "run_portfolios",
            "Many portfolios in one call, in parallel, to sweep weights, rebalancing or costs. Every run is saved. One that fails doesn't stop the rest.",
            vec![
                arg(
                    "runs",
                    "list_or_path",
                    "A list of arguments as run_portfolio takes, or a JSON file holding one.",
                ),
                opt(
                    "defaults",
                    "object",
                    "Arguments every run shares. A run's own win: `settings` merge key by key, and so do `strategy.params` for the same strategy; a different strategy keeps only the defaults' `Size %`.",
                ),
                opt(
                    "variants_tried",
                    "integer",
                    "How many variants you've tried in all, counting every setting, rule, span and market before this call as well as the runs in it. The deflated Sharpe ratio counts the larger of this and the runs here, and `variants_tried` says which. Default: the runs here.",
                ),
                LIMIT,
            ],
            &[
                "columns",
                "rows",
                "total",
                "succeeded",
                "failed",
                "first_error",
                "not_run",
                "deflated_sharpe",
                "variants_tried",
            ],
        ),
        job(
            "run_shifted",
            "A run's positions or a portfolio's weights moved along the span by random amounts, wrapping round, so what's held and for how long stay the same and only the timing is lost. If those do as well, the timing is worth nothing. Works on any run, of one market or a portfolio.",
            vec![
                RUN_ID,
                opt("runs", "number", "How many shifts. Default 200."),
                opt("seed", "number", "Which shifts; the same gives the same."),
                opt(
                    "variants_tried",
                    "integer",
                    "How many variants this run was picked from, counting every setting, rule and span you tried. `share_matching_adjusted` is then the chance that one of that many worthless tries would have done as well. Default 1.",
                ),
            ],
            &[
                "runs",
                "actual_return",
                "share_matching_return",
                "median",
                "low",
                "high",
                "return_chart",
                "fall_chart",
                "actual_fell",
                "changes",
                "share_matching_fall",
                "warning",
                "variants_tried",
                "share_matching_adjusted",
            ],
        ),
        job(
            "run_portfolio_monte_carlo",
            "A portfolio's account returns drawn again in blocks along its timeline, keeping how its markets moved together. Says how wide its result and worst fall could have been. Fills and costs aren't run again.",
            vec![
                RUN_ID,
                opt("runs", "number", "How many draws. Default 1000."),
                opt(
                    "block_bars",
                    "number",
                    "How many bars a block is. Default 100. Longer keeps more of how returns follow each other.",
                ),
                opt(
                    "ruin_percent",
                    "number",
                    "The fall counted as ruin. Default 50.",
                ),
                opt("seed", "number", "Which draws; the same gives the same."),
                opt(
                    "include_lines",
                    "boolean",
                    "Add up to 200 of the drawn lines. Costs more to read.",
                ),
            ],
            &[
                "runs",
                "median",
                "low",
                "high",
                "ruin_share",
                "losing_share",
                "return_chart",
                "fall_chart",
                "median_line",
                "low_line",
                "high_line",
                "actual_line",
                "actual_fell",
                "actual_return",
                "block_bars",
                "share_matching_return",
                "stride",
            ],
        ),
        job(
            "run_portfolio_walk_forward",
            "Each window picks the best of the setups you give on its stretch, and trades it on the unseen one after, sliding along. The fair test of whether picking among portfolios helps. Setups can be anything run_portfolio takes, weight tables too. `headline` is the fair comparison: the unseen windows compounded one after another against buy_and_hold_whole, the market held once over the same span. `unseen_return` and `buy_and_hold_return` add up windows each started from the same capital, so they aren't what an account would have made.",
            vec![
                arg(
                    "candidates",
                    "list_or_path",
                    "The setups: run_portfolio's arguments, each with a `label`, or a JSON file holding them. They should trade the same markets.",
                ),
                opt(
                    "defaults",
                    "object",
                    "Arguments every setup shares, such as bars, from, to and settings. A setup's own win: `settings` merge key by key, and so do `strategy.params` for the same strategy; a different strategy keeps only the defaults' `Size %`.",
                ),
                opt(
                    "windows",
                    "number",
                    "How many times to slide along. Default 5.",
                ),
                opt(
                    "train_ratio",
                    "number",
                    "How many times longer than its unseen stretch a window picks on. Default walk_ratio in settings, 3.",
                ),
                opt(
                    "variants_tried",
                    "integer",
                    "How many variants you've tried in all, counting every setting, rule, span and market before this call as well as the runs in it. The deflated Sharpe ratio counts the larger of this and the runs here, and `variants_tried` says which. Default: the runs here.",
                ),
                opt("pick_by", "string", "`return` (default) or `sharpe`."),
            ],
            &[
                "windows",
                "unseen_return",
                "buy_and_hold_return",
                "profitable_windows",
                "deepest_fall",
                "chart",
                "buy_and_hold_compounded",
                "buy_and_hold_whole",
                "test_bars",
                "trades",
                "train_bars",
                "unseen_compounded",
                "unseen_sharpe",
                "headline",
                "variants_tried",
            ],
        ),
        job(
            "run_backtests",
            "Many backtests in one call, in parallel: much quicker for an agent than one call each. Every run is saved, so all the tools that take a run_id work on them. One that fails doesn't stop the rest.",
            vec![
                arg(
                    "runs",
                    "list_or_path",
                    "A list of arguments as run_backtest takes, up to 5,000, or a JSON file holding one.",
                ),
                opt(
                    "defaults",
                    "object",
                    "Arguments every run shares, such as bars, from, to and settings. A run's own win: `settings` merge key by key, and so do `strategy.params` for the same strategy; a different strategy keeps only the defaults' `Size %`.",
                ),
                opt(
                    "variants_tried",
                    "integer",
                    "How many variants you've tried in all, counting every setting, rule, span and market before this call as well as the runs in it. The deflated Sharpe ratio counts the larger of this and the runs here, and `variants_tried` says which. Default: the runs here.",
                ),
                opt(
                    "limit",
                    "integer",
                    "How many runs to list, best return first. Default 100.",
                ),
            ],
            &[
                "columns",
                "rows",
                "total",
                "succeeded",
                "failed",
                "not_run",
                "first_error",
                "deflated_sharpe",
                "variants_tried",
                "warnings",
            ],
        ),
        job(
            "run_monte_carlo",
            "Draws a run's trades at random many times, to see the range of results they could have given. Returns, falls and ruin are fractions.",
            vec![
                RUN_ID,
                opt("runs", "integer", "How many draws. Default 1,000."),
                opt(
                    "ruin_percent",
                    "number",
                    "The fall from a peak that counts as ruin. Default 50.",
                ),
                opt(
                    "seed",
                    "integer",
                    "The same seed gives the same draws. Default 0.",
                ),
                INCLUDE_LINES,
                opt(
                    "show_run",
                    "integer",
                    "A run's number, to list what it drew; one past the last is the backtest.",
                ),
                opt(
                    "limit",
                    "integer",
                    "How many runs to list, best first. Default 20.",
                ),
            ],
            &[
                "trades",
                "runs",
                "median",
                "low",
                "high",
                "drawdown",
                "ruin",
                "actual",
                "best_runs",
                "chart",
                "run_columns",
                "run_rows",
            ],
        ),
        job(
            "run_random_blocks",
            "Backtests random stretches of a span, to see whether a strategy works throughout. Returns and falls are fractions.",
            with(
                test_args(),
                &[
                    opt("blocks", "integer", "How many stretches. Default 50."),
                    opt(
                        "length",
                        "string",
                        "week, month (30 days, the default) or quarter (91 days).",
                    ),
                    opt(
                        "seed",
                        "integer",
                        "The same seed gives the same stretches. Default 0.",
                    ),
                    opt("include_trades", "boolean", "Include each block's trades."),
                    INCLUDE_LINES,
                    opt("limit", "integer", "How many blocks to list. Default 50."),
                ],
            ),
            &[
                "profitable",
                "median",
                "best",
                "worst",
                "blocks",
                "total",
                "chart",
                "median_buy_and_hold",
                "warning",
                "worst_dates",
            ],
        ),
        job(
            "run_random_entries",
            "Whether a run beat chance: the same holds and directions at random times, on the same bars and settings, many times over. For a portfolio, each leg's own trades, with their shares of the account, are placed at random leg by leg and run as a table of weights; `actual_return` is then the run's own trades as a table at their own times. `by_draw` lists each placing's draw and return. Returns and falls are fractions.",
            vec![
                RUN_ID,
                opt("runs", "integer", "How many random placings. Default 200."),
                opt(
                    "seed",
                    "integer",
                    "The same seed gives the same placings. Default 0.",
                ),
                opt(
                    "variants_tried",
                    "integer",
                    "How many variants this run was picked from, counting every setting, rule and span you tried. `share_matching_adjusted` is then the chance that one of that many worthless tries would have done as well. Default 1.",
                ),
            ],
            &[
                "runs",
                "trades",
                "actual_return",
                "share_matching_return",
                "share_matching_fall",
                "median",
                "low",
                "high",
                "actual_fell",
                "fall_chart",
                "return_chart",
                "warning",
                "variants_tried",
                "share_matching_adjusted",
                "by_draw",
            ],
        ),
        job(
            "run_walk_forward",
            "Picks the best values on one stretch, tests them on the next, unseen, and slides along, giving what they did where they hadn't been picked. Returns and falls are fractions. `headline` is the fair comparison: the unseen windows compounded one after another against buy_and_hold_whole, the market held once over the same span. `unseen_return` and `buy_and_hold_return` add up windows each started from the same capital, so they aren't what an account would have made.",
            with(
                test_args(),
                &[
                    arg(
                        "variables",
                        "array",
                        "One or two of {\"name\", \"min\", \"max\", \"step\"}.",
                    ),
                    opt(
                        "windows",
                        "integer",
                        "How many unseen stretches. Default 5.",
                    ),
                    opt(
                        "train_ratio",
                        "number",
                        "How many times longer each picking stretch is than its unseen one. Default 3.",
                    ),
                    opt(
                        "score",
                        "string",
                        "What to pick by: backtest (the default) or monte-carlo.",
                    ),
                    opt(
                        "runs",
                        "integer",
                        "For monte-carlo: how many draws. Default 1,000.",
                    ),
                    opt("ruin_percent", "number", "For monte-carlo. Default 50."),
                    opt("seed", "integer", "Default 0."),
                    opt(
                        "variants_tried",
                        "integer",
                        "How many variants you've tried in all, counting every setting, rule, span and market before this call as well as the values here. Each deflated Sharpe ratio counts the larger of this and the values here, and `variants_tried` says which. Default: the values here.",
                    ),
                ],
            ),
            &[
                "windows",
                "unseen_return",
                "profitable_windows",
                "deepest_fall",
                "trades",
                "buy_and_hold_compounded",
                "buy_and_hold_return",
                "buy_and_hold_whole",
                "chart",
                "test_bars",
                "train_bars",
                "unseen_compounded",
                "warning",
                "unseen_sharpe",
                "headline",
                "variants_tried",
            ],
        ),
        job(
            "run_optimize",
            "Backtests a range of one or two parameters, scored by return, Monte Carlo or random blocks. Scores are fractions.",
            with(
                test_args(),
                &[
                    arg(
                        "variables",
                        "array",
                        "One or two of {\"name\", \"min\", \"max\", \"step\"}.",
                    ),
                    opt(
                        "score",
                        "string",
                        "backtest (the default), monte-carlo or random-blocks.",
                    ),
                    opt(
                        "hold_out",
                        "boolean",
                        "Score on the first 70% of the span and check the best on the rest. Default false.",
                    ),
                    opt(
                        "blocks",
                        "integer",
                        "For random-blocks: how many. Default 50.",
                    ),
                    opt(
                        "length",
                        "string",
                        "For random-blocks: week, month or quarter.",
                    ),
                    opt(
                        "runs",
                        "integer",
                        "For monte-carlo: how many draws. Default 1,000.",
                    ),
                    opt("ruin_percent", "number", "For monte-carlo. Default 50."),
                    opt("seed", "integer", "Default 0."),
                    opt(
                        "variants_tried",
                        "integer",
                        "How many variants you've tried in all, counting every setting, rule, span and market before this call as well as the values here. Each deflated Sharpe ratio counts the larger of this and the values here, and `variants_tried` says which. Default: the values here.",
                    ),
                    opt(
                        "limit",
                        "integer",
                        "How many values to list, best first. Default 50.",
                    ),
                ],
            ),
            &[
                "values",
                "total",
                "top",
                "on_edge",
                "held_out",
                "backtests",
                "chart",
                "held_out_buy_and_hold",
                "warning",
                "deflated_sharpe",
                "variants_tried",
            ],
        ),
        tool(
            "get_report",
            "A run's full report: every statistic and note. Its own figures are fractions, and pnl counts a trade still open at its last value; closed_positions_pnl, returns and general are Nautilus's own.",
            vec![RUN_ID],
            &[
                "run_id",
                "request",
                "summary",
                "pnl",
                "returns",
                "general",
                "notes",
                "closed_positions_pnl",
                "ends_with_open_trade",
                "fees",
                "max_drawdown",
                "starting_balance",
                "total_orders",
                "total_positions",
            ],
        ),
        tool(
            "get_trades",
            "A run's trades, from flat to flat, in pages: profit in the quote currency, return_percent as a percentage of capital.",
            vec![RUN_ID, CURSOR, LIMIT],
            &["columns", "rows", "next_cursor", "total"],
        ),
        tool(
            "get_fills",
            "A run's fills, in pages.",
            vec![RUN_ID, CURSOR, LIMIT],
            &["columns", "rows", "next_cursor", "total"],
        ),
        tool(
            "get_limit_orders",
            "Every limit order a run sent, in pages, filled or not: what became of it, how far past its price the market traded (reached_percent), and how far the market moved its way a bar and 24 bars later. Shows whether those that fill are the bad ones.",
            vec![
                RUN_ID,
                opt(
                    "outcome",
                    "string",
                    "Only filled, lapsed, replaced, cancelled, refused or open.",
                ),
                CURSOR,
                LIMIT,
            ],
            &["columns", "rows", "next_cursor", "total"],
        ),
        tool(
            "get_decisions",
            "A run's decision for each bar, in pages, if it ran with `decision_log` on: what the strategy asked for, what was held, what the trading rules made of it, what order went out, and why not if none did.",
            vec![
                RUN_ID,
                opt("why", "string", "Only rows with this reason."),
                opt(
                    "wanted_only",
                    "boolean",
                    "Only bars where the strategy asked for something, and not those while it was warming up. `reasons` then counts only these.",
                ),
                CURSOR,
                LIMIT,
            ],
            &["columns", "rows", "next_cursor", "reasons", "total"],
        ),
        tool(
            "get_portfolio",
            "A portfolio run's summary, a table by leg of what each made and paid, and how the markets' own returns moved together, whichever way they're held.",
            vec![RUN_ID],
            &["summary", "legs", "correlation", "notes", "run_id"],
        ),
        tool(
            "get_portfolio_series",
            "A portfolio run at each bar of its span, in pages: equity, what equal shares held would be worth, gross and net exposure, and each leg's value and what it has made.",
            vec![RUN_ID, CURSOR, LIMIT],
            &["columns", "rows", "next_cursor", "total"],
        ),
        tool(
            "get_portfolio_fills",
            "A portfolio run's fills for one leg, in pages.",
            vec![RUN_ID, arg("symbol", "string", "Which leg."), CURSOR, LIMIT],
            &["columns", "rows", "next_cursor", "total"],
        ),
        tool(
            "get_portfolio_trades",
            "A portfolio run's trades, flat to flat, across every leg in the order they opened, or one leg's, in pages. profit is in the quote currency after fees; return_percent is of the starting capital, position_return_percent of what the trade put in. One still open at the end counts at its last close; funding and borrowing are the leg's, not any one trade's.",
            vec![
                RUN_ID,
                opt("symbol", "string", "Only this leg's."),
                CURSOR,
                LIMIT,
            ],
            &["columns", "rows", "next_cursor", "total", "profit"],
        ),
        tool(
            "get_equity",
            "A run's equity at each bar's close, in pages, beside what holding the market with a position of the same size would be worth: the line the app draws.",
            vec![RUN_ID, CURSOR, LIMIT],
            &["columns", "rows", "next_cursor", "total"],
        ),
        tool(
            "get_returns",
            "A run's per-period returns, as Nautilus works them out, in pages.",
            vec![RUN_ID, CURSOR, LIMIT],
            &["columns", "rows", "next_cursor", "total"],
        ),
        tool(
            "get_calendar",
            "What a run made, in the quote currency and as return_percent: by month; with `year` and `month`, by day; with `day` too, that day's trades.",
            vec![
                RUN_ID,
                opt("year", "integer", "A year."),
                opt("month", "integer", "A month, from 1."),
                opt("day", "integer", "A day of the month."),
            ],
            &["level", "rows"],
        ),
        tool(
            "list_runs",
            "Saved runs, newest first unless sorted. Searching reads every summary, so it takes longer with many runs.",
            vec![
                opt("label", "string", "Only runs saved with this label."),
                opt(
                    "sort",
                    "string",
                    "A summary figure to sort by, highest first, such as sharpe or net_return.",
                ),
                opt("ascending", "boolean", "Sort lowest first instead."),
                CURSOR,
                LIMIT,
            ],
            &["rows", "next_cursor", "total"],
        ),
        tool(
            "compare_runs",
            "Runs' summaries side by side.",
            vec![arg("run_ids", "array", "The runs to compare.")],
            &["runs"],
        ),
        tool(
            "get_job",
            "How a job is doing, and its result once it's done. Give wait_seconds rather than asking again and again.",
            vec![
                arg("job_id", "string", "A job id."),
                opt(
                    "wait_seconds",
                    "number",
                    "How long to wait for it to finish, answering the moment it does. Default 0.",
                ),
            ],
            &["job_id", "status", "result", "tool"],
        ),
        tool(
            "cancel_job",
            "Asks a job to stop. What's under way finishes and is dropped. get_job then says status cancelled, or done with what it got, if it can answer in part.",
            vec![arg("job_id", "string", "A job id.")],
            &["job_id", "status"],
        ),
        tool(
            "get_layout",
            "The app's saved layout: its charts and workspaces. The app saves it on every change.",
            vec![],
            &["layout"],
        ),
        tool(
            "get_chart_settings",
            "What's on the app's charts: market, indicators, strategy and backtest settings.",
            vec![],
            &["charts"],
        ),
        tool(
            "get_drawings",
            "The drawings saved on the app's charts.",
            vec![],
            &["drawings"],
        ),
        tool(
            "save_result",
            "Saves a strategy for the app, where it joins the backtest panel's strategy list.",
            vec![
                arg("name", "string", "What to call it."),
                STRATEGY,
                SETTINGS,
                opt("run_id", "string", "A run, whose summary to keep with it."),
                opt("note", "string", "Anything worth saying about it."),
            ],
            &["id", "path"],
        ),
        tool(
            "list_results",
            "Saved strategies, newest first.",
            vec![],
            &["results"],
        ),
        tool(
            "export_state",
            "Writes saved results, the layout, drawings and which plugins are in use to one JSON file, or just one chart's setup if given a strategy or settings. Never API keys, plugins' code or market data. Won't replace a file.",
            vec![
                opt(
                    "path",
                    "string",
                    "Where to write it. Default: a new file in Lab's exports folder.",
                ),
                opt(
                    "include_runs",
                    "boolean",
                    "Add every saved run in full, which can be large. Default false.",
                ),
                opt("strategy", "object", "A strategy, for one chart's setup."),
                opt("settings", "object", "Settings, for one chart's setup."),
                opt("name", "string", "A name for one chart's setup."),
            ],
            &[
                "path", "results", "runs", "plugins", "bytes", "chart", "drawings", "layout",
            ],
        ),
        tool(
            "import_state",
            "Adds an exported file's results and runs, keeping anything already here under the same id. A chart's setup comes back to use in calls. A layout and drawings aren't applied, as Lab doesn't change those: the app imports them and asks first.",
            vec![arg("path", "string", "The exported JSON file.")],
            &[
                "results_added",
                "results_skipped",
                "runs_added",
                "runs_skipped",
                "plugins_missing",
                "chart",
                "not_applied",
            ],
        ),
        tool(
            "get_call_log",
            "Every call made to Lab: tool, arguments and how it went, newest first. Lab enforces nothing with it.",
            vec![
                opt("tool", "string", "Only this tool's calls."),
                CURSOR,
                LIMIT,
            ],
            &["columns", "rows", "next_cursor"],
        ),
    ]
}

/// The tools' reference, as `docs/lab-tools.md` has it.
#[must_use]
pub fn reference() -> String {
    let mut out = String::from(
        "# Persephone Lab tools\n\n\
         Generated from the tools themselves: `persephone-lab tools --markdown`.\n\n\
         Tools marked *job* may answer with a `job_id` to check with `get_job` if they take longer \
         than `wait_seconds`.\n",
    );
    for tool in tools() {
        let job = if tool.job { " *job*" } else { "" };
        out += &format!("\n## `{}`{job}\n\n{}\n\n", tool.name, tool.what);
        for arg in &tool.args {
            let needed = if arg.required { ", required" } else { "" };
            out += &format!("- `{}` ({}{needed}): {}\n", arg.name, arg.kind, arg.what);
        }
        if !tool.args.is_empty() {
            out.push('\n');
        }
        let returns: Vec<String> = tool.returns.iter().map(|r| format!("`{r}`")).collect();
        out += &format!("Returns: {}\n", returns.join(", "));
    }
    out
}

/// A required text argument.
pub(crate) fn text<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{name}` is missing"))
}

/// Checks a call's arguments against its tool, so an agent hears what's wrong before anything runs.
pub(crate) fn check_args(tool: &Tool, args: &Value) -> Result<(), String> {
    let object = args
        .as_object()
        .ok_or_else(|| format!("{}: arguments must be an object, not {args}", tool.name))?;
    let known = |name: &str| {
        tool.args.iter().any(|arg| arg.name == name) || (tool.job && name == WAIT.name)
    };
    if let Some(unknown) = object.keys().find(|name| !known(name)) {
        let names: Vec<&str> = tool.args.iter().map(|arg| arg.name).collect();
        return Err(format!(
            "{}: `{unknown}` isn't an argument; it takes {names:?}",
            tool.name
        ));
    }
    for arg in &tool.args {
        let Some(value) = object.get(arg.name) else {
            if arg.required {
                return Err(format!(
                    "{}: `{}` is required: {}",
                    tool.name, arg.name, arg.what
                ));
            }
            continue;
        };
        let fits = match arg.kind {
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => value.is_u64() || value.is_i64(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "list_or_path" => value.is_array() || value.is_string(),
            "list" => value.is_array(),
            _ => value.is_string() || value.is_u64(),
        };
        if !fits {
            return Err(format!(
                "{}: `{}` should be {}, not {value}",
                tool.name, arg.name, arg.kind
            ));
        }
    }
    Ok(())
}

impl Lab {
    /// Calls a tool. Every way into Lab comes through here, so they all behave the same.
    ///
    /// # Errors
    ///
    /// Returns a message saying what went wrong and, where it can, what would be right.
    pub fn dispatch(self: &Arc<Self>, name: &str, args: &Value) -> Result<Value, String> {
        let result = self.call(name, args);
        self.log_call(name, args, &result);
        result
    }

    fn call(self: &Arc<Self>, name: &str, args: &Value) -> Result<Value, String> {
        let all = tools();
        let tool = all.iter().find(|tool| tool.name == name).ok_or_else(|| {
            format!(
                "there's no tool `{name}`; the tool list has them, and `get_guide` where to start"
            )
        })?;
        check_args(tool, args)?;
        // A portfolio's run has no one market's trades, and an empty answer would read as none made.
        if let Some(id) = args.get("run_id").and_then(Value::as_str)
            && !name.starts_with("get_portfolio")
            && !matches!(
                name,
                "run_shifted" | "run_portfolio_monte_carlo" | "run_random_entries"
            )
            && self.is_portfolio_run(id)
        {
            return Err(format!(
                "`{id}` is a portfolio, which {name} can't read; get_portfolio, get_portfolio_series and get_portfolio_fills do"
            ));
        }
        match name {
            "get_guide" => crate::guide::get_guide(args),
            "get_settings" => Ok(crate::guide::get_settings()),
            "list_markets" => self.list_markets(),
            "get_instrument" => self.get_instrument(args),
            "get_history_range" => self.get_history_range(args),
            "get_funding" => self.get_funding(args),
            "get_news" => self.get_news(args),
            "get_bars" => self.get_bars(args),
            "get_ticks" => self.get_ticks(args),
            "list_indicators" => self.list_indicators(),
            "list_models" => self.list_models(),
            "run_model" => self.run_model(args),
            "list_strategies" => self.list_strategies(),
            "describe_strategy" => self.describe_strategy(args),
            "validate_logic" => Ok(Self::validate_logic(args)),
            "install_plugin" => self.install_plugin(args),
            "get_report" => self.get_report(args),
            "get_trades" => self.get_trades(args),
            "get_fills" => self.get_fills(args),
            "get_limit_orders" => self.get_limit_orders(args),
            "get_decisions" => self.get_decisions(args),
            "get_portfolio" => self.get_portfolio(args),
            "get_portfolio_series" => self.get_portfolio_series(args),
            "get_portfolio_fills" => self.get_portfolio_fills(args),
            "get_portfolio_trades" => self.get_portfolio_trades(args),
            "get_equity" => self.get_equity(args),
            "get_returns" => self.get_returns(args),
            "get_calendar" => self.get_calendar(args),
            "list_runs" => self.list_runs(args),
            "compare_runs" => self.compare_runs(args),
            "get_job" => self.get_job(args),
            "cancel_job" => self.cancel_job(args),
            "get_layout" => self.get_layout(),
            "get_chart_settings" => self.get_chart_settings(),
            "get_drawings" => self.get_drawings(),
            "save_result" => self.save_result(args),
            "list_results" => Ok(self.list_results()),
            "export_state" => self.export_state(args),
            "import_state" => self.import_state(args),
            "get_call_log" => self.get_call_log(args),
            slow => {
                let owned = args.clone();
                let tool_name = slow.to_owned();
                self.run_job(slow, args, move |lab, job| match tool_name.as_str() {
                    "export_data" => lab.export_data(&owned, job),
                    "import_data" => lab.import_data(&owned, job),
                    "fetch_archive" => lab.fetch_archive(&owned, job),
                    "compute_indicator" => lab.compute_indicator(&owned),
                    "run_backtest" => lab.run_backtest(&owned),
                    "run_backtests" => lab.run_backtests(&owned, job),
                    "run_portfolio" => lab.run_portfolio(&owned),
                    "run_portfolios" => lab.run_portfolios(&owned, job),
                    "run_shifted" => lab.run_shifted(&owned, job),
                    "run_portfolio_monte_carlo" => lab.run_portfolio_monte_carlo(&owned),
                    "run_portfolio_walk_forward" => lab.run_portfolio_walk_forward(&owned, job),
                    "run_monte_carlo" => lab.run_monte_carlo(&owned, job),
                    "run_random_blocks" => lab.run_random_blocks(&owned, job),
                    "run_optimize" => lab.run_optimize(&owned, job),
                    "run_walk_forward" => lab.run_walk_forward(&owned, job),
                    "run_random_entries" => lab.run_random_entries(&owned, job),
                    other => Err(format!("there's no tool `{other}`")),
                })
            }
        }
    }

    /// Notes a call. Enormous arguments are cut short: the log is for seeing what happened.
    pub(crate) fn log_call(&self, name: &str, args: &Value, result: &Result<Value, String>) {
        const MOST: usize = 4_000;
        let arguments = if args.to_string().len() > MOST {
            json!({"cut_short": true})
        } else {
            args.clone()
        };
        let line = json!({
            "at": now_ms(),
            "tool": name,
            "args": arguments,
            "ok": result.is_ok(),
            "error": result.as_ref().err(),
        });
        let path = self.paths.lab().join("calls.jsonl");
        if let Ok(_guard) = self.log.lock()
            && let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// How many rows an answer has unless asked for more: enough to look at, small enough for an agent's
/// context, as a thousand rows runs to tens of thousands of tokens.
pub(crate) const DEFAULT_ROWS: usize = 100;

/// One page of `rows`, from `cursor`, with where to carry on.
pub(crate) fn page(columns: &[&str], rows: Vec<Value>, args: &Value, default: usize) -> Value {
    let from = args
        .get("cursor")
        .and_then(Value::as_str)
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = limit_of(args, default);
    let total = rows.len();
    let rows: Vec<Value> = rows.into_iter().skip(from).take(limit).collect();
    let end = from + rows.len();
    json!({
        "columns": columns,
        "rows": rows,
        "total": total,
        "next_cursor": (end < total).then(|| end.to_string()),
    })
}

/// A batch item's call: the batch's `defaults` with the item's own keys on top.
///
/// `settings` merge setting by setting, and within one, `on` apart from `value`, so an item's own
/// slippage keeps the defaults' commission. So does `strategy` when it's the same strategy, or
/// names none of its own: its `params` are then merged one by one. A different strategy keeps
/// only the defaults' `Size %`, which every strategy has, so a batch doesn't quietly fall back to
/// the 10% default for some runs.
pub(crate) fn merge_call(defaults: Option<&Map<String, Value>>, own: &Map<String, Value>) -> Value {
    let mut call = defaults.cloned().unwrap_or_default();
    for (key, value) in own {
        let merged = match (key.as_str(), call.get(key)) {
            ("settings", Some(shared)) => merged(shared, value, 2),
            ("strategy", Some(shared)) => merged_strategy(shared, value),
            _ => value.clone(),
        };
        call.insert(key.clone(), merged);
    }
    Value::Object(call)
}

/// `own` laid over `shared`: objects merge key by key, `depth` levels down, and below that, or
/// for anything but two objects, `own` replaces. A value such as `{"percent": 0.1}` is one
/// choice, so it's never mixed with another.
fn merged(shared: &Value, own: &Value, depth: usize) -> Value {
    match (shared, own) {
        (Value::Object(shared), Value::Object(own)) if depth > 0 => {
            let mut out = shared.clone();
            for (key, value) in own {
                let value = match out.get(key) {
                    Some(old) => merged(old, value, depth - 1),
                    None => value.clone(),
                };
                out.insert(key.clone(), value);
            }
            Value::Object(out)
        }
        _ => own.clone(),
    }
}

/// The keys that say which strategy it is.
const STRATEGY_KINDS: [&str; 4] = ["builtin", "plugin", "logic", "positions"];

fn merged_strategy(shared: &Value, own: &Value) -> Value {
    let (Some(shared), Some(own_object)) = (shared.as_object(), own.as_object()) else {
        return own.clone();
    };
    let kind_of = |object: &Map<String, Value>| -> Vec<(String, Value)> {
        object
            .iter()
            .filter(|(key, _)| STRATEGY_KINDS.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    };
    let own_kind = kind_of(own_object);
    if own_kind.is_empty() || own_kind == kind_of(shared) {
        let mut out = shared.clone();
        for (key, value) in own_object {
            let value = match (key.as_str(), out.get(key)) {
                ("params", Some(old)) => merged(old, value, 1),
                _ => value.clone(),
            };
            out.insert(key.clone(), value);
        }
        return Value::Object(out);
    }
    let mut out = own_object.clone();
    let size = persephone_core::strategy::SIZE.name.as_ref();
    if let Some(given) = shared.get("params").and_then(|params| params.get(size)) {
        let params = out
            .entry("params")
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(params) = params.as_object_mut() {
            params.entry(size).or_insert_with(|| given.clone());
        }
    }
    Value::Object(out)
}

/// One page of `total` rows, building only those on it.
pub(crate) fn page_of(
    columns: &[&str],
    total: usize,
    args: &Value,
    default: usize,
    row: impl Fn(usize) -> Value,
) -> Value {
    let from = args
        .get("cursor")
        .and_then(Value::as_str)
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .unwrap_or(0)
        .min(total);
    let end = (from + limit_of(args, default)).min(total);
    let rows: Vec<Value> = (from..end).map(row).collect();
    json!({
        "columns": columns,
        "rows": rows,
        "total": total,
        "next_cursor": (end < total).then(|| end.to_string()),
    })
}

/// How many to return: what's asked, up to a size an agent can take in.
pub(crate) fn limit_of(args: &Value, default: usize) -> usize {
    args.get("limit")
        .and_then(Value::as_u64)
        .map_or(default, |limit| limit as usize)
        .clamp(1, 50_000)
}
