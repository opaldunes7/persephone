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

It answers with a `run_id` and a `summary`, including `warnings`: read them first. Read the rest by that id:

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

- **Decisions come at a bar's close.** A strategy sees one closed bar at a time and can't look ahead, except
  a list of `positions`, which holds whatever you put in it.
- **Fills:** a market order, at that bar's close, plus any slippage and spread. With `latency`, it arrives after
  that close, so on bars it fills at the next bar's close, which is a whole bar's delay however small the latency.
  Every change still goes out, each from where the order before it will leave things.
  With `precision: "high"`, it fills on what's next inside the bars instead: ticks, or 1-minute bars. So `high` only
  changes results alongside `latency`, or on ticks, and otherwise just takes longer.
- **A strategy says what position it wants:** long, short or flat. One position at a time, in one
  direction; reversing is one order, twice the size. There are no stops or limit orders.
- **Size:** each position is worth capital × `Size %` × leverage, worked out from the **starting**
  capital, at the price when it opens. Profits aren't compounded unless `compound` is set, which sizes from the
  account as it is at the time. `Size %` is a parameter of every
  strategy, 10 unless set, from 1 to 100.
- **Fees:** Binance markets pay 0.1% of each fill unless `settings` say otherwise; imported markets pay
  what they were imported with.
- **Warm-up:** on time bars, up to 500 bars before `from` load first. The strategy sees them but doesn't
  trade, and they aren't in the results.
- **At the end:** a position still open counts at the last close, with no closing fee.

**What backtests leave out,** so they flatter a strategy:

- A decision and its fill share one close. Real orders go after it, so set `slippage_percent` or `latency`. Prefer
  `slippage_percent` to `slippage`, which is in ticks and so means different things on different markets.
- Stops are checked at closes too, so one fills at the close that broke it, which may be well past its level.
- Shorts are allowed on spot markets, with no borrowing or funding costs. Paper and live spot accounts can't short.
- No margin calls or financing costs, and any size fills in full, while the account can pay. If it can't, as with
  `Size %` near 100 or high leverage, the order's refused, the position stays as it was, and the summary's `warnings`
  says so. Such a result doesn't follow the strategy: don't trust it.
- Every bar, the position is checked against what the strategy asked for. A mismatch adds to `warnings`: one with no
  order refused is a fault in the backtest, and one where the position rounds to nothing means raising the capital or size.
- Unless `compound` is set, a strategy trading in and out falls behind `buy_and_hold_return` as profits build. To
  compare fairly, use `Size %` 100 and `compound`.
- One market at a time: there are no portfolios or limit orders.

A strategy is `{"builtin": id}`, `{"plugin": id}`, `{"logic": tree}` or `{"positions": list}`, each with optional
`"params"`.
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

**Now:** `position` is what's really held (1, -1 or 0), `entry_price` what it filled at (not ready while flat),
and `bars_held` the bars since (1 on the bar after its fill, 0 while flat). `high_since_entry` and `low_since_entry`
are the furthest it's gone since, for trailing stops, also not ready while flat. `hour` and `weekday` (Monday is 0) are
UTC, at the bar's close. So exits work like this:

```json
{"tool": "run_backtest", "args": {
  "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-03-01",
  "strategy": {"logic": {
    "name": "Breakout with a stop, out after two days, weekdays only",
    "params": {"stop": 0.97, "most_bars": 48},
    "long": {"and": [
      {"crosses_above": ["close", {"ago": {"of": {"highest": {"length": 50}}, "bars": 1}}]},
      {"lt": ["weekday", 5]}
    ]},
    "flat": {"or": [
      {"lt": ["close", {"mul": ["entry_price", "$stop"]}]},
      {"ge": ["bars_held", "$most_bars"]}
    ]}
  }}
}}
```

A trailing stop follows the furthest it's gone since entry:

```json
{"tool": "run_backtest", "args": {
  "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-03-01",
  "strategy": {"logic": {
    "name": "Cross, with a 5% trailing stop",
    "params": {"trail": 0.95},
    "long": {"crosses_above": [{"ema": {"length": 10}}, {"ema": {"length": 30}}]},
    "flat": {"lt": ["close", {"mul": ["high_since_entry", "$trail"]}]}
  }}
}}
```

An exit beats an entry to what's already held, so a stop always works, with no need to guard the entry. A guard only
matters for going back in: once out, an entry that's still true goes back in on the next bar, so write one that fires
once, like a cross, or also require `{"lt": ["position", 1]}` and your own condition for being ready again. `highest`, `lowest`, `zscore` (population deviation) and `high_since_entry` include
the current bar; `high_since_entry` starts from the entry price, then each later bar's high.

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
    "compound": false,
    "slippage_percent": {"on": true, "value": 0.01},
    "session": {"on": true, "value": {
      "days": [true, true, true, true, true, false, false],
      "from": 570, "to": 960, "zone": "America/New_York", "close_after": false
    }},
    "precision": "standard"
  }
}}
```

Most are `{"on": bool, "value": ...}`, and do nothing while off. A setting that doesn't exist, or one with no `on`, is
an error.

| Setting | Value |
|---|---|
| `stock` | `false` turns all of the below off at once, except `capital` and `precision` |
| `capital` | Starting cash, in the quote currency |
| `commission` | `{"percent": 0.1}` of each fill, or `{"per-order": 1.5}` flat, instead of the market's fee |
| `slippage` | Ticks every market order fills worse. A tick is `10^-price_precision`, from `get_instrument` |
| `slippage_percent` | Percent of the price every market order fills worse, on top of any in ticks: `0.01` is 1 basis point |
| `compound` | `true` sizes positions from the account as it is now. Monte Carlo redraws such trades' amounts as they were |
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
- **`buy_and_hold_return`** is what the market did, from the first bar's close that could trade to the last.
- **`sharpe`** is the Sharpe ratio of daily equity returns, with no risk-free rate, over 365 days a year. It's null
  under two days, or if nothing moved. Equity includes capital not in the position, so it falls with `Size %`:
  compare runs of the same size.
- **`profit_factor`** is winning trades' profit over losing trades' loss, null if there were no losses.
- **`exposure`** is the fraction of bars that began with a position open.
- **`average_trade`** is net profit per trade, in the quote currency, after fees.
- **`by_year`** is what each UTC year added, as a fraction of the starting capital, so the years add up to `net_return`.
- **`return_percent`** on a trade is a percentage of the starting capital, even with `compound`, so add them up rather
  than compounding them.
- **Times:** `ts` is nanoseconds since 1970, and `time` is `YYYY-MM-DD HH:MM`. All are UTC.
- **A bar's `ts` is when it opens.** Its order fills at its close.
- **`from` and `to`** take `2024-01-31`, `2024-01-31 09:30` or nanoseconds. `to` isn't included.
- **`win_rate`, `ruin` and `profitable`** are fractions of trades, runs and blocks.
- **`fills` and `trades` differ:** a trade is flat to flat, and a reversal is one fill in two trades.
- **Null** in a number means it wasn't a number: often an indicator not ready yet.

`get_report`'s `pnl` agrees with `summary`: it covers every trade, counting one still open at the end at its last
value, and `ends_with_open_trade` says if there is one. `Max Loser` is the largest loss and `Min Loser` the smallest.
`closed_positions_pnl`, `returns` and `general` come from Nautilus under its own names, and `closed_positions_pnl`
leaves out an open trade, so a run holding to the end shows only its entry fee there.

**What a time means, by tool.** Getting this wrong by one bar invalidates a model's test:

| Where | It's when |
|---|---|
| `ts` in `get_bars`, `export_data`, `positions` and `get_equity` | The bar opens. `get_equity`'s value is at that bar's close |
| `ts` in `get_fills`, and `opened` and `closed` in `get_trades` | The bar it filled at closes, so a list's entry fills one bar's length after its `ts` |
| `ts` sent to a program over a socket, and `hour` and `weekday` | The bar closes |
| `bar`, `entry_bar` and `exit_bar` | Not a time: numbers counting warm-up bars |

A random block's `to` is its last bar, which is included. To replay one with `run_backtest`, whose `to` isn't
included, give a `to` one bar later.

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
- **For training models elsewhere,** `export_data` writes CSV or Parquet. Then test the predictions as a list of
  positions: see "Using a model you trained" in the `extending` topic.

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

`actual.fell` works from closed trades, so it can be less than the run's `max_drawdown`, which falls bar by bar. This
shows the range of luck in those trades. It doesn't show whether the entries beat chance: for that, see
below.

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
- **`hold_out`** scores on the span's first 70% and gives the top 5's `unseen_return` on the last 30%, with
  `held_out_buy_and_hold` for what the market did there. It's off unless asked for.
- **Values with no score:** `status` says `too-few-trades` or `failed`, and the rest carry on.

Walk-forward picks the best values on a stretch, tests them on the next, unseen, and slides along.

```json
{"tool": "run_walk_forward", "args": {
  "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-07-01",
  "strategy": {"builtin": "ema-cross", "params": {"Slow": 40}},
  "variables": [{"name": "Fast", "min": 5, "max": 15, "step": 5}],
  "windows": 4, "train_ratio": 3
}}
```

Each of `windows` says what it picked, its `unseen_return`, and the market's `buy_and_hold_return` over it. The
top-level figures add them up, as each stretch starts from the same capital. It fills on the chart's bars whatever the `precision`.

**Against chance:** `run_random_entries` places a run's own holds and directions at random times, on the same bars and
settings, and says how many placings did as well.

```json
{"tool": "run_random_entries", "args": {"run_id": "$RUN", "runs": 200, "seed": 1}}
```

`share_matching_return` is the share that did as well, so a low one is good, and `share_matching_fall` likewise for the
deepest fall. It doesn't allow for how many strategies you tried before picking this one: picking the best of 100 will
often give 1 in 100 by luck.

Give any backtest a `label` to keep experiments apart. `list_runs` finds and sorts them, with
`{"label": "my-idea", "sort": "sharpe"}`, by any summary figure.

## jobs

Backtests and tests may take a while. Each waits `wait_seconds` (30 by default), and if it isn't finished
answers with `status: "running"` and a `job_id` rather than holding the call open.

```json
{"tool": "get_job", "args": {"job_id": "$JOB", "wait_seconds": 60}}
```

`get_job` gives `done` and `total` while running, and `status: "done"` with `result` when finished, or the
error if it failed. Give it `wait_seconds` rather than asking repeatedly: it answers the moment the job
finishes. `cancel_job` stops it, and it shows `status: "cancelling"` until it has. Then it's `status: "cancelled"`
with `done` and `total`, or `status: "done"` with what it got if the tool can answer in part. Answers are kept, so
a job can be read after a restart.

Backtests run in parallel up to one less than the machine's cores, however many calls they come from. The
rest wait their turn, so calling in parallel is safe.

## recipe

A sensible order for testing an idea, cheapest first. Nothing here is required.

1. **Try it once, with costs:** `run_backtest` with `slippage_percent` set, and read `warnings`. If it loses before costs
   too (set `commission` to 0), stop: cheaper fills won't save it.
2. **Compare fairly:** `Size %` 100 and `compound` against `buy_and_hold_return`, and look at `by_year`, as one good year
   often explains everything.
   To test a model trained elsewhere, see "Using a model you trained" in the `extending` topic.
3. **Vary it:** `run_backtests` across parameters, symbols and costs, with a `label`. A real effect survives nearby values.
4. **Against chance:** `run_random_entries` on the best, remembering how many you tried.
5. **On unseen data:** `run_walk_forward`, or `run_optimize` with `hold_out`, each with the market's return beside it.
6. **Throughout the market:** `run_random_blocks`, and `run_monte_carlo` for the range of luck in its trades.
7. **Show someone:** `save_result`.

The built-in strategies differ more than their names say. None ever goes flat by itself: once in, each holds a side until
the opposite signal, so `exposure` is near 1.

- `ema-cross`: long when the fast EMA crosses above the slow one, short when it crosses below.
- `rsi-reversion`: long when RSI climbs back above oversold, short when it drops back below overbought.
- `donchian-breakout`: long on a close above the previous `Length` bars' highest high, short below their lowest low.

## paging

Long answers come in pages: `columns`, `rows` and `next_cursor`. Pass `next_cursor` back as `cursor` until
it's null. Pages are 100 rows unless `limit` says otherwise (`list_runs` gives 20, and `get_call_log` 50),
up to 50,000. A page of 100 trades costs about 5,000 tokens, so ask for what you need: a run's `summary` is
usually enough, and `export_data` suits anything large.

Over MCP, an answer over about 100 KB is saved to a file rather than sent: you get `too_large` and its `path`.

## results

Showing a strategy to the person using the app, and seeing what's on their charts.

```json
{"tool": "save_result", "args": {"name": "RSI bounce", "strategy": {"builtin": "rsi-reversion"}, "run_id": "$RUN", "note": "Worth a look on 4h"}}
```

It appears in the app's backtest panel straight away, marked Lab, to chart and replay.

`get_layout`, `get_chart_settings` and `get_drawings` show what's on their charts, and `get_call_log` shows
every call made to Lab, newest first.

## extending

Adding markets, strategies and models.

`import_data` and `fetch_archive` add markets. `list_indicators` lists built-in and plugin indicators, and `run_model`
runs an ONNX model from `list_models`.

**Warm-up and bar numbers:** bar numbers in `bar`, `entry_bar` and `exit_bar` count warm-up bars, so the first that can
trade is usually number 500. A program over a socket is sent warm-up bars too, and what it last asked for during them
is taken up on the first bar that can trade.

**Using a model you trained, three ways, simplest first:**

1. **A list of positions,** written out or as a JSON file's path, as `"positions": "/path/list.json"`, which spares you
   writing long lists into calls. Export bars with `export_data`, make predictions, and backtest them as
   `{"positions": [[ts, 1], [ts, -1], [ts, 0]]}`. Each `ts` is a bar's, from `get_bars` or the export: when it opens.
   It fills at that bar's close and holds until the next entry. A list can run past the span at either end: a span or
   random block starting later takes up what's wanted by then, and later entries are left out. Inside the span, a time
   that isn't a bar's is an error. It runs in parallel, but nothing checks the list only used what was known by then:
   train and predict on separate stretches. It can't go in `save_result`, as it only fits the bars it was made for.
2. **A program over a socket,** for a full Python stack. It's told one bar at a time, so it can't see ahead, but
   backtests run one at a time. Listen on a local socket, with a thread per connection, answering one JSON line per
   request:

   | It's sent | It answers |
   |---|---|
   | `{"type":"describe"}` | `{"name":"…","description":"…","params":[{"name":"Fast","default":10,"min":1,"max":100}]}` |
   | `{"type":"start","params":[10.0]}` | `{}` |
   | `{"type":"bar","ts":…,"open":…,"high":…,"low":…,"close":…,"volume":…}` | `{"target":1}`, `-1` or `0` |
   | `{"type":"end"}` | `{}` |

   Any answer may be `{"error":"…"}`, which stops the run. Here `ts` is the bar's close. Add it while it's running
   with `install_plugin` and a `name` and `address`, then use it as `{"plugin": id}`.
3. **A compiled plugin,** by `install_plugin` and a `.wasm` file's `path`.

## limits

What Lab can't do.

- **No orders:** Lab can't trade. Paper and live trading are in the app.
- **No keys:** API keys aren't available through any tool.
- **No language models:** Lab can't call them, and a plugin that asks one gets no answer. ONNX models work.
- **No deleting or overwriting:** you can add markets, plugins and results, but not remove or change them.
  `export_data` won't replace a file.
- **Errors** are plain messages saying what's wrong and, where possible, what would be right.
