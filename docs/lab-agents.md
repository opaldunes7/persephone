# Persephone Lab: guide for agents

Lab gives you Persephone's markets, backtests and tests as tools. Nothing here limits how you research:
any strategy, span or number of tries is allowed. Whoever set you up may add rules of their own.

Start with `get_guide` and a `topic`, as each section is short and you rarely need them all. Every
example below is a real call: the tool, then its arguments.

## quickstart

A backtest, then its details, then a strategy written without code.

```json
{"tool": "run_backtest", "args": {
  "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-03-01",
  "strategy": {"builtin": "ema-cross", "params": {"Fast": 10, "Slow": 30}}
}}
```

It answers with a `run_id` and a `summary`. Read the rest by that id:

```json
{"tool": "get_trades", "args": {"run_id": "$RUN", "limit": 20}}
```

```json
{"tool": "run_backtest", "args": {
  "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-03-01",
  "strategy": {"logic": {
    "params": {"length": 14, "low": 30, "high": 70},
    "long":  {"crosses_above": [{"rsi": {"length": "$length"}}, "$low"]},
    "short": {"crosses_below": [{"rsi": {"length": "$length"}}, "$high"]}
  }}
}}
```

## backtests

How a backtest works, which decides what its numbers mean.

- **Decisions come at a bar's close.** A strategy sees one closed bar at a time and can't look ahead.
- **Fills:** a market order, at that bar's close, plus any slippage and spread. With `precision: "high"`,
  orders fill on what happened next inside the bars instead: ticks, or 1-minute bars.
- **A strategy says what position it wants:** long, short or flat. One position at a time, in one
  direction; reversing is one order, twice the size. There are no stops or limit orders.
- **Size:** each position is worth capital × `Size %` × leverage, worked out from the **starting**
  capital, at the price when it opens. Profits aren't compounded. `Size %` is a parameter of every
  strategy, 10 unless set, from 1 to 100.
- **Fees:** Binance markets pay 0.1% of each fill unless `settings` say otherwise; imported markets pay
  what they were imported with.
- **Warm-up:** on time bars, up to 500 bars before `from` load first. The strategy sees them but doesn't
  trade, and they aren't in the results.
- **At the end:** a position still open counts at the last close, with no closing fee.

A strategy is `{"builtin": id}`, `{"plugin": id}` or `{"logic": tree}`, each with optional `"params"`.
`list_strategies` lists them, and `describe_strategy` gives one's parameters and limits.

## logic

Strategies as JSON, tested straight away with nothing to compile. `validate_logic` checks one.

```json
{"tool": "validate_logic", "args": {"logic": {
  "name": "Above its average, in rising volume",
  "params": {"length": 20},
  "long": {"and": [
    {"gt": ["close", {"sma": {"length": "$length"}}]},
    {"gt": ["volume", {"ago": {"of": "volume", "bars": 1}}]}
  ]},
  "flat": {"lt": ["close", {"sma": {"length": "$length"}}]}
}}}
```

- **Rules:** `long` and `short` ask for that position. On a bar with both or neither, `flat` asks to
  close, and otherwise the position stays as it is. At least one of `long` or `short` is required.
- **Values:** a number; a bar's `open`, `high`, `low`, `close` or `volume`; a parameter as `"$name"`, from
  `params`; or an object below.
- **Indicators:** `{"sma": {"of": value, "length": n}}`, and the same for `ema`, `rsi` (0 to 100),
  `highest`, `lowest` (both include this bar), `roc` (percent change over n bars) and `zscore`. `of` is the
  close unless given, and can be another indicator. `atr` only takes `length`.
- **Other values:** `{"ago": {"of": value, "bars": n}}`, and `{"add": [a, b]}`, likewise `sub`, `mul`, `div`.
- **Comparisons:** `gt`, `lt`, `ge`, `le`, `crosses_above`, `crosses_below`, each `[a, b]`.
- **Combinations:** `and` and `or` take a list; `not` takes one rule.
- **Not ready yet:** a value without enough history makes any comparison on it false.
- **Limits:** 500 nodes, and look-backs up to 10,000 bars.
- **Parameters** can be changed per call with `"params"` beside `"logic"`, and optimized by name.

## settings

Costs and rules go in `settings` on `run_backtest`, `run_random_blocks` and `run_optimize`. Give only what
differs from the defaults, which `get_settings` returns as exact JSON.

```json
{"tool": "run_backtest", "args": {
  "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-03-01",
  "strategy": {"builtin": "ema-cross"},
  "settings": {
    "capital": 50000,
    "commission": {"on": true, "value": {"per-order": 1.5}},
    "slippage": {"on": true, "value": 2},
    "spread": {"on": true, "value": 4},
    "latency": {"on": true, "value": 100},
    "leverage": {"on": true, "value": 3},
    "direction": {"on": true, "value": "long-only"},
    "invert": true,
    "session": {"on": true, "value": {
      "days": [true, true, true, true, true, false, false],
      "from": 570, "to": 960, "zone": "America/New_York", "close_after": false
    }},
    "precision": "standard"
  }
}}
```

Most are `{"on": bool, "value": ...}`, and do nothing while off.

| Setting | Value |
|---|---|
| `stock` | `false` turns all of the below off at once, except `capital` and `precision` |
| `capital` | Starting cash, in the quote currency |
| `commission` | `{"percent": 0.1}` of each fill, or `{"per-order": 1.5}` flat, instead of the market's fee |
| `slippage` | Ticks every market order fills worse |
| `spread` | Ticks of spread assumed for bars and trades |
| `latency` | Milliseconds before orders reach the venue |
| `leverage` | A multiple of position size, 1 or more. Margin calls aren't simulated |
| `direction` | `"long-only"` or `"short-only"`; the other side's signals close instead of opening |
| `invert` | `true` takes the opposite of every signal, before `direction` applies |
| `session` | When positions may open: `days` from Monday, `from` and `to` in minutes after midnight, `zone` as `UTC`, `-05:00` or `America/New_York`, and `close_after` to close when it ends. Outside it a signal can close a position, but not open or reverse one. An end before its start runs overnight, and equal times are all day |
| `precision` | `"standard"`, or `"high"` to fill on ticks or 1-minute bars. Slower |

The panel's own choices, `test`, `runs`, `ruin`, `blocks`, `block_length`, `optimize`, `plugins` and
`stock_open`, are accepted and ignored here. Tests take their own arguments instead.

## units

What every number means, the same everywhere.

- **Returns and falls are fractions,** so `0.05` is 5%, unless the name ends in `_percent`.
- **Falls are positive:** `max_drawdown`, `fell` and `drawdown` of `0.1` mean 10% down from a peak.
- **Money** (`profit`, `fees`, `final_equity`) is in the market's quote currency.
- **Times:** `ts` is nanoseconds since 1970, and `time` is `YYYY-MM-DD HH:MM`. All are UTC.
- **A bar's `ts` is when it opens.** Its order fills at its close.
- **`from` and `to`** take `2024-01-31`, `2024-01-31 09:30` or nanoseconds. `to` isn't included.
- **`win_rate`, `ruin` and `profitable`** are fractions of trades, runs and blocks.
- **`fills` and `trades` differ:** a trade is flat to flat, and a reversal is one fill in two trades.
- **Null** in a number means it wasn't a number: often an indicator not ready yet.

Statistics in `get_report`'s `pnl`, `returns` and `general` come from Nautilus under its own names.

## data

Which bars a call uses, their limits, and what's cached.

```json
{"tool": "get_bars", "args": {"symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "limit": 500}}
```

- **`bars`:** `1m`, `5m`, `15m`, `30m`, `1h`, `4h`, `1d`, `1w` or `1M`. On local markets, also `100t` for every
  100 ticks and `2.5v` by volume. Default `1h`.
- **Binance markets:** any spot symbol, with no `venue`. Only **closed** bars are used.
- **Local markets:** `venue` from `list_markets`.
- **Without `from`:** the latest 5,000 bars, so a call with no `to` moves as new bars close. A summary's
  `first_bar` and `last_bar` say which bars it used. Give both ends to get the same answer every time.
- **Limits on a backtest:** 300,000 bars from Binance, and 1,000,000 1-minute bars for `precision: "high"`.
  `export_data` has no such limit.
- **Caching:** Binance bars are kept on disk, so a span costs a download once. A first call on a long span
  can take a while; later ones take milliseconds. Don't avoid repeating a span to save time.
- **For training models elsewhere,** `export_data` writes CSV or Parquet.

## tests

Monte Carlo, random blocks, Optimize and batches of backtests. The first three take a `seed`: the same inputs
and seed give the same answer.

**Trying many things? Use one `run_backtests` call rather than many `run_backtest` calls.** It runs them in
parallel and answers in one table, best return first, which saves a model turn for each one.

```json
{"tool": "run_backtests", "args": {
  "defaults": {"bars": "1h", "from": "2024-01-01", "to": "2024-03-01"},
  "runs": [
    {"symbol": "BTCUSDT", "strategy": {"builtin": "ema-cross", "params": {"Fast": 5}}},
    {"symbol": "BTCUSDT", "strategy": {"builtin": "ema-cross", "params": {"Fast": 10}}},
    {"symbol": "BTCUSDT", "strategy": {"builtin": "rsi-reversion"}}
  ]
}}
```

Each run's own arguments override `defaults`. `index` in a row is the run's place in your list. One that fails
doesn't stop the others: `error` says why, and `failed` and `first_error` report it. It lists 100 unless
`limit` says otherwise, and all are saved, so `get_report`, `get_trades` and the rest work on any `run_id`.

Monte Carlo draws a finished run's trades at random. It needs at least 10 trades.

```json
{"tool": "run_monte_carlo", "args": {"run_id": "$RUN", "runs": 1000, "ruin_percent": 50, "seed": 1, "limit": 5}}
```

`median`, `low` and `high` are returns (5th to 95th percentile), `drawdown` is the 95th percentile fall,
and `actual` shows where the real order of trades ranks. `show_run` lists what one run drew.

Random blocks backtests random stretches of a span, and needs a span longer than one block. Orders fill on
the chart's bars whatever the `precision`.

```json
{"tool": "run_random_blocks", "args": {
  "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-04-01",
  "strategy": {"builtin": "ema-cross"}, "blocks": 10, "length": "week", "seed": 1
}}
```

Optimize tries a range of one or two parameters, by the names `describe_strategy` gives.

```json
{"tool": "run_optimize", "args": {
  "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-04-01",
  "strategy": {"builtin": "ema-cross", "params": {"Slow": 40}},
  "variables": [{"name": "Fast", "min": 5, "max": 15, "step": 5}],
  "score": "backtest", "hold_out": true
}}
```

- **`score`:** `backtest` (return), `monte-carlo` (5th percentile return) or `random-blocks` (median block).
- **`values`** lists the best 50 unless `limit` says otherwise, and `total` says how many there were.
- **`top`** ranks by `with_neighbours`, a value's score averaged with those beside it, so a lone spike
  doesn't win.
- **`hold_out`** scores on the span's first 70% and gives the top 5's `unseen_return` on the last 30%. It's
  off unless asked for.
- **Values with no score:** `status` says `too-few-trades` or `failed`, and the rest carry on.

## jobs

Backtests and tests may take a while. Each waits `wait_seconds` (30 by default), and if it isn't finished
answers with `status: "running"` and a `job_id` rather than holding the call open.

```json
{"tool": "get_job", "args": {"job_id": "$JOB", "wait_seconds": 60}}
```

`get_job` gives `done` and `total` while running, and `status: "done"` with `result` when finished, or the
error if it failed. Give it `wait_seconds` rather than asking repeatedly: it answers the moment the job
finishes. `cancel_job` stops it, and it shows `status: "cancelling"` until it has. Answers are kept, so a
job can be read after a restart.

Backtests run in parallel up to one less than the machine's cores, however many calls they come from. The
rest wait their turn, so calling in parallel is safe.

## paging

Long answers come in pages: `columns`, `rows` and `next_cursor`. Pass `next_cursor` back as `cursor` until
it's null. Pages are 100 rows unless `limit` says otherwise (`list_runs` gives 20, and `get_call_log` 50),
up to 50,000. A page of 100 trades costs about 5,000 tokens, so ask for what you need: a run's `summary` is
usually enough, and `export_data` suits anything large.

## results

Showing a strategy to the person using the app, and seeing what's on their charts.

```json
{"tool": "save_result", "args": {"name": "RSI bounce", "strategy": {"builtin": "rsi-reversion"}, "run_id": "$RUN", "note": "Worth a look on 4h"}}
```

It appears in the app's backtest panel straight away, marked Lab, to chart and replay.

`get_layout`, `get_chart_settings` and `get_drawings` show what's on their charts, and `get_call_log` shows
every call made to Lab, newest first.

## extending

Adding markets, compiled strategies and models.

`import_data` and `fetch_archive` add markets, `install_plugin` adds compiled strategies
and indicators, `list_indicators` lists both kinds, and `run_model` runs an ONNX model from `list_models`,
for models trained elsewhere.

## limits

What Lab can't do.

- **No orders:** Lab can't trade. Paper and live trading are in the app.
- **No keys:** API keys aren't available through any tool.
- **No language models:** Lab can't call them, and a plugin that asks one gets no answer. ONNX models work.
- **No deleting or overwriting:** you can add markets, plugins and results, but not remove or change them.
  `export_data` won't replace a file.
- **Errors** are plain messages saying what's wrong and, where possible, what would be right.
