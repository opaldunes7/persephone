//! The list of tools, and the one place every way into Lab calls them through.

use std::{fs::OpenOptions, io::Write as _, sync::Arc};

use serde_json::{Value, json};

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
    "{\"builtin\": \"ema-cross\"}, {\"plugin\": \"id\"} or {\"logic\": {...tree...}}, each with optional \"params\": {\"name\": value}.",
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
            "get_history_range",
            "When a market's history starts and ends.",
            vec![SYMBOL, VENUE, BARS],
            &["first", "last"],
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
            &["path", "rows", "format"],
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
            &["columns", "rows", "next_cursor"],
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
            &["name", "description", "params"],
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
            "Adds a compiled plugin to the plugins folder, where the app sees it too. Won't replace one of the same name.",
            vec![arg("path", "string", "The plugin's .wasm file.")],
            &["id", "kind", "path"],
        ),
        job(
            "run_backtest",
            "Backtests any strategy on any span with any settings. The run is saved, so its details can be read by run_id. Returns and falls are fractions, so 0.05 is 5%.",
            test_args(),
            &["run_id", "summary"],
        ),
        job(
            "run_backtests",
            "Many backtests in one call, in parallel: much quicker for an agent than one call each. Every run is saved, so all the tools that take a run_id work on them. One that fails doesn't stop the rest.",
            vec![
                arg(
                    "runs",
                    "array",
                    "A list of arguments as run_backtest takes, up to 5,000.",
                ),
                opt(
                    "defaults",
                    "object",
                    "Arguments every run shares, such as bars, from, to and settings. A run's own win.",
                ),
                opt(
                    "limit",
                    "integer",
                    "How many runs to list, best return first. Default 100.",
                ),
            ],
            &["columns", "rows", "total", "succeeded", "failed", "not_run"],
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
                ],
            ),
            &["profitable", "median", "best", "worst", "blocks"],
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
                        "limit",
                        "integer",
                        "How many values to list, best first. Default 50.",
                    ),
                ],
            ),
            &["values", "total", "top", "on_edge", "held_out", "backtests"],
        ),
        tool(
            "get_report",
            "A run's full report: every statistic and note. Its own figures are fractions; pnl, returns and general are Nautilus's own.",
            vec![RUN_ID],
            &[
                "run_id", "request", "summary", "pnl", "returns", "general", "notes",
            ],
        ),
        tool(
            "get_trades",
            "A run's trades, from flat to flat, in pages: profit in the quote currency, return_percent as a percentage of capital.",
            vec![RUN_ID, CURSOR, LIMIT],
            &["columns", "rows", "next_cursor"],
        ),
        tool(
            "get_fills",
            "A run's fills, in pages.",
            vec![RUN_ID, CURSOR, LIMIT],
            &["columns", "rows", "next_cursor"],
        ),
        tool(
            "get_equity",
            "A run's equity at each bar's close, in pages.",
            vec![RUN_ID, CURSOR, LIMIT],
            &["columns", "rows", "next_cursor"],
        ),
        tool(
            "get_returns",
            "A run's per-period returns, as Nautilus works them out, in pages.",
            vec![RUN_ID, CURSOR, LIMIT],
            &["columns", "rows", "next_cursor"],
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
            "Saved runs, newest first.",
            vec![CURSOR, LIMIT],
            &["columns", "rows", "next_cursor"],
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
            &["job_id", "status"],
        ),
        tool(
            "cancel_job",
            "Asks a job to stop. What's under way finishes and is dropped.",
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
            format!("there's no tool `{name}`; `persephone-lab tools` lists them")
        })?;
        check_args(tool, args)?;
        match name {
            "get_guide" => crate::guide::get_guide(args),
            "get_settings" => Ok(crate::guide::get_settings()),
            "list_markets" => self.list_markets(),
            "get_instrument" => self.get_instrument(args),
            "get_history_range" => self.get_history_range(args),
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
                    "run_monte_carlo" => lab.run_monte_carlo(&owned, job),
                    "run_random_blocks" => lab.run_random_blocks(&owned, job),
                    "run_optimize" => lab.run_optimize(&owned, job),
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
