# Persephone user guide

Persephone shows nothing but charts. Everything else is in the menu you get by clicking a
chart, in the strategy report, and in `persephone-cli`. This guide walks through each.

- [Starting](#starting)
- [Charts](#charts)
- [Tiles and workspaces](#tiles-and-workspaces)
- [Strategies and backtests](#strategies-and-backtests)
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

- **Click** (without dragging) opens the menu:
  - **Indicators ›**: the built-ins, then plugin indicators.
  - **Strategy ›**: the strategy to backtest on this chart.
  - **Draw ›**: drawing tools.
  - **Symbol**: search Binance pairs and your imported markets. Enter opens any Binance spot
    pair you type.
  - **Timeframe**: 1m, 5m, 15m, 30m, 1h, 4h or 1d.
  - **Tick & volume bars**: shown only for imported markets.
- **Drag** pans, **scroll** zooms, and **dragging the price axis** stretches prices.
  Double-click the price axis to fit prices again.
- **Indicator legends** (top left) open that indicator's settings when clicked. Values check
  as you type, and Remove is at the bottom. Overlays (moving averages, bands) draw on the
  prices; oscillators (RSI, MACD, …) get a pane of their own below.
- **Hovering** a bar shows a crosshair on every other chart at the same moment, whatever their
  timeframe.

## Tiles and workspaces

The window is tiled like Hyprland's dwindle layout. A new chart splits the focused one along
its longer side, and closing a chart gives its space to its neighbour. Focus follows the
mouse.

| Keys (Mod = ⌥ Option on macOS, Alt elsewhere) | Action |
|---|---|
| Mod+Enter | new chart, showing the focused chart's market |
| Mod+Q | close the focused chart |
| Mod+H/J/K/L or arrows | move focus left/down/up/right |
| Mod+Shift+H/J/K/L | swap with the neighbour in that direction |
| Mod+Ctrl+H/J/K/L | move the divider |
| Mod+T | toggle the split direction |
| Mod+F | fullscreen the focused chart (again to restore) |
| Mod+1…9 | switch to that workspace |
| Mod+Shift+1…9 | move the focused chart to that workspace |

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

Every strategy also has **Size %**: each position is worth that share of the 100,000
starting balance (in the quote currency, e.g. USDT). Plugin and external strategies are
listed below the built-ins.

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
- On imported tick data, orders fill on the ticks that follow instead (at the ask or bid for
  quotes, at the trade price for trades). The report says which.
- Changing the timeframe, symbol or parameters reruns the backtest.

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

### CSV tick files

Drop CSV files (plain, `.gz` or `.zip`) on a chart. The import panel shows what it detected:
- separator and header;
- time format and zone;
- columns: time, price, size, side, or bid and ask;
- trades or quotes;

and boxes for the symbol, venue and time zone. Import streams the
file to the local store, and the chart switches to the new market when it's done.

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
- **Columns:** chosen by header names, or by their values when there are no headers.
- Anything it gets wrong can be overridden (`--time`, `--price`, `--kind`, `--time-format`,
  …).
- `--save-profile NAME` remembers a layout and `--profile NAME` reuses it.
- Importing the same span again replaces it rather than doubling it.

Imported markets are named `SYMBOL.VENUE` (venue `LOCAL` unless you say otherwise). On the
chart, they appear under **Symbol › Local**.

### Tick and volume bars

Imported markets can also be cut into bars by activity instead of time. Open **Tick & volume
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
| `store/VENUE/SYMBOL/` | imported ticks (Parquet, one file per day) and their 1-minute bars |
| `plugins/` | plugin `.wasm` files and `.external` strategy addresses |
| `models/` | ONNX models plugins can run |
| `ai.json` | how plugins reach language models (optional) |
| `ai-cache/` | recorded language-model replies |
| `ai-grants.json` | strategies you've allowed to ask models |

`persephone-cli data dir`, `plugin dir` and `models dir` print their folders. Delete
`layout.ron` to start over with one chart.
