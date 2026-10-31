# Persephone Lab tools

Generated from the tools themselves: `persephone-lab tools --markdown`.

Tools marked *job* may answer with a `job_id` to check with `get_job` if they take longer than `wait_seconds`.

## `get_guide`

How to use Lab, written for agents. Read this first: without a topic it lists the topics, each short. They include backtests (how fills, sizing and warm-up work), logic, settings, units and data.

- `topic` (string): A section's name, or `all` for the whole guide.

Returns: `guide`, `topics`

## `get_settings`

The default settings as exact JSON, ready to edit and pass as `settings`.

Returns: `settings`

## `list_markets`

Markets in the local store. Binance spot symbols work anywhere a symbol does, without being listed.

Returns: `local`, `binance`

## `get_instrument`

A market's details: quote currency, precision and fees.

- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string): The venue of a market in the local store, from list_markets. Without one it's Binance spot.

Returns: `symbol`, `venue`, `base`, `quote`, `price_precision`, `size_precision`, `fees`

## `get_history_range`

When a market's history starts and ends.

- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string): The venue of a market in the local store, from list_markets. Without one it's Binance spot.
- `bars` (string): How bars are cut: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M; or 100t for every 100 ticks and 2.5v by volume, on local markets. Default 1h.

Returns: `first`, `last`

## `get_bars`

A market's bars, in pages, oldest first. Without `from`, its latest bars.

- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string): The venue of a market in the local store, from list_markets. Without one it's Binance spot.
- `bars` (string): How bars are cut: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M; or 100t for every 100 ticks and 2.5v by volume, on local markets. Default 1h.
- `from` (time): Start, in UTC: 2024-01-31 or 2024-01-31 09:30.
- `to` (time): End (not included), in UTC.
- `cursor` (string): Where to carry on, from the last page's next_cursor.
- `limit` (integer): The most to return in this page, up to 50,000. Default 100, to keep answers small.

Returns: `columns`, `rows`, `next_cursor`

## `get_ticks`

A local market's trades or quotes, in pages.

- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string, required): The local market's venue.
- `from` (time, required): Start, in UTC.
- `to` (time): End (not included), in UTC.
- `cursor` (string): Where to carry on, from the last page's next_cursor.
- `limit` (integer): The most to return in this page, up to 50,000. Default 100, to keep answers small.

Returns: `kind`, `columns`, `rows`, `next_cursor`

## `export_data` *job*

Writes bars or ticks to a file, for large pulls and training outside Persephone.

- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string): The venue of a market in the local store, from list_markets. Without one it's Binance spot.
- `bars` (string): How bars are cut: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M; or 100t for every 100 ticks and 2.5v by volume, on local markets. Default 1h.
- `from` (time): Start, in UTC: 2024-01-31 or 2024-01-31 09:30.
- `to` (time): End (not included), in UTC.
- `ticks` (boolean): Ticks instead of bars, on a local market.
- `format` (string): csv (the default) or parquet.
- `path` (string): Where to write it. Default: Lab's exports folder.

Returns: `path`, `rows`, `format`

## `import_data` *job*

Imports CSV files of ticks or bars into the local store, working out their layout as the app does.

- `files` (array, required): Paths of the files.
- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string, required): The venue to file them under.
- `base` (string): Base currency; worked out from the symbol if left out.
- `quote` (string): Quote currency; worked out from the symbol if left out.
- `fee` (number): Fee rate per fill (0.001 is 0.1%).

Returns: `market`, `imported`, `skipped`, `problems`

## `fetch_archive` *job*

Downloads Binance's monthly or daily trade archives into the local store.

- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `from` (string, required): First period: 2024-01 for a month, 2024-01-31 for a day.
- `to` (string): Last period. Default: just `from`.
- `all_trades` (boolean): Every trade, not the aggregated ones.

Returns: `market`, `imported`, `skipped`, `files`

## `list_indicators`

Built-in and plugin indicators, with their parameters and outputs.

Returns: `indicators`, `plugin_errors`

## `compute_indicator` *job*

An indicator's values over a market's bars.

- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string): The venue of a market in the local store, from list_markets. Without one it's Binance spot.
- `bars` (string): How bars are cut: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M; or 100t for every 100 ticks and 2.5v by volume, on local markets. Default 1h.
- `from` (time): Start, in UTC: 2024-01-31 or 2024-01-31 09:30.
- `to` (time): End (not included), in UTC.
- `indicator` (string, required): An id from list_indicators.
- `params` (object): {"name": value}; the rest keep their defaults.
- `cursor` (string): Where to carry on, from the last page's next_cursor.
- `limit` (integer): The most to return in this page, up to 50,000. Default 100, to keep answers small.

Returns: `columns`, `rows`, `next_cursor`

## `list_models`

ONNX models in the models folder.

Returns: `models`, `folder`

## `run_model`

Runs an ONNX model on an input.

- `model` (string, required): A name from list_models.
- `shape` (array, required): The input's shape, like [1, 4].
- `data` (array, required): The input's numbers, row by row.

Returns: `outputs`

## `list_strategies`

Built-in and plugin strategies, with their parameters.

Returns: `strategies`, `plugin_errors`

## `describe_strategy`

One strategy's parameters, with their defaults and limits. Works for a logic tree too.

- `strategy` (object, required): {"builtin": "ema-cross"}, {"plugin": "id"}, {"logic": {...tree...}} or {"positions": [[ts, 1], [ts, 0]]}, each with optional "params": {"name": value}.

Returns: `name`, `description`, `params`

## `validate_logic`

Checks a logic tree without running it, saying where it's wrong and what would be right. Also says what a tree can be made of.

- `logic` (object): The tree. Without one, this only describes what's allowed.

Returns: `valid`, `error`, `allowed`

## `install_plugin`

Adds a plugin to the plugins folder, where the app sees it too: a compiled one by `path`, or a program answering over a socket by `name` and `address`. Won't replace one of the same name.

- `path` (string): A compiled plugin's .wasm file.
- `name` (string): What to call a program answering over a socket.
- `address` (string): Where it listens, as tcp://127.0.0.1:7878 or unix:///tmp/model.sock. It has to be running. The guide's `extending` topic has what it must answer.

Returns: `id`, `kind`, `path`

## `run_backtest` *job*

Backtests any strategy on any span with any settings. The run is saved, so its details can be read by run_id. Returns and falls are fractions, so 0.05 is 5%.

- `strategy` (object, required): {"builtin": "ema-cross"}, {"plugin": "id"}, {"logic": {...tree...}} or {"positions": [[ts, 1], [ts, 0]]}, each with optional "params": {"name": value}.
- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string): The venue of a market in the local store, from list_markets. Without one it's Binance spot.
- `bars` (string): How bars are cut: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M; or 100t for every 100 ticks and 2.5v by volume, on local markets. Default 1h.
- `from` (time): Start, in UTC: 2024-01-31 or 2024-01-31 09:30.
- `to` (time): End (not included), in UTC.
- `settings` (object): Costs and rules, as in run_backtest; only what differs from the defaults is needed. get_settings gives the defaults as exact JSON.
- `label` (string): A name for grouping runs, saved with the run for finding it with list_runs.

Returns: `run_id`, `summary`

## `run_backtests` *job*

Many backtests in one call, in parallel: much quicker for an agent than one call each. Every run is saved, so all the tools that take a run_id work on them. One that fails doesn't stop the rest.

- `runs` (array, required): A list of arguments as run_backtest takes, up to 5,000.
- `defaults` (object): Arguments every run shares, such as bars, from, to and settings. A run's own win.
- `limit` (integer): How many runs to list, best return first. Default 100.

Returns: `columns`, `rows`, `total`, `succeeded`, `failed`, `not_run`

## `run_monte_carlo` *job*

Draws a run's trades at random many times, to see the range of results they could have given. Returns, falls and ruin are fractions.

- `run_id` (string, required): A run, from run_backtest or list_runs.
- `runs` (integer): How many draws. Default 1,000.
- `ruin_percent` (number): The fall from a peak that counts as ruin. Default 50.
- `seed` (integer): The same seed gives the same draws. Default 0.
- `show_run` (integer): A run's number, to list what it drew; one past the last is the backtest.
- `limit` (integer): How many runs to list, best first. Default 20.

Returns: `trades`, `runs`, `median`, `low`, `high`, `drawdown`, `ruin`, `actual`, `best_runs`

## `run_random_blocks` *job*

Backtests random stretches of a span, to see whether a strategy works throughout. Returns and falls are fractions.

- `strategy` (object, required): {"builtin": "ema-cross"}, {"plugin": "id"}, {"logic": {...tree...}} or {"positions": [[ts, 1], [ts, 0]]}, each with optional "params": {"name": value}.
- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string): The venue of a market in the local store, from list_markets. Without one it's Binance spot.
- `bars` (string): How bars are cut: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M; or 100t for every 100 ticks and 2.5v by volume, on local markets. Default 1h.
- `from` (time): Start, in UTC: 2024-01-31 or 2024-01-31 09:30.
- `to` (time): End (not included), in UTC.
- `settings` (object): Costs and rules, as in run_backtest; only what differs from the defaults is needed. get_settings gives the defaults as exact JSON.
- `blocks` (integer): How many stretches. Default 50.
- `length` (string): week, month (30 days, the default) or quarter (91 days).
- `seed` (integer): The same seed gives the same stretches. Default 0.
- `include_trades` (boolean): Include each block's trades.

Returns: `profitable`, `median`, `best`, `worst`, `blocks`

## `run_walk_forward` *job*

Picks the best values on one stretch, tests them on the next, unseen, and slides along, giving what they did where they hadn't been picked. Returns and falls are fractions.

- `strategy` (object, required): {"builtin": "ema-cross"}, {"plugin": "id"}, {"logic": {...tree...}} or {"positions": [[ts, 1], [ts, 0]]}, each with optional "params": {"name": value}.
- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string): The venue of a market in the local store, from list_markets. Without one it's Binance spot.
- `bars` (string): How bars are cut: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M; or 100t for every 100 ticks and 2.5v by volume, on local markets. Default 1h.
- `from` (time): Start, in UTC: 2024-01-31 or 2024-01-31 09:30.
- `to` (time): End (not included), in UTC.
- `settings` (object): Costs and rules, as in run_backtest; only what differs from the defaults is needed. get_settings gives the defaults as exact JSON.
- `variables` (array, required): One or two of {"name", "min", "max", "step"}.
- `windows` (integer): How many unseen stretches. Default 5.
- `train_ratio` (number): How many times longer each picking stretch is than its unseen one. Default 3.
- `score` (string): What to pick by: backtest (the default) or monte-carlo.
- `runs` (integer): For monte-carlo: how many draws. Default 1,000.
- `ruin_percent` (number): For monte-carlo. Default 50.
- `seed` (integer): Default 0.

Returns: `windows`, `unseen_return`, `profitable_windows`, `deepest_fall`, `trades`

## `run_optimize` *job*

Backtests a range of one or two parameters, scored by return, Monte Carlo or random blocks. Scores are fractions.

- `strategy` (object, required): {"builtin": "ema-cross"}, {"plugin": "id"}, {"logic": {...tree...}} or {"positions": [[ts, 1], [ts, 0]]}, each with optional "params": {"name": value}.
- `symbol` (string, required): The market's symbol, like BTCUSDT.
- `venue` (string): The venue of a market in the local store, from list_markets. Without one it's Binance spot.
- `bars` (string): How bars are cut: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M; or 100t for every 100 ticks and 2.5v by volume, on local markets. Default 1h.
- `from` (time): Start, in UTC: 2024-01-31 or 2024-01-31 09:30.
- `to` (time): End (not included), in UTC.
- `settings` (object): Costs and rules, as in run_backtest; only what differs from the defaults is needed. get_settings gives the defaults as exact JSON.
- `variables` (array, required): One or two of {"name", "min", "max", "step"}.
- `score` (string): backtest (the default), monte-carlo or random-blocks.
- `hold_out` (boolean): Score on the first 70% of the span and check the best on the rest. Default false.
- `blocks` (integer): For random-blocks: how many. Default 50.
- `length` (string): For random-blocks: week, month or quarter.
- `runs` (integer): For monte-carlo: how many draws. Default 1,000.
- `ruin_percent` (number): For monte-carlo. Default 50.
- `seed` (integer): Default 0.
- `limit` (integer): How many values to list, best first. Default 50.

Returns: `values`, `total`, `top`, `on_edge`, `held_out`, `backtests`

## `get_report`

A run's full report: every statistic and note. Its own figures are fractions; pnl, returns and general are Nautilus's own.

- `run_id` (string, required): A run, from run_backtest or list_runs.

Returns: `run_id`, `request`, `summary`, `pnl`, `returns`, `general`, `notes`

## `get_trades`

A run's trades, from flat to flat, in pages: profit in the quote currency, return_percent as a percentage of capital.

- `run_id` (string, required): A run, from run_backtest or list_runs.
- `cursor` (string): Where to carry on, from the last page's next_cursor.
- `limit` (integer): The most to return in this page, up to 50,000. Default 100, to keep answers small.

Returns: `columns`, `rows`, `next_cursor`

## `get_fills`

A run's fills, in pages.

- `run_id` (string, required): A run, from run_backtest or list_runs.
- `cursor` (string): Where to carry on, from the last page's next_cursor.
- `limit` (integer): The most to return in this page, up to 50,000. Default 100, to keep answers small.

Returns: `columns`, `rows`, `next_cursor`

## `get_equity`

A run's equity at each bar's close, in pages.

- `run_id` (string, required): A run, from run_backtest or list_runs.
- `cursor` (string): Where to carry on, from the last page's next_cursor.
- `limit` (integer): The most to return in this page, up to 50,000. Default 100, to keep answers small.

Returns: `columns`, `rows`, `next_cursor`

## `get_returns`

A run's per-period returns, as Nautilus works them out, in pages.

- `run_id` (string, required): A run, from run_backtest or list_runs.
- `cursor` (string): Where to carry on, from the last page's next_cursor.
- `limit` (integer): The most to return in this page, up to 50,000. Default 100, to keep answers small.

Returns: `columns`, `rows`, `next_cursor`

## `get_calendar`

What a run made, in the quote currency and as return_percent: by month; with `year` and `month`, by day; with `day` too, that day's trades.

- `run_id` (string, required): A run, from run_backtest or list_runs.
- `year` (integer): A year.
- `month` (integer): A month, from 1.
- `day` (integer): A day of the month.

Returns: `level`, `rows`

## `list_runs`

Saved runs, newest first unless sorted. Searching reads every summary, so it takes longer with many runs.

- `label` (string): Only runs saved with this label.
- `sort` (string): A summary figure to sort by, highest first, such as sharpe or net_return.
- `ascending` (boolean): Sort lowest first instead.
- `cursor` (string): Where to carry on, from the last page's next_cursor.
- `limit` (integer): The most to return in this page, up to 50,000. Default 100, to keep answers small.

Returns: `columns`, `rows`, `next_cursor`

## `compare_runs`

Runs' summaries side by side.

- `run_ids` (array, required): The runs to compare.

Returns: `runs`

## `get_job`

How a job is doing, and its result once it's done. Give wait_seconds rather than asking again and again.

- `job_id` (string, required): A job id.
- `wait_seconds` (number): How long to wait for it to finish, answering the moment it does. Default 0.

Returns: `job_id`, `status`

## `cancel_job`

Asks a job to stop. What's under way finishes and is dropped.

- `job_id` (string, required): A job id.

Returns: `job_id`, `status`

## `get_layout`

The app's saved layout: its charts and workspaces. The app saves it on every change.

Returns: `layout`

## `get_chart_settings`

What's on the app's charts: market, indicators, strategy and backtest settings.

Returns: `charts`

## `get_drawings`

The drawings saved on the app's charts.

Returns: `drawings`

## `save_result`

Saves a strategy for the app, where it joins the backtest panel's strategy list.

- `name` (string, required): What to call it.
- `strategy` (object, required): {"builtin": "ema-cross"}, {"plugin": "id"}, {"logic": {...tree...}} or {"positions": [[ts, 1], [ts, 0]]}, each with optional "params": {"name": value}.
- `settings` (object): Costs and rules, as in run_backtest; only what differs from the defaults is needed. get_settings gives the defaults as exact JSON.
- `run_id` (string): A run, whose summary to keep with it.
- `note` (string): Anything worth saying about it.

Returns: `id`, `path`

## `list_results`

Saved strategies, newest first.

Returns: `results`

## `get_call_log`

Every call made to Lab: tool, arguments and how it went, newest first. Lab enforces nothing with it.

- `tool` (string): Only this tool's calls.
- `cursor` (string): Where to carry on, from the last page's next_cursor.
- `limit` (integer): The most to return in this page, up to 50,000. Default 100, to keep answers small.

Returns: `columns`, `rows`, `next_cursor`
