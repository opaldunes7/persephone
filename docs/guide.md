# Persephone user guide

Persephone shows nothing but charts. Everything else is in the menu you get by clicking a
chart, in the strategy report, and in `persephone-cli`. This guide walks through each.

- [Starting](#starting)
- [Charts](#charts)
- [Tiles and workspaces](#tiles-and-workspaces)
- [Strategies and backtests](#strategies-and-backtests)
- [The backtest panel](#the-backtest-panel)
- [The calendar: replaying a day's trades](#the-calendar-replaying-a-days-trades)
- [Following any test's trades](#following-any-tests-trades)
- [Skill or luck: Monte Carlo and random blocks](#skill-or-luck-monte-carlo-and-random-blocks)
- [Building a strategy](#building-a-strategy)
- [Optimizing a strategy](#optimizing-a-strategy)
- [Parameter sweeps](#parameter-sweeps)
- [Drawings](#drawings)
- [Your own data](#your-own-data)
- [Paper and live trading](#paper-and-live-trading)
- [Plugins, models and AI](#plugins-models-and-ai)
- [Limit orders, stops and funding](#limit-orders-stops-and-funding)
- [News](#news)
- [Exporting and importing](#exporting-and-importing)
- [Persephone Lab](#persephone-lab)
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
| Ctrl+N | open or close the news sidebar |
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
- a summary in the top-right corner: return, drawdown, win rate, trades. A trade is flat to flat, so a reversal counts as two; fills are counted separately in the statistics.

Click the summary for the **report**. It has the parameters (editing one reruns the backtest),
net profit, return, max drawdown, Sharpe, Sortino, profit factor, win rate, trades, fills, time in a position and fees. It also shows the return before costs, what costs took and the break-even cost a fill, which take a second backtest with costs off that runs when you open it, unless the strategy asks AI models. What costs took is split by kind where there was any: **Slippage paid** (the set ticks and percentage), **Spread paid** (half the assumed spread, crossed by each order that took), **Impact paid** (size against the bar's volume) and **Range slip paid**, beside the fees, funding and borrowing. Below those are what the market did on its own, its Sharpe and drawdown, **Beta to the market** (how far the strategy's account moved with the market, day by day: 1 moves with it, 0 owes it nothing, below 0 moves against it), and each year: what it added as a share of the starting capital, which adds up to the return, and in brackets what it returned **on the year**, on what the account began it with, which compounds to it. A strategy that can only go wrong by rounding, running out of money or a mismatch with what it asked for shows a red `!` and says so here.
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
| Most exposure | Portfolios only. The most the book may hold, long plus short, as a percentage of the account times the leverage. Orders that would take it past are cut; what's held isn't sold to get under it unless the book is rebalanced. Off by default. |
| Intra-bar precision | **Standard** fills at the bars, and playback shows each bar as it closes: fast and light, best for long spans. **High** fills on ticks or 1-minute bars, and at slow speeds playback builds each bar as it formed, from the ticks (imported tick data), 1-minute bars (Binance, and 1-minute imports; they load in the background) or, with nothing finer, a simulated path from open through low and high to close. |
| Direction | Long only, or short only; the other side's signals close positions instead. |
| Invert | Trade the opposite of what the strategy says: short for long and long for short, at the same moments. It comes before Direction, so Long only still means only long positions. Going flat stays flat, and the panel's title says **inverted**. |
| Trading session | Weekdays, a time window and a time zone (e.g. America/New_York, daylight saving included). Outside it, signals can close positions but not open them; optionally, positions close when it ends. |

Direction, Invert and the trading session apply to paper trading too. Position sizing models, dynamic
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

## Following any test's trades

Every test can be taken down to single trades and played on a chart. The trades open in a drawer
beside the test: when each opened and closed, the market, long or short, its size, its profit after
fees, its return on what it put in, and its fees. **By profit** puts the best first. Click a trade
and playback pauses with the playhead 10 bars before it opened, the trade shaded and edged in
yellow, and every other trade of that run as markers. Press **Play** or **Step** to watch it happen,
at any speed. **✕** closes the drawer and the chart goes back to its own trades, whole.

| Test | Where its trades open |
|---|---|
| Backtest | **Trades** in the legend |
| Monte Carlo, Random blocks | a run's trade list beside the lines: click a trade |
| Optimize | click a value's row or its square; **Apply** puts it on the strategy |
| Walk-forward | click a window's row: its pick, on its unseen stretch alone |
| Random entries, shifted signals | click the returns chart: the run nearest that return |
| Portfolio | **Trades** beside the markets; a sweep's row; a walk-forward window's row |

Runs a test kept only the figures of are run again on their own, which takes a moment; their trades
add up to the figure on the row. A portfolio's trade on a market the panel's chart isn't showing
opens a chart of that market by splitting the panel's chart in half, as a new chart does, whichever
chart has focus: the new market takes the right or bottom half, and the playback then drives it.
Later trades on that market use the same chart. Moving to another test, or changing the backtest, closes the drawer.

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

Pick a Monte Carlo line, or Best, Middle, Worst or The backtest in the list, and the pane splits in
half to show **that run's trades** beside the lines, in the order it drew them, with the equity after
each. They're your backtest's real trades
in a random mix, so one can repeat and another be left out. Click a trade to see where it really
happened on the chart. The buttons above switch runs, and **All** goes back to the list.

**Table**, beside **Lines**, splits the pane in half: every run on the left, with the backtest itself
among them so you can see where it ranks, and the picked run's trades on the right. Click a run to
see its trades, and a column to sort it. Random blocks has the same, one row per block with its
dates; those trades really happened in that order, and clicking a block also moves the chart to it.

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

## Building a strategy

**Build**, under **Test**, writes a strategy as rules: no code and no files. Three sections say what
to hold: **Go long when**, **Go short when** and **Go flat when** (without it, a position is held
until the other side is asked for). Each holds **All of** or **Any of** a list of conditions, which
can nest as groups and be turned round with **not**. A condition compares two values: a number, a
bar's price or volume, what's held (entry price, bars held, highs and lows since entry), the hour or
weekday, an indicator (SMA, EMA, RSI, ATR, highest, lowest, ROC, z-score, std dev) of any value, a
value some bars ago, arithmetic on two values, or a value on a longer timeframe, seen only once that
bar has closed. **Start from…** begins again, or from one of the examples.

- **It's the chart's strategy at once.** As soon as the rules can run, the chart runs them and the
  backtest follows every change; the layout keeps them. Rules that can't run say why in red, and the
  last ones that could keep running. **Use on chart** puts them on before you've changed anything.
- **# and $:** every number has a **#** beside it. Click it and the number becomes a variable
  (**$**), named for where it is, which **Variables** lists to rename and set. Variables are the
  strategy's parameters, so **Optimize**, sweeps and **Walk-forward** can vary them, and applying an
  optimized value makes it the variable's own.
- **Raw JSON** shows the same tree as JSON. Paste one in, from Lab, an agent or a script, with or
  without its `{"logic": …}` wrapper, and **Apply** fills in the rules; one with a mistake says
  where, and nothing changes. **Copy JSON** copies it out.
- **Save as strategy…** saves it by name, with the chart's settings, where saved strategies live:
  it's then in every chart's strategy list and portfolio legs' list, marked *saved*, and Lab and
  agents can read and run it. Saving again under a name makes a new one, newest first.

## Optimizing a strategy

**Walk-forward**, under **Test**, is Optimize done fairly. It picks the best values on a stretch, tests them on the unseen stretch after it, slides along and repeats, for as many **Windows** as you set. The heading's **Unseen** is what those stretches returned added up, beside what the **Market** did. It's the figure to trust, as none of it was used to pick anything. The chart joins those stretches end to end beside the market's held in full, marks each window and shades those that lost. The table beside it shows what each window picked and how that did; hover the chart for the same. Its **Deflated** column is the chance the pick's Sharpe ratio where it was picked was more than luck, for having been the best of every value tried, and the line under the table gives the unseen stretches' own Sharpe ratio, which needs no deflating. Click a window's row to see its pick's trades on its unseen stretch.

**Random entries**, under **Test**, compares a backtest with chance. It places the same trades at random times, holding each as long, as many **Runs** as you set. **As good** is the share of placings that returned at least as much: near 50% means the entries add nothing. The charts show how the placings' returns and worst falls were spread, with the backtest's own marked in yellow, the median dashed and the middle 19 in 20 shaded.

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

Under the results, **Best's Sharpe … Deflated for N tried** puts a number on the luck: the deflated
Sharpe ratio, the chance the best's true Sharpe ratio is above nothing, allowing for how many values
were tried, how much their Sharpe ratios differed, and how far its daily returns were from normal.
Under 95% is weak evidence. It counts only this grid, so ideas you tried before aren't in it.

Click a row or a bar to see that value's trades beside the results, and follow one on the chart
(see [Following any test's trades](#following-any-tests-trades)). **Apply** on a row puts that value
on the strategy; the results stay, so you can try several, and **Back to the original** undoes it. A live session on that chart
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

## Limit orders, stops and funding

The panel's stock settings go beyond market orders:

- **Limit orders** rest a set percentage better than the close, for a set number of bars. They fill only when price trades through
  them, at their own price, and pay the maker's fee. An entry that doesn't fill lapses; an exit goes at the market. **Through** requires the
  market to trade that much further first, standing in for the queue ahead of an order, which a bar can't show. Try a few values: small profits
  often vanish. The **Limits** view beside Equity and Calendar lists every order, and compares how far the market moved after those that
  filled against those that missed. Those that miss are often the ones that would have won.
- **Stop loss** and **Target** are real orders from the bar after an entry. On a bar reaching both, the stop's assumed to have come first.
- **Fees** have an **m/t** choice, for separate maker and taker rates. A maker's can be below zero, for a rebate.
- **Impact** makes market orders fill worse the larger they are against their bar's volume. It's a rough assumption.
- **Volume limit** stops an order taking more than that share of a bar's volume. What's left goes out at the next closes, at their prices, so big positions take time to build and to leave. A resting limit fills no more than that share of each bar, and a stop takes that share and leaves the rest to the closes after.
- **Range slip** makes market orders and stops pay a share of a bar's high to low, times how many times wider it is than usual. A bar 10 times the usual width costs 100 times a usual one, so wild markets cost what they should. It's a rough assumption.
- **Trailing stop** follows the best price since entry, moved at each close, so it only tightens. **Decision log**, when on, adds a **Decisions** table to the Backtest tab: for each bar, what the strategy asked for, what was held, what went out, and why not.
- **No costs**, beside Stock settings, takes off everything there is to pay, including a market's own fees and funding, which turning Stock off leaves on.
- **Warn at** says how large an order against its bar, and how few limits filling, gets a warning. **Warm-up** is how many bars before the span strategies warm up on: raise it for longer look-backs. **Cash interest** charges a portfolio for cash below zero, a year's rate. Optimize's **Pick on %** and Walk-forward's **Ratio** set how a span is split into what's picked on and what's unseen. Range slip's **over** and Impact's **power** set how far back a usual bar is judged from and how steeply impact grows.
- **Jitter** makes each order take a random time longer than the latency. An order sent at a close reaches the next bar anyway, so only fills on ticks can show it.
- **Borrow a year** charges shorts on spot, which otherwise cost nothing to hold, unlike in reality.

For perpetuals, type a symbol like `BTCUSDT-PERP`. It uses Binance's USD-M futures bars and fees, and the statistics show **Funding paid**.

Paper and live trading stay on market orders and spot markets for now, and won't start with any of this on.

## Portfolios: several markets on one account

**Portfolio**, under **Test**, runs several markets together on this chart's timeframe and span. They share one cash balance and one margin, so a profit on one supports another.

**Add market** for each, as `BTCUSDT`, or `BTCUSDT-PERP` for a perpetual, with its weight: 0.5 holds half the capital in it, and -0.5 half short. Under **Trades**, each can hold its weight or trade a strategy of its own, with **Chart's** making those that hold trade this chart's strategy instead. **Venue** is for a market you imported, blank meaning where this chart's bars come from. Or use **Load weights** for a JSON file of `[[time, {"BTCUSDT": 0.5, "ETHUSDT": -0.5}], ...]`, which states the whole book at each time and is how to run pairs or rotation. **Every** and **Drift** go back to the weights every so many bars or when a leg strays, and **Min** skips trades too small to be worth their fees. Costs come from the settings above.

The heading has the return, before costs, equal shares, Sharpe, **Beta** (how far the account moved with equal shares of its markets, day by day), the deepest fall and margin. The results show the account, equal shares of each market held, and what each market has made, with gross and net exposure below. Under the tables, what costs took is split into fees, slippage, spread, range, impact, funding, borrowing and interest, and each year shows what it added and what it returned on the year. **Trades**, before each market's fills, lists every trade across the markets to follow on a chart. The tables have each market's part, how large orders were worked (**To go** being what never got out, **Gaps** bars it was missing), how the markets' own returns moved together, returns by year, and each market's fills, a click on its name away.

Every run is kept where Lab keeps its runs. **Saved runs** goes back to one, including those from Lab, and Lab reads the app's with `get_portfolio`. They travel in **Export all** with **Runs** on.

- A market with no bar at some moment is valued at its last close, and can't trade until it has one.
- **Margin %** is what positions need to stay open. When the account falls to it, judged at each market's worst in the bar, everything is sold at that close, whatever the volume limit, and trading stops. **Closest** says how near it came.
- Exchanges' discounts for hedged books aren't modelled, so a hedge gets no margin relief.
- Portfolios use market orders, at the closes they decide on, or at the next opens with **Fill at next open** on, which is the fairer test of a weight table. Limit orders and stops are for single markets for now, and paper and live trading don't run them.

The chips over the settings ask whether a result is luck:

- **Sweep** varies one thing, such as a weight, rebalancing or a cost, from **Min** to **Max** by **Step**, into a table with the best first. **Deflated** on the best row is its Sharpe ratio deflated for having been picked from the rest. Click a row to see that setup's trades.
- **Shifted** moves the last run's book along the span by random amounts, wrapping round. It holds the same things for as long, and only the timing goes. **As good** near 50% means the timing added nothing. **Random entries** has the same test for one market, under **Shifted**.
- **Monte Carlo** draws the last run's returns again in blocks of **Block** bars, keeping how the markets moved together, and shows the band 19 in 20 stayed inside.
- **Walk-forward** picks the best of the sweep's values on each stretch, trades it on the unseen one after, and slides along. **Windows** and **Ratio** are Walk-forward's own, and Run shows the same chart and table.

## News

**News** in a chart's right-click menu, or **Ctrl+N**, opens a narrow sidebar of headlines down the
window's right edge, under the backtest panel. The charts make room for it, and its left edge drags
it wider or narrower (220 to 520 pixels, always leaving the charts 400). It's saved with the layout.

- **Where they come from:** public RSS and Atom feeds that need no key: CoinDesk, Cointelegraph,
  Decrypt, and the Federal Reserve's and the SEC's press releases. List your own instead, one URL a
  line, in `news/feeds.txt` in the data folder. They're read every 5 minutes, at once when the
  sidebar first opens, and on **⟳**. Feeds that couldn't be read are named in red.
- **Live and replay:** with no backtest playing back, it lists the latest first. While one plays
  back, it lists only what had been published by the playhead, as of the time in its heading, and
  moves with it, so it never gives away what came next.
- **Filters:** **All**, the focused chart's market (headlines naming it, as BTC or Bitcoin), and
  **Macro** (the Fed, the SEC, and rates, inflation, hacks, failures and the like). The bar beside
  each headline says how much it may matter: red for market-moving, amber for naming this market,
  grey otherwise. It's a rough rule on the words, not a judgement.
- **Clicking a headline** takes the playhead there in a replay, or the focused chart otherwise.
  **↗** opens it in the browser.
- **On the charts:** while the sidebar's open, each chart marks the headlines that matter to it with
  a small diamond along the bottom of its price pane, on the bar each came out in, and none past a
  playhead. Hover one for its title; click it to find it in the sidebar.
- **History:** every headline is kept in `news/headlines.jsonl` in the data folder, so a history
  builds up. Feeds only carry their last few dozen items, so there's nothing from before you started:
  a replay of an older span shows none, and says so. **Keep collecting** reads the feeds while the
  sidebar's closed too, so there are no gaps. It turns on the first time you open the sidebar, and
  stays as you set it; nothing is fetched until then.
- **Times** are when each headline's publisher put it out, which can be after the news first broke.
  Good for seeing what was being said around a bar, not for trading on to the second.

## Exporting and importing

The backtest panel's bar has **Export chart**, **Export all** and **Import**, all as JSON files.

- **Export chart** saves this chart's settings, strategy and values. Importing it puts them on the chart you're on.
- **Export all** saves your saved results, layout, workspaces, drawings and which plugins are in use. Turn on **Runs** to add
  every full run, which can be large: gigabytes, after an agent has been researching for a while.
- **Import** adds results and drawings beside yours. If the file has a layout it asks first, and keeps yours as
  `layout.before-import.ron` beside it. It says if the file names a plugin you don't have.

A file can't carry your API keys, what AI models a plugin may use, plugins' code or market data. Lab reads and writes the
same files.

## Persephone Lab

Agents and scripts can read every data point and run every test through
[Persephone Lab](lab.md), without the app open. Strategies they save appear in the backtest panel's
strategy list, marked **Lab**, ready to backtest and replay. Everything the backtest panel shows,
including the data behind its charts, is available to them as well.

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
