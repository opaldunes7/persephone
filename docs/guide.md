# Persephone user guide

Persephone shows nothing but charts. Everything else is in the menu you get by clicking a
chart, in the strategy report, and in `persephone-cli`. This guide walks through each.

- [Starting](#starting)
- [Charts](#charts)
- [Tiles and workspaces](#tiles-and-workspaces)
- [Strategies and backtests](#strategies-and-backtests)
- [The backtest panel](#the-backtest-panel)
- [The calendar: replaying a day's trades](#the-calendar-replaying-a-days-trades)
- [Skill or luck: Monte Carlo and random blocks](#skill-or-luck-monte-carlo-and-random-blocks)
- [Optimizing a strategy](#optimizing-a-strategy)
- [Parameter sweeps](#parameter-sweeps)
- [Drawings](#drawings)
- [Your own data](#your-own-data)
- [Paper and live trading](#paper-and-live-trading)
- [Plugins, models and AI](#plugins-models-and-ai)
- [Where files live](#where-files-live)

## Starting

```bash
cargo run -p persephone --release                    # your last layout (BTCUSDT 1m the first time)
cargo run -p persephone --release -- ETHUSDT 4h      # one chart for a Binance pair
cargo run -p persephone --release -- EURUSD.LOCAL    # one chart for a market you imported
```

With arguments, the session's layout isn't saved, so your usual layout is still there next
time. Binance charts load the latest 5,000 bars of their timeframe.

## Charts

- **Right-click** opens the menu:
  - **Active**: everything on the chart (volume, indicators, the strategy, drawings), each
    with a checkbox. Unchecking hides it without removing it; checking brings it back at once.
    What's hidden is saved with the layout.
  - **Indicators ›**: the built-ins, then plugin indicators.
  - **Strategy ›**: the strategy to backtest on this chart.
  - **Draw ›**: drawing tools.
  - **Backtest**: opens the [backtest panel](#the-backtest-panel).
  - **Symbol**: search Binance pairs and your imported markets. Enter opens any Binance spot
    pair you type. **Import data…** at the bottom imports your own files.
  - **Timeframe**: 1m, 5m, 15m, 30m, 1h, 4h, 1d, 1w or 1M. Weeks start on Monday and months
    on the 1st (UTC), as on Binance.
  - **Tick & volume bars**: shown only for markets imported as ticks.
- **The header** (top left, e.g. `BTCUSDT · 1h · BINANCE`):
  - Click the symbol and it becomes a search box, with matching markets (yours first, then
    Binance pairs) dropping down as you type. Click one to switch, press Enter to open
    whatever Binance pair you typed, or press Esc to leave it as it was. **Import data…** at
    the bottom imports files, and the **×** beside one of your markets deletes it.
  - Click the timeframe for a dropdown of every timeframe.
  - After the name, round tags: **Vol** for the volume bars, then one for each indicator.
    A tag is lit while its indicator is shown and outlined while it's hidden. Click a tag to
    hide or show it (the same as its box in the Active list). **Indicators › Volume** also
    brings volume back.
  - A left click anywhere else on the chart closes what's open.
- **Drag** pans and **scroll** zooms.
- **Dragging the price axis** stretches prices; double-click it to fit prices again.
- **Dragging the time axis** stretches time: drag right for wider bars (fewer on screen), left
  for more bars. The newest bar on screen stays put. Double-click it for the default width.
- **Indicator legends** (top left) open that indicator's settings when clicked. Values check
  as you type, and Remove is at the bottom. Overlays (moving averages, bands) draw on the
  prices; oscillators (RSI, MACD, …) get a pane of their own below.
- **Hovering** a bar shows a crosshair on every other chart at the same moment, whatever their
  timeframe.

## Tiles and workspaces

The window is tiled like Hyprland's dwindle layout. A new chart splits the focused one along
its longer side, and closing a chart gives its space to its neighbour. Focus follows the
mouse.

| Keys | Action |
|---|---|
| Ctrl+Enter | new chart, showing the focused chart's market |
| Ctrl+Q | close the focused chart |
| Ctrl+H/J/K/L or arrows | move focus left/down/up/right |
| Ctrl+Shift+H/J/K/L | swap with the neighbour in that direction |
| Ctrl+Alt+H/J/K/L (Option on a Mac) | move the divider |
| Ctrl+T | toggle the split direction |
| Ctrl+F | fullscreen the focused chart (again to restore) |
| Ctrl+B | open or close the backtest panel on the focused chart |
| Ctrl+1…9 | switch to that workspace |
| Ctrl+Shift+1…9 | move the focused chart to that workspace |

Every chart also has three small buttons in the bottom-right corner, where the price and
time axes meet: **+** adds a chart (split from this one), **◫** flips the split direction,
and **✕** closes the chart. Hovering a button shows its shortcut.

On a Mac, the system already uses Control with the arrow keys (Mission Control), so use
Ctrl+H/J/K/L to move focus; the arrow versions only work where macOS doesn't take them. If
Control+number is set to switch desktops in System Settings, it will do that instead of
switching workspace.

A workspace holds up to 16 charts. The border flashes red when something can't be done (a
17th chart, closing the last one). Layouts, indicators, strategies and workspaces are saved
as you go and come back at the next launch.

## Strategies and backtests

Pick a strategy from **Strategy ›** and it backtests on the chart's bars at once:

| Strategy | Rule | Parameters |
|---|---|---|
| EMA Cross | long when the fast EMA crosses above the slow, short below | Fast, Slow |
| RSI Reversion | long when RSI climbs back above Oversold, short when it falls back below Overbought | Length, Oversold, Overbought |
| Donchian Breakout | long on a close above the last N bars' high, short below their low | Length |

Every strategy also has **Size %**: each position is worth that share of the starting capital
(100,000 in the quote currency, e.g. USDT, unless the [backtest panel](#the-backtest-panel)
says otherwise). Plugin and external strategies are listed below the built-ins.

The chart then shows:
- arrows at every fill, labelled with the position they leave (Long, Short, Close);
- a tint behind the bars while a position is open;
- an **Equity** pane with the account value at each bar;
- a summary in the top-right corner: return, drawdown, win rate, trades.

Click the summary for the **report**. It has the parameters (editing one reruns the backtest),
net profit, return, max drawdown, Sharpe, Sortino, profit factor, win rate, trades and fees.
Notes from the strategy go at the bottom, such as a language model's look-ahead warning.

**How backtests run:**
- They run on NautilusTrader with a simulated exchange: margin account, netting positions
  (reversing is one order), and Binance's 0.1% fee for Binance markets (an imported market
  uses the fee set at import, `--fee`).
- A strategy decides when a bar has closed, and its market order fills at that close, never
  earlier.
- With **High** intra-bar precision (in the backtest panel's settings), orders fill on what
  happened inside the bars instead: the ticks that follow on imported tick data (at the ask or
  bid for quotes, the trade price for trades), or 1-minute bars elsewhere. The report says
  which.
- Changing the timeframe, symbol, parameters or backtest settings reruns the backtest.

## The backtest panel

Press **Ctrl+B**, or choose **Backtest** from a chart's right-click menu or a strategy's report. A panel opens across the top of
the window for the focused chart (it gets a blue outline), drawn like a chart pane; **Ctrl+B**
closes it again:
- The equity curve as far as the playback has got, with the return, drawdown and trades so far
  in its legend.
- Play or pause, back to the start, and one bar on; hovering a button names it.
- **Span**: two handles over the market's whole history (prices on a log scale behind them).
  Drag a handle to move one end, or the top of the bar between them to move both; when you
  let go, the chart loads that span and backtests it. Strategies warm up on up to 500 bars
  before the span without trading. Click between the handles to jump the playback there.
  **Latest bars** goes back to the chart's latest bars.
- **Speed**: a long slider from real time (1×) on the left, through 10× in the middle, to
  1,000,000× and **Max** (the rest of the span in two seconds) on the right. The left half is
  for fine control at slow speeds. Scroll over it to nudge it; the readout says how many bars
  go by a second.
- **✕**, in the corner under the price axis, closes the panel. Dragging its bottom edge resizes
  it, but it never gets shorter than it takes to show all the settings.
- The left 30% holds the **strategy** dropdown (handy after Ctrl+B on a chart without one), the
  settings below, and a search that adds plugins as tags (click a tag to take it off). Hover a
  setting's name for what it does.

Playback reveals a finished backtest: the backtest runs once over the span, and playing,
pausing, stepping or changing speed never reruns it. While it plays, the chart shows only the
bars that had closed by then, with their trades and equity, and the strategy summary waits
until it's over.

### Backtest settings

**Stock settings** folds away with the arrow beside its name, for more room; they still apply, and
the fold shows how many are on.

Each setting has its own switch, and **Stock** turns them all off at once, for an
ideal run to compare against. They're saved with each chart.

| Setting | What it does |
|---|---|
| Starting capital | The account's cash, in the quote currency; paper trading starts with it too. |
| Commission | A percentage of each fill, or a flat amount per order, instead of the market's own fee. |
| Slippage | Every market order fills this many ticks worse. |
| Spread | A spread assumed for bar and trade data (quotes have their own): buys pay the larger half of it, sells the rest. |
| Latency | Orders reach the venue this many milliseconds late. |
| Leverage | A position is worth capital × Size % × leverage. Margin calls aren't simulated. |
| Intra-bar precision | **Standard** fills at the bars, and playback shows each bar as it closes: fast and light, best for long spans. **High** fills on ticks or 1-minute bars, and at slow speeds playback builds each bar as it formed, from the ticks (imported tick data), 1-minute bars (Binance, and 1-minute imports; they load in the background) or, with nothing finer, a simulated path from open through low and high to close. |
| Direction | Long only, or short only; the other side's signals close positions instead. |
| Trading session | Weekdays, a time window and a time zone (e.g. America/New_York, daylight saving included). Outside it, signals can close positions but not open them; optionally, positions close when it ends. |

Direction and the trading session apply to paper trading too. Position sizing models, dynamic
slippage, order-book fills, news-driven spreads and latency, holding costs, commission
schedules and exchange calendars are found through the plugin search. Those kinds aren't built
yet, so a tag has no effect on results for now; see
[the plugin guide](plugins.md#backtest-plugins-planned).

## The calendar: replaying a day's trades

**Calendar**, beside **Equity** in the panel's legend, shows what the backtest made when: it's for
understanding the days behind a result. It's part of **Backtest** in **Test**.

1. **Months:** a row for each year, shaded from red through dark to green, with the year's total.
2. **Days:** click a month for its days, a row for each week from Monday, with the week's total.
3. **Trades:** click a day for a table of the trades that closed that day: when each opened, long or
   short, entry and exit prices, how long it was held, and its profit in money and as a percentage
   of the capital. Hover for its size and fees.
4. **Replay:** click a trade. Playback pauses and the chart jumps to 10 bars before the trade opened,
   with later bars hidden. Press **Play**, or **Step** bar by bar, at any speed.

A trade counts on the day it **closed**, even if it opened days earlier; replaying goes to where it
opened. A trade still open at the end counts on the last bar's day, at that bar's close, so the
calendar adds up to the backtest's whole profit. Days are in UTC, like the chart's own time axis.
Hover over any square or total for its percentage of the capital. With a day open, **Day before**
and **Day after** step through the days that have trades. Changing the backtest takes the calendar
back to its months.

## Skill or luck: Monte Carlo and random blocks

A backtest is one path through one stretch of history, which can't tell skill from luck. **Test**
in the backtest panel's settings has two more ways to look at a strategy. Hover over any control
or figure in the panel to see what it does. Playback is only for **Backtest**.

**Monte Carlo** takes the trades the backtest made and draws that many at random, with
replacement, over and over (**Runs**, 1,000 at first). It needs at least 10 trades. A position
still open at the end counts as a trade at the last close. It runs on its own whenever the
backtest finishes. The faint lines are up to 200 of the runs, by trade number; the bright one is
the median of all of them. **Each line is one possible history**: not a real one, but what these
trades could have added up to in a different mix. Best is green, worst red, and the backtest itself
bright blue, so you can see where the real order sits among the draws.

- **Median** and **5–95%**: where runs ended. If the low end is well below zero, these trades can
  lose money overall, whatever the backtest happened to show.
- **DD 95%**: only one run in twenty fell further than this from a peak. Expect a drawdown of about
  this size, not just the one in the backtest.
- **Ruin**: the share of runs that fell as far as **Ruin at** from a peak (50% at first).

It assumes trades don't depend on each other, so it understates streaks that come from the market,
and it can only draw trades the backtest made.

**Random blocks** backtests random stretches of the span, each with the bars before it to warm up
on. Pick how many (**Blocks**) and how long (one week, 30 days or 91 days). It runs by itself, like
Monte Carlo, and again when anything changes; **Cancel** stops it, and **Run** starts it again.
Strategies in a process of their own wait for **Run**. Blocks can overlap. **The span slider decides what's sampled**, so widen it to reach other kinds of
market. Each line is a block, by bars into it, all from the same capital.

If it can't run, **Run** is disabled and a red line under the test's settings says why and how to
fix it. A span too short for the blocks also gets a **Widen span** button, which moves the span's
start back far enough. A chart's latest bars often cover less than a month on short timeframes, so
this is the usual reason. A number typed without pressing Enter counts once you press Run.

- **Profitable**: the share of blocks that made money. Near 50%, the strategy may have no real edge
  in this market.
- **Median**, **Best**, **Worst**: block returns, and the worst block's dates. A best far above the
  median means the result leans on a few good stretches.

Pick a Monte Carlo line, or Best, Middle, Worst or The backtest in the list, to see **that run's
trades** in the order it drew them, with the equity after each. They're your backtest's real trades
in a random mix, so one can repeat and another be left out. Click a trade to see where it really
happened on the chart, and **All runs** to go back to the list.

On both tests, the list beside the lines says what the result means and how each one went. **Click a
line or a row** to pick it out; hover over a line to see its return and worst fall. On Random blocks
this also moves the chart to that block, as it happened at a real time. **Scroll to zoom and drag to
move along**, as on a chart, and **Show it all** goes back.

Blocks use the chart's strategy and settings, but orders always fill on the chart's bars, even at
High precision, to keep a batch quick. Changing the strategy, a setting or the span stops a batch
and clears its results; so does changing Blocks or the length. A strategy in a process of its own
runs one block at a time, and **a strategy that asks AI models is refused**, as that many backtests
could cost real money.

Results aren't saved, but your choices are, with the chart. The same inputs give the same results;
**Run again** draws new random numbers.

Two related tests are already here: a [parameter sweep](#parameter-sweeps) shows how sensitive a
result is to a parameter, and [paper trading](#paper-trading-in-the-app) is a forward test on bars
the strategy has never seen.

## Optimizing a strategy

**Optimize**, under **Test** in the backtest panel, tries a range of one of the strategy's variables
or two together (including Size %). Pick each variable's **Min**, **Max** and **Step**, what to
**Score by**, and press **Run**. **Cancel** stops it: backtests already under way finish and are
discarded.

| Score by | A value is better if | Backtests |
|---|---|---|
| Backtest return | it returned more in one backtest | one per value |
| Monte Carlo | the low end of what its trades could return is higher (19 runs in 20 did better), for survival over peak profit | one per value |
| Random blocks | its median return over the same random stretches is higher, for consistency | one per value per block |

One variable draws bars. Two draw a heatmap, red below zero and green above, with a scale showing
the lowest and highest scores. **Grey means no score**: that value made no trades, too few to draw
from, or couldn't run, such as a Fast above Slow. Hover over it to see which, and the rest of the
batch carries on. Up to 5,000 backtests are allowed at once. Strategies that ask AI models are
refused, and strategies in a process of their own run one backtest at a time. Orders always fill
on the chart's bars.

**The highest score is partly luck**, and the more values you try, the more so. Two things help:

- **Top values are ranked with their neighbours**, so a good value in a good area beats a lone
  spike. A value at the edge of the range has fewer neighbours, so it can still rank well on its
  own; the panel warns when the best is at the edge, and a wider range usually settles it.
- **Keep unseen** (on by default) picks values on the first 70% of the span, then backtests the top
  5 on the last 30%, warmed up on the bars before it and starting from the full capital. A value
  that made money where it was picked but lost on the unseen bars shows in orange, and is likely
  only fit to the past.

Click a row or a bar to put that value on the strategy and see its trades on the chart; the results
stay, so you can try several, and **Back to the original** undoes it. A live session on that chart
stops, as it runs the old settings. Changing anything else clears the results.

## Parameter sweeps

To see how a parameter matters, use **Sweep a parameter…** in the report:
1. Pick the parameter.
2. Type **From**, **To** and how many **Charts** (2 to 9).
3. Click **Open … charts on workspace N**.

Persephone opens a free workspace with one chart per value, each backtesting the same strategy
on the same market. The report on any of them lists every value's return, with the chart
you're on marked. Values of whole-number parameters are rounded, and repeats are dropped.

## Drawings

**Draw ›** has three tools:
- **Trend line**: two clicks.
- **Horizontal line**: one click.
- **Rectangle**: two clicks, at opposite corners.

Esc stops drawing. Click a drawing for a button that removes it, or use **Remove all drawings** on the Draw
page. Drawings are tied to times and prices, not to screen positions. They stay put as you pan
and zoom, show on every timeframe of the market, and are saved per market.

## Your own data

### Importing ticks or bars

Click a chart's symbol and choose **Import data…** at the bottom of the dropdown, or drop CSV
files (plain, `.gz` or `.zip`) on a chart. The import panel shows what it detected:
- separator and header;
- time format and zone;
- columns: time, price, size, side; bid and ask; or open, high, low, close and volume;
- trades, quotes, or bars and how far apart they are (`1h bars`, `1d bars`, `1M bars`);

and boxes for the symbol, venue and time zone. Import streams the files to the local store,
and the chart switches to the new market when it's done. If the market already exists, the
panel says the files add to it.

- **Ticks** (trades or quotes) chart at every timeframe, can be cut into tick and volume bars,
  and backtest on the ticks.
- **Bars** of any spacing, from a minute to a month (MetaTrader, TradingView or Yahoo exports,
  Binance klines, …), chart at their own timeframe and longer ones: daily bars show 1d, 1w and
  1M, and the shorter timeframes are greyed out. A chart switched to them moves up to the
  shortest they make. Their backtests fill on the bars. Bars less than a minute apart are
  rolled up into 1-minute bars.

Imported markets stay in the store, and in the symbol search under **Local**, until you
delete them.

### Deleting an imported market

In the symbol search, click the **×** beside a market under **Local**. The row asks first and
says how much space it frees. **Delete** removes Persephone's copy from the store. The files
you imported from aren't touched, so you can import them again any time. Charts showing the
market switch to BTCUSDT.

From the command line:

```bash
persephone-cli import ticks-2024-*.csv.gz --symbol EURUSD --tz America/New_York
persephone-cli import file.csv --symbol EURUSD --dry-run          # just show the detection
persephone-cli data list                                          # what's in the store
```

**Detection:**
- **Times:** epoch seconds, milliseconds, microseconds or nanoseconds; ISO-8601 and most
  year-first dates; day- or month-first dates; separate date and time columns.
- **Numbers:** decimal commas.
- **Columns:** chosen by header names, or by their values when there are no headers (four
  columns where the second is always highest and the third lowest are a bar's open, high,
  low and close).
- **Bar spacing:** the usual gap between rows, so daily files with weekends are still daily.
- Anything it gets wrong can be overridden (`--time`, `--price`, `--open`, `--kind bars`,
  `--time-format`, …).
- `--save-profile NAME` remembers a layout and `--profile NAME` reuses it.
- Importing the same span again replaces it rather than doubling it.

Imported markets are named `SYMBOL.VENUE` (venue `LOCAL` unless you say otherwise). On the
chart, they appear under **Local** in the symbol search. `persephone-cli data remove
SYMBOL.VENUE --yes` deletes one from the command line.

### Tick and volume bars

Markets imported as ticks can also be cut into bars by activity instead of time. Open **Tick & volume
bars** from the menu and type `1000t` (a bar every 1,000 ticks) or `2.5v` (a bar each time 2.5
units trade; trades only). Each bar closes at its last tick, so strategies never act on a bar
before it's complete.

### Binance archives

For years of Binance history, download its bulk archives, which are checked and imported in
one go:

```bash
persephone-cli fetch binance BTCUSDT --from 2023-01 --to 2024-06      # monthly files
persephone-cli fetch binance BTCUSDT --from 2024-06-01 --to 2024-06-07 # daily files
```

These are aggregated trades by default, or `--all-trades` for every single trade (much
larger). Each file is checked against its published SHA-256. The market becomes
`BTCUSDT.BINANCE` in the local store.

### Command-line backtests

```bash
persephone-cli backtest --symbol BTCUSDT --timeframe 1h --days 180 --strategy ema-cross --params 12,26
persephone-cli backtest --local EURUSD.LOCAL --timeframe 5m --days 30 --fills
persephone-cli backtest --local EURUSD.LOCAL --bars 2000t --strategy donchian-breakout
```

## Paper and live trading

A strategy that backtests can also trade bars as they close, through NautilusTrader's live
node. Binance markets on time bars only.

### Paper trading, in the app

In a strategy's report, click **Paper trade on live bars**. No account or key is needed:
- Persephone fetches the latest bars to warm the strategy up, connects to Binance's public
  data streams, and waits for the next bar to close.
- Orders fill in Nautilus's simulated exchange, the same way they do in backtests, from a
  100,000 paper balance.
- The session starts flat.
- New bars are added to the chart as they close. From where the session started, the chart
  shows the session's own trades.
- The corner shows a dot and the session's profit.
- The report shows its state, profit, position and fills, and any refused order.

**Stop and close the position** ends it. Changing the strategy's parameters, the market or the
strategy also stops the session, since it was running the old settings.

### Binance testnet

[Binance's spot testnet](https://testnet.binance.vision) takes real orders with play money. Make
API keys there, then start Persephone with them in the environment:

```bash
export BINANCE_TESTNET_API_KEY=…
export BINANCE_TESTNET_API_SECRET=…
cargo run -p persephone --release
```

The report then also offers **Trade on Binance's testnet**. Spot accounts can't borrow, so on
the testnet a strategy goes flat where it would go short.

### From the command line, and with real money

```bash
persephone-cli live --symbol BTCUSDT --timeframe 15m --strategy ema-cross --balance 5000
persephone-cli live --testnet --symbol BTCUSDT --timeframe 1m
persephone-cli live --real-money --symbol BTCUSDT --timeframe 1h --balance 500
```

The terminal prints each closed bar with the position and profit, plus each fill. **Ctrl-C**
stops the session and closes the position.

`--balance` is the cash positions are sized from: each position is worth Size % of it. With
real money that's how much of your account the strategy may use; the account itself isn't
checked.

**Real money** is only in the command line, never in the app:
- It needs `BINANCE_API_KEY` and `BINANCE_API_SECRET` in the environment.
- It must be confirmed by typing a phrase at a terminal (`trade BTCUSDT with real money`).
  Anything else, or no terminal, stops it before anything is sent.
- Strategies go flat instead of short.

**Use it at your own risk.** Paper trade a strategy for a while first; backtests flatter.

Keys are only ever read by NautilusTrader from the environment. Persephone never reads,
stores or shows them.

## Plugins, models and AI

Indicators and strategies of your own load from the plugins folder and reload when the file
changes, without restarting. Plugins that run ONNX models or ask language models work the
same way, as do strategies in any language talking over a socket. See
[Writing plugins](plugins.md).

When a strategy plugin wants to ask language models, the app asks you first. The report
explains what's at stake (prompts leave your machine, and new calls can cost money) and has
**Allow it to ask AI models**. Replies are recorded, so rerunning a backtest asks nothing
new.

## Where files live

Everything is in one folder:
- macOS: `~/Library/Application Support/persephone`
- Linux: `~/.local/share/persephone`
- Windows: `%APPDATA%\persephone`

| Path | What |
|---|---|
| `layout.ron` | charts, indicators, strategies and workspaces |
| `drawings.ron` | drawings, by market |
| `store/VENUE/SYMBOL/` | imported ticks (Parquet, one file per day) and their 1-minute bars, or imported bars (one file per year) |
| `plugins/` | plugin `.wasm` files and `.external` strategy addresses |
| `models/` | ONNX models plugins can run |
| `ai.json` | how plugins reach language models (optional) |
| `ai-cache/` | recorded language-model replies |
| `ai-grants.json` | strategies you've allowed to ask models |

`persephone-cli data dir`, `plugin dir` and `models dir` print their folders. Delete
`layout.ron` to start over with one chart.
