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
  Every change still goes out, each from where the order before it will leave things. `fill_timing: "next_open"` is
  the realistic delay for most uses: decide at a close, fill at the next bar's open, with no finer data.
- **A model's predictions** go in as a list of positions: see the `models` topic.
  With `precision: "high"`, it fills on what's next inside the bars instead: ticks, or 1-minute bars. So `high` only
  changes results alongside `latency`, or on ticks, and otherwise just takes longer.
- **A strategy says what position it wants:** long, short or flat. One position at a time, in one
  direction; reversing is one order, twice the size. Orders go at the market unless `limit` is set.
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
- A stop in a logic tree is checked at closes, so it fills at the close that broke it, which may be well past its level.
- Shorts are allowed on spot markets, with no borrowing or funding costs. Paper and live spot accounts can't short.
- No margin calls or financing costs, and any size fills in full, while the account can pay. If it can't, as with
  `Size %` near 100 or high leverage, the order's refused, the position stays as it was, and the summary's `warnings`
  says so. Such a result doesn't follow the strategy: don't trust it.
- Every bar, the position is checked against what the strategy asked for. A mismatch adds to `warnings`: one with no
  order refused is a fault in the backtest, and one where the position rounds to nothing means raising the capital or size.
- Unless `compound` is set, a strategy trading in and out falls behind `buy_and_hold_return` as profits build. To
  compare fairly, use `Size %` 100 and `compound`.
- `run_backtest` is one market. Several on one account are `run_portfolio`: see `portfolios`.
- **`limit` orders** rest from the close by `offset_percent`, for `expires_bars`. They fill only when price trades **through**
  them, as a bar can't show a place in the queue, at their own price, even on a gap past them, and pay the maker's fee. An entry
  that doesn't fill lapses, and an exit goes at the market, including all of a reversal at the taker's fee. A level-based strategy will
  place it again next bar, and each counts as a new order in `fill_rate`. It can't go with `latency` or `fill_timing: "next_open"`.
  **Limit fills flatter unless tested.** A bar can't show the queue ahead of an order, so by default one tick through counts as filled. Set
  `through_percent` to require the market to trade that far past first, at its own price all the same: `0.01` is 1 basis point. A trial found
  that alone turned a small profit into a loss, so run a result at a few values. Orders that fill also tend to do worse than those that
  miss, as price runs away from the latter. `limit_summary` compares them, `get_limit_orders` lists every order, and `warnings` says when
  the gap's wide. So treat a small profit that depends on limits as unproven.
- **`stop_loss_percent` and `take_profit_percent`** are real orders. After a market entry they rest through the very next bar. After a
  limit entry, which fills partway through a bar, they start at that bar's close, so that bar's own low isn't checked against the stop,
  which flatters. A stop fills at its level, or the open on a gap, plus slippage. A target fills at its level, and only when price trades
  through it. On a bar reaching both, the stop's assumed to have come first. A strategy still asking for the position goes back in next
  bar. They can't go with `fill_timing: "next_open"`.
- **Perpetuals:** a symbol like `BTCUSDT-PERP` is Binance's USD-M perpetual, with its bars, and 0.02% maker and 0.05% taker fees
  unless `commission` says otherwise. Funding falls due on what's held going into the bar it's in, at that bar's open, in `funding_paid`,
  positive when it cost, and `get_funding` lists the rates. A limit filled partway through a bar counts from that bar's close. Trades in
  `get_trades` don't include funding, though `net_return` and equity do. `buy_and_hold_return` doesn't either, so compare with
  `benchmark.return_after_funding` on a perpetual. Spot shorts cost nothing unless `borrow_percent_per_year` is set, and that
  isn't realistic.
- **`impact_percent`** is a rough assumption: market orders fill worse by it times the square root of the order's share of its bar's volume.
  `warnings` says when an order's over a tenth of its bar, where fills are fiction anyway.
- **`trailing_stop_percent`** rests like `stop_loss_percent`, but follows the best price since entry, moved at each close, so it only tightens. Inside a bar it stays where the last close left it. If a close is already past where it would go, the
  position goes at that close. It can't go with `fill_timing: "next_open"`, and portfolios refuse it. Logic trees can still write their own with `high_since_entry`, checked at closes.
- **`max_participation_percent`** limits an order to that share of the volume of the bar just closed. What's left goes out at the next
  closes, at their prices, so `orders_worked`, `bars_to_fill_average`, `bars_to_fill_longest` and `unfilled_at_end` say how that went,
  and `warnings` says if a position never got there. A different wish replaces what's still to go and works from what's held. A resting
  limit fills no more than that share of each bar, its `filled` in `get_limit_orders`. A stop takes that share at its level, and the rest
  goes at the closes after. It's a real constraint rather than a cost, so `gross_return`'s run keeps it.
- **`range_slippage_percent`** makes takers pay that percentage of a bar's high to low, times how many times wider the bar is than the median
  of the last 100, if it's wider at all. So a bar 10 times the usual width costs 100 times a usual one. A market order reads the bar just
  closed and a stop the bar it fills in, as that's the venue's conditions and not something the strategy saw. Limits and targets pay none.
  Each fill's `range_slip` in `get_fills` says what it took per unit of the market, in price, so multiply by its size before adding them up. It's a rough assumption: to stress a strategy, sweep it with `run_backtests`.
- **`latency_jitter`** adds an exponential draw averaging that many milliseconds to each order's `latency`, `latency_seed` choosing the draws
  so a run repeats. An order sent at a close reaches the next bar however long it took, so only `precision: "high"` on ticks can show it,
  and a note says so otherwise. It can't go with limits or `next_open`.


A strategy is `{"builtin": id}`, `{"plugin": id}`, `{"logic": tree}` or `{"positions": list}`, each with optional
`"params"`.
`list_strategies` lists them, and `describe_strategy` gives one's parameters and limits.

## portfolios

`run_portfolio` runs several markets on one account: one cash balance, one timeline. `run_portfolios` sweeps many in one call.

```json
{"tool": "run_portfolio", "args": {
  "legs": [{"symbol": "BTCUSDT", "weight": 0.5}, {"symbol": "ETHUSDT", "weight": -0.5}],
  "bars": "1h", "from": "2024-02-01", "to": "2024-03-01",
  "rebalance": {"every_bars": 24, "min_trade_value": 100},
  "settings": {"slippage_percent": {"on": true, "value": 0.01}}
}}
```

- **What decides a leg's position, one of three.** A `strategy` on the leg, as anywhere else, with `weight` multiplying its position size. No
  strategy: it holds `weight`, a signed share of the capital. Or `weights`, a table for the whole book, from your own code: this is how to
  do pairs, rotation, risk parity and delta-neutral. `[[time, {"BTCUSDT": 0.5, "ETHUSDT": -0.5}], ...]` as signed shares of the account,
  a market a row leaves out going flat. A row stamped with a bar's open applies at that bar's close, as position lists do, so stamp it with
  the bar it was worked out from. With a table, a weight of 2 is twice the account, and needs `leverage` to match.
- **Rebalancing** takes the whole book back to the weights `every_bars`, or when any leg is `drift_percent` points of the account away, skipping
  trades worth under `min_trade_value`. Rebalanced and table runs size from the account as it is now. Without either a leg stays as it was sized.
  With costs off it trades other sizes, and on drift at other times, so on these and on table runs `gross_return` less `net_return` isn't just the costs:
  `total_costs_paid` is what was really paid. `warnings` says so on any run where the two differ by more than a tenth.
- **Cash below zero** on what's bought outright is free unless `cash_interest_percent_per_year` is set (perpetuals aren't bought with cash, and pay funding instead), which flatters weights adding up to over 1. `interest_paid` says what
  it came to, and it's in `total_costs_paid`. Interest not paid goes on the loan, as on a margin account, so it compounds every bar: a year at 5% on
  a loan that stays the same costs about 5.13% of it, not 5%. A market is a perpetual when it's given funding (a `-PERP` symbol), even if no
  funding fell due in the span. `warnings` says if the book needed well over the account in margin.
- **A row before `from`** takes effect at the span's first bar, the last such row winning. Rows out of order are put in order. A row between
  closes waits for the next one. Rows after the last bar do nothing, and `warnings` says so.
- **One timeline.** Every moment any leg's bar closes is a moment for all of them. A leg with no bar then carries its last close at no volume,
  so cash, equity and margin always read every market at the same moment. It can't trade there, and signals don't hear it. `warnings`
  says how many a leg had. A leg trades from its first bar, and a table's weight before that is reported.
- **Funding** is snapped to the nearest minute and settled across all legs in the same step, net into the one balance. Each leg pays its own
  fees. Shorts pay `borrow_percent_per_year` only on legs with no funding.
- **Cross-margin.** A profit on one leg supports another. Margin is the sum of position values over `leverage`. When the account falls to
  `maintenance_percent` of what's held (0.5 by default), everything is liquidated at that close, whatever `max_participation_percent` says, and
  nothing trades after. The account's judged at each market's worst in the bar, as bars can't show what came first, so a hedged book may be
  cut sooner than it really would. `warnings` says if the book ever needed more margin than the account had, which isn't refused here but a venue wouldn't allow. `peak_margin_use` and `closest_to_liquidation` say how near it came, the latter as a multiple of what was
  needed, so 1 is liquidated. Exchanges' discounts for hedged books aren't modelled, so a hedge gets no margin relief: those are venue-specific
  stress tables, and a rough version would flatter delta-neutral books.
- **By time:** `by_hour` and `by_weekday` in a summary are what trades made by the UTC hour and weekday (Monday first) they started. They add up to everything trades made, and the app draws them in the Calendar view.
- **Reading one.** `get_portfolio` has the summary, a table by leg, and `correlation`, which is of the markets' own returns, whichever way they're held, so a long and a short of markets moving together shows near +1. `get_portfolio_series` has equity, equal shares held,
  exposure, and each leg's value and what it's made at each moment. `get_portfolio_fills` has one leg's fills. `benchmark_return` is an equal
  share of the capital held in each market from its first bar, with no funding taken off, and `benchmark_after_funding` with it taken off, which `excess_return` is
  against. `weights_held_return` holds the weights asked for without rebalancing, funding paid, which is the fairer comparison for a long and short book. It's null when
  strategies or a table that switches decide the positions, as there's then no one set of weights to hold. `list_runs` and `compare_runs` include them, with `kind: "portfolio"`. The app saves its portfolio runs in the same place, so these tools read those too, and its Saved runs list shows these.
- **`fill_timing: "next_open"`** works as on one market: an order decided at a close goes at the next bar's open, with each leg at its own open at
  the same moment. A leg with no next bar waits for the close before one it has, as its last price isn't one anyone could trade at. Liquidation
  is then at the next open, gap and all, so an account can end below zero. This is the fair way to run a weight table: the delay between
  working out a weight and trading it often takes much of an edge.
- **Is it luck?** `run_shifted` takes any run, of one market or a portfolio, and moves its positions or weights along the span, wrapping round, so what's held and for how long stay
  the same and only the timing goes. `share_matching_return` near 0.5 means the timing added nothing. For a portfolio, its book becomes a table of weights, and `actual_return` there is that table run with no shift, as sizing
  from weights isn't quite how a strategy sizes. A table run is already a table, so there it's the run itself. `run_portfolio_monte_carlo` draws the account's returns again in blocks along the timeline, keeping how the markets moved together. It doesn't run fills or costs again.
  `run_portfolio_walk_forward` picks among `candidates` on each stretch and trades the pick on the unseen one after. A candidate with `"weights": {}` stays flat, and is picked when all else loses. A candidate that can't be run stops the call, saying which `item` it is. In its answer `buy_and_hold_return` is equal shares of the markets, with no funding taken off. It and `unseen_return` add up each window from the same capital, and `unseen_compounded` and `buy_and_hold_compounded` run them one after another. `buy_and_hold_whole` is equal shares held once from the first unseen bar to the last, with no gaps between windows: the market to beat.
- **What it doesn't do yet.** Market orders only: `limit`, stops and targets are for single
  markets, and are refused. `precision: "high"` does nothing here, as portfolios fill on bars. `run_optimize`, `run_monte_carlo`, `run_walk_forward` and `run_random_entries` don't take a portfolio, so
  sweep with `run_portfolios`. A plugin can be a leg or write a weight table, but can't see several markets at once. All legs must be quoted in
  one currency and share time bars. Paper and live trading don't run them.
- **A check on all of this:** one leg at weight 1 with its strategy comes out exactly as `run_backtest` does: its fills, equity, costs and figures.

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
  `highest`, `lowest` (both include this bar), `roc` (percent change over n bars), `zscore` and `stddev` (both over the window, as a population). `of` is the
  close unless given, and can be another indicator. `atr` only takes `length`.
- **Other values:** `{"ago": {"of": value, "bars": n}}`, `{"add": [a, b]}`, likewise `sub`, `mul`, `div`, `min` and `max`, and `{"abs": value}`.
- **A longer timeframe:** `{"on": {"timeframe": "1d", "value": value}}` works the value out on bars of that timeframe, built from yours. A longer bar is only seen once it's over: at the close of your bar that ends it, or if that's missing, when the next bar comes. So `{"on": {"timeframe": "1d", "value": "close"}}` is yesterday's close all through today. Lengths inside count its own bars, so `sma` of 20 on `1d` needs 20 days, which `warm_up_bars` has to cover.
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
| `stock` | `false` turns all of the below off at once, except `capital` and `precision`. A market's own fees and a perpetual's funding aren't settings, so they stay. `warnings` names any switched on that it ignored |
| `capital` | Starting cash, in the quote currency |
| `commission` | `{"percent": 0.1}` of each fill, `{"per-order": 1.5}` flat, or `{"rates": {"maker": 0.02, "taker": 0.05}}`, instead of the market's fee. A maker's can be below zero, for a rebate |
| `limit` | `{"offset_percent": 0.05, "expires_bars": 3, "through_percent": 0.01}`: send orders as limits, `through_percent` being optional. See `backtests` |
| `stop_loss_percent` | A real stop this far from the entry |
| `trailing_stop_percent` | A real stop this far from the best price since entry, moved at each close. With `stop_loss_percent` too, whichever is nearer goes first. See `backtests` |
| `decision_log` | `true` keeps a row for each bar, read with `get_decisions`: what was asked, what was held, what the rules made of it, and why no order went out. A stop or target filling between closes gets a row of its own with the size it filled, saying whether the stop was the trailing one |
| `take_profit_percent` | A real target this far from the entry |
| `borrow_percent_per_year` | What borrowing costs a short |
| `impact_percent` | How far a market order fills worse at all of its bar's volume |
| `max_participation_percent` | The most of a bar's volume an order can take. See `backtests` |
| `range_slippage_percent` | What takers pay on top from a bar's range. See `backtests` |
| `latency_jitter` | Milliseconds orders take on top of `latency` on average, drawn for each. See `backtests` |
| `latency_seed` | Which draws of `latency_jitter` a run uses |
| `range_look_back` | How many bars back `range_slippage_percent` judges a bar's usual range from. Default 100 |
| `impact_exponent` | How steeply `impact_percent` grows with an order's share of its bar's volume: 0.5, the default, is the square root, 1 in proportion |
| `volume_warning_percent` | With impact on, orders over this share of their bar's volume get a warning. Default 10 |
| `fill_warning_percent` | Fewer than this share of limit orders filling gets a warning, from 5 orders up. Default 50 |
| `cash_interest_percent_per_year` | What cash below zero costs in a portfolio, in `interest_paid` |
| `warm_up_bars` | How many bars before `from` strategies warm up on. Default 500. Raise it for longer look-backs |
| `hold_out_percent` | How much of the span an optimization with `hold_out` picks on. Default 70 |
| `walk_ratio` | How many times longer than its unseen stretch a walk-forward window picks on. Default 3. `train_ratio` on the call wins |
| `no_costs` | `true` means nothing to pay at all: no fees, funding, slippage, spread, impact, borrowing or interest, whatever else says. Unlike `stock: false`, it takes off a market's own fees and funding |
| `sweep_min`, `sweep_max`, `sweep_step` | What the app's Portfolio Sweep and Walk-forward go through. Calls to `run_portfolios` and `run_portfolio_walk_forward` give their own setups |
| `portfolio_block_bars` | How long a block is in the app's Portfolio Monte Carlo. `block_bars` on `run_portfolio_monte_carlo` is the same for calls |
| `portfolio` | What the app's Portfolio test trades: legs, weights and rebalancing, as `run_portfolio` takes them. Calls to `run_portfolio` don't read it |
| `slippage` | Ticks every market order fills worse. A tick is `10^-price_precision`, from `get_instrument` |
| `slippage_percent` | Percent of the price every market order fills worse, on top of any in ticks: `0.01` is 1 basis point |
| `compound` | `true` sizes positions from the account as it is now. Monte Carlo then draws such trades as returns on the account and multiplies, so a fall can't pass 100% |
| `fill_timing` | `"close"`, or `"next_open"` to fill at the next bar's open. It needs time bars, and can't go with `latency` or `precision: "high"`. Unlike the costs, `stock: false` leaves it alone. A fill's `ts` is 2 ms after that open, and its `bar` is the bar opening. Nothing fills for a decision on the last bar |
| `spread` | Ticks of spread assumed for bars and trades |
| `latency` | Milliseconds before orders reach the venue |
| `leverage` | A multiple of position size, 1 or more. Margin calls aren't simulated |
| `direction` | `"long-only"` or `"short-only"`; the other side's signals close instead of opening |
| `invert` | `true` takes the opposite of every signal, before `direction` applies |
| `session` | When positions may open: `days` from Monday, `from` and `to` in minutes after midnight, `zone` as `UTC`, `-05:00` or `America/New_York`, and `close_after` to close when it ends. Outside it a signal can close a position, but not open or reverse one. An end before its start runs overnight, and equal times are all day |
| `precision` | `"standard"`, or `"high"` to fill on ticks or 1-minute bars. Slower |

The panel's own choices, `test`, `runs`, `ruin`, `blocks`, `block_length`, `windows`, `entries`, `optimize`,
`plugins` and `stock_open`, are accepted and ignored here. Tests take their own arguments instead.

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
- **`gross_return`** is `net_return` from a second backtest with fees, slippage, spread, impact, funding and borrowing off, so it's exact with `compound`
  and with an account that ran out. **`total_costs_paid`** is fees, funding and borrowing, plus what slippage, spread and impact took, in the quote
  currency. **`break_even_cost_per_fill`** is the cost per fill that would bring the costless run to nothing, in basis
  points of the value traded: negative if it loses before costs. It's a guide under `compound`, where costs also
  shrink later positions. A run with costs takes twice as long, so `friction: false` leaves these out for huge
  batches. A plugin that doesn't repeat itself may differ between the two runs.
- **`benchmark`** has `return`, `sharpe` and `max_drawdown` for buying at the first close that could trade and holding to
  the end, with no costs. `return` is the same as `buy_and_hold_return`.
- **`limit_orders`, `limits_filled` and `fill_rate`** say how limits went, and `maker` in `get_fills` which fills rested.
  **`limit_summary`** has how far the market then moved each order's way, on average, as percentages: `filled_move_1` and `missed_move_1`
  a bar on, the same with `_24` for 24 bars on, and `barely_filled`, how many fills went under a hundredth of a percent past their price.
  **`get_limit_orders`** lists each order, with `reached_percent` for how far past its price the market traded, below zero if it stayed off.
  **`maker_fees` and `taker_fees`** split the fees, and **`funding_paid` and `borrow_paid`** are what holding cost.
- **Costs by kind:** `total_costs_paid` is `maker_fees` + `taker_fees` + `slippage_paid` (the set ticks and percentage) +
  `spread_paid` (half the assumed spread, crossed by each order that took) + `range_slippage_paid` (the volatility setting) +
  `impact_paid` (size against the bar's volume) + `funding_paid` + `borrow_paid` (+ `interest_paid` in a portfolio) +
  `cost_rounding`, which is what rounding prices to the tick left over, so they add up exactly. `run_backtests` and
  `run_portfolios` have them as columns. The assumed spread counts even when the data brings its own quotes.
- **`beta`** is how far the account moved with the market, from daily returns: the market's own for one market, equal
  shares of its markets for a portfolio. 1 moves with it, 0 owes it nothing, below 0 moves against it. A strategy that
  only beats the market by holding more of it shows a beta over 1.
- **`fills_on`** in a summary says how orders filled: `bars`, `ticks`, `minute_bars` or `next_open`.
- **`get_equity`'s `market`** is the app's market line: what holding the market with a position the run's size would be
  worth, from the first close that could trade. It's null on runs saved before it was added.
- **Charts:** what the app's panel draws comes with the answers too. `run_walk_forward`'s `chart` has the unseen stretches
  and the market held in full joined end to end, as percentages, at about 200 points, with where each window starts. `run_random_entries`' `return_chart`
  and `fall_chart` each have the range from `from` to `to`, `counts` in equal steps across it, and the backtest's `actual`.
  `run_optimize`'s `chart` has every score in the grid, row by row, `across` one variable's values and `down` the other's.
  The `results` topic lists every screen in the app and where its data comes from.
- **`by_year`** is what each UTC year added, as a fraction of the starting capital, so the years add up to `net_return`.
  **`by_year_compounded`** is each year's return on what the account held going into it, so the years compound to `net_return`; it's
  what a year really returned once earlier years grew or shrank the account. `null` for a year that began with nothing left.
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

A random block's `to` is its last bar, which is included. To replay one with `run_backtest`, whose `to` isn't
included, give a `to` one bar later.

## data

Which bars a call uses, their limits, and what's cached.

```json
{"tool": "get_bars", "args": {"symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "limit": 500}}
```

- **`bars`:** `1m`, `5m`, `15m`, `30m`, `1h`, `4h`, `1d`, `1w` or `1M`. On local markets, also `100t` for every
  100 ticks and `2.5v` by volume. Default `1h`.
- **Binance markets:** any spot symbol, with no `venue`, or a USD-M perpetual as `BTCUSDT-PERP`. Only **closed** bars are used.
- **Local markets:** `venue` from `list_markets`.
- **Without `from`:** the latest 5,000 bars, so a call with no `to` moves as new bars close. A summary's
  `first_bar` and `last_bar` say which bars it used. Give both ends to get the same answer every time.
- **Limits on a backtest:** 300,000 bars from Binance, and 1,000,000 1-minute bars for `precision: "high"`.
  `export_data` has no such limit.
- **Caching:** Binance bars are kept on disk, so a span costs a download once. A first call on a long span
  can take a while; later ones take milliseconds. Don't avoid repeating a span to save time.
- **For training models elsewhere,** `export_data` writes CSV or Parquet. Then test the predictions as a list of
  positions: see the `models` topic, which also says what a time means in each tool.

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

Its `chart` has the lines the app's fan draws, as equity: `median_line`, `backtest_line`, `best_line`, `worst_line` and
`middle_line`, thinned to about 100 points. `include_lines: true` adds every line as `lines`, and `run_random_blocks` has
the same for its blocks.

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
- **`deflated_sharpe`** is the chance the best's true Sharpe ratio is above nothing, given that it's the best of
  `variants_tried`: Bailey and López de Prado's deflated Sharpe ratio, which allows for how many were tried, how much their
  Sharpe ratios (`sharpe` on each value) differed, how many days it ran and how far its daily returns were from normal.
  Under 0.95 is weak evidence. It counts only this grid: count what else you tried yourself. Not given when scoring on
  random blocks. `run_backtests` and `run_portfolios` give it for their top row, with every run counted as a try.

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
top-level `unseen_return` and `buy_and_hold_return` add them up, as each stretch starts from the same capital, and so change with
how many windows there are. `unseen_compounded` and `buy_and_hold_compounded` run them one after another, and
`buy_and_hold_whole` holds the market once over all the unseen bars. `warning` says if a logic tree looks back further than
`warm_up_bars`, as each stretch is warmed up on only that many. Each window's `picked_sharpe` and `deflated_sharpe` say
how much its pick was luck where it was picked; `unseen_sharpe` is the unseen stretches' joined, which needs no deflating. It fills on the chart's bars whatever the `precision`.

**Against chance:** `run_random_entries` places a run's own holds and directions at random times, on the same bars and
settings, and says how many placings did as well.

```json
{"tool": "run_random_entries", "args": {"run_id": "$RUN", "runs": 200, "seed": 1}}
```

`share_matching_return` is the share that did as well, so a low one is good, and `share_matching_fall` likewise for the
deepest fall. On its own it doesn't allow for how many strategies you tried before picking this one: picking the best of
100 will often give 1 in 100 by luck. Say how many in `variants_tried`, counting every setting, rule and span you tried, and
`share_matching_adjusted` is the chance that at least one of that many worthless tries would have done as well: 3% after 11
tries is 28%. `run_shifted` takes it too.

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
2. **Compare fairly:** `Size %` 100 and `compound` against `buy_and_hold_return`, or for a strategy trading often, whose fees drain the account until orders are refused, a smaller size against holding at that size, and look at `by_year`, as one good year
   often explains everything.
   To test a model trained elsewhere, see the `models` topic.
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
every call made to Lab, newest first. Lists of positions can be saved too: they're written out in full, and work on
markets and timeframes whose bars they name.

**Moving a setup:** `export_state` writes saved results, the layout, drawings and which plugins are in use to one JSON file,
with `include_runs` for every run in full (gigabytes after long research), or one chart's setup if given a `strategy` or `settings`. It won't replace a
file. `import_state` adds a file's results and runs, keeping what's already here under the same id. It answers with any
`chart` to use in calls, `plugins_missing` to install, and `not_applied` for a layout or drawings, which only the app
imports. API keys, what AI models a plugin may use, plugins' code and market data never go in a file.

**Everything the app's panel shows, and where to get it:**

| In the app | From Lab |
|---|---|
| A chart's bars and studies | `get_bars`, `compute_indicator`, `get_chart_settings`, `get_drawings` |
| The span slider's history | `get_history_range`, then `get_bars` with `1M` |
| The strategy's list and its settings | `list_strategies`, `describe_strategy`, `list_results`, `get_settings` |
| Backtest: return, fall, fills, equity line | `summary`; `get_equity` |
| Backtest: the market line | `get_equity`'s `market` |
| Backtest: trade marks and replay | `get_fills`, `get_trades` |
| Backtest: how orders fill | `summary.fills_on` |
| Calendar: months, days and a day's trades | `get_calendar` with `year`, `month` and `day` |
| Statistics: Sharpe, win rate, profit factor, exposure | `summary` |
| Statistics: before costs, costs, break-even | `summary.gross_return`, `total_costs_paid`, `break_even_cost_per_fill` |
| Statistics: the market, and each year | `summary.benchmark`, `by_year` |
| Statistics: Sortino, and notes | `get_report`'s `returns` and `notes`; `warnings` in `summary` |
| Monte Carlo: its figures and fan | `run_monte_carlo`, with `chart` for the lines |
| Monte Carlo: table of runs and a run's trades | `best_runs` with `limit`, and `show_run` |
| Random blocks: its figures and fan | `run_random_blocks`, with `chart` for the lines |
| Random blocks: table of blocks and their trades | `blocks`, with `include_trades` |
| Optimize: the heatmap or bars | `run_optimize`'s `chart`, with `scores` across `across` and down `down` |
| Optimize: the best, and what they did unseen | `top`, `on_edge`, `held_out_buy_and_hold` |
| Walk-forward: chart and table | `run_walk_forward`'s `chart` and `windows` |
| Random entries: both charts | `run_random_entries`' `return_chart` and `fall_chart` |
| Portfolio: figures and the table by market | `get_portfolio` |
| Portfolio: the account and exposure charts | `get_portfolio_series` |
| Portfolio: Sweep | `run_portfolios` |
| Portfolio: Shifted, and Random entries' Shifted | `run_shifted`'s `return_chart` and `fall_chart` |
| Portfolio: Monte Carlo | `run_portfolio_monte_carlo` |
| Portfolio: Walk-forward | `run_portfolio_walk_forward` |
| A parameter sweep | `run_optimize`, or `run_backtests` across the values |
| Export and Import buttons | `export_state`, `import_state` |
| Backtest: Limits view, each order and how they compare | `get_limit_orders`, `summary.limit_summary` |
| A perpetual's funding, in its statistics | `summary.funding_paid`, `benchmark.return_after_funding`, `get_funding` |
| Statistics: limits filled, maker and taker fees, funding, borrowing | `summary` |
| Settings: limit orders, stop, target, impact, short borrow, maker and taker | `settings` |

Every line and chart is the same data the panel draws. Lab gives numbers rather than images. Only paper and live sessions
have no equivalent, as Lab can't trade.

## extending

Adding markets, strategies and models.

`import_data` and `fetch_archive` add markets. `list_indicators` lists built-in and plugin indicators, and `run_model`
runs an ONNX model from `list_models`.

**Warm-up and bar numbers:** bar numbers in `bar`, `entry_bar` and `exit_bar` count warm-up bars, so the first that can
trade is usually number 500. A program over a socket is sent warm-up bars too, and what it last asked for during them
is taken up on the first bar that can trade.

To use a model you trained, see the `models` topic.

## limits

What Lab can't do.

- **No orders:** Lab can't trade. Paper and live trading are in the app.
- **No keys:** API keys aren't available through any tool.
- **No language models:** Lab can't call them, and a plugin that asks one gets no answer. ONNX models work.
- **No deleting or overwriting:** you can add markets, plugins and results, but not remove or change them.
  `export_data` won't replace a file.
- **Errors** are plain messages saying what's wrong and, where possible, what would be right.
