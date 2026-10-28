# Persephone

A backtesting app with no interface but its charts. It opens to one TradingView-style chart;
a right click adds indicators and strategies, and a key splits the window into more charts,
tiled the way Hyprland tiles windows. Every backtest runs on
[NautilusTrader](https://github.com/nautechsystems/nautilus_trader)'s Rust engine, and the same
strategies can paper trade on live Binance bars.

![Two tiled charts: EMA and Bollinger bands with an EMA Cross backtest and its equity, and RSI](docs/images/persephone.png)

- **Charts** drawn on the GPU: candles, volume, built-in indicators (SMA, EMA, Bollinger, RSI,
  MACD, Stochastic, ATR, Donchian), drawings, and a crosshair synced across charts.
- **Tiling**, fixed like Hyprland's dwindle layout: up to 16 charts per workspace, nine
  workspaces, all saved.
- **Backtests** on Nautilus, with trade markers, an equity pane, a full report, and parameter
  sweeps compared side by side. A backtest panel picks the span from the market's whole
  history, plays the run back bar by bar at any speed from real time up, and sets costs,
  slippage, latency, leverage, direction, trading sessions and intra-bar precision.
- **Your own indicators and strategies** as sandboxed WebAssembly plugins (Rust SDK), which
  can run ONNX models and ask language models, or as any process (Python, …) reached over a
  socket.
- **Data:** Binance spot, your own CSV files of ticks (trades or quotes) or bars (a minute to
  a month apart), in almost any layout, imported from the symbol search or by dropping them on
  a chart, and Binance's bulk archives. Tick data backtests fill on ticks.
- **Paper and live trading** of the same strategies through Nautilus's live node.

## Run it

You need [Rust](https://rustup.rs). The repository pins the toolchain, so `rustup` installs
the right version on first build.

```bash
cargo run -p persephone --release
```

The first build takes several minutes. The app opens a Binance BTCUSDT 1-minute chart, and
your last layout on later launches. To open one particular market without touching the saved
layout:

```bash
cargo run -p persephone --release -- ETHUSDT 15m
```

The command-line tools (backtests, imports, plugins, live trading) are in `persephone-cli`:

```bash
cargo run -p persephone-cli --release -- --help
```

## Keys

Every command is Control plus a key (⌃ on a Mac). Keys follow physical positions, so they work
on any keyboard layout.

| Keys | Action |
|---|---|
| Ctrl+Enter | new chart |
| Ctrl+Q | close chart |
| Ctrl+H/J/K/L or arrows | move focus |
| Ctrl+Shift+H/J/K/L | swap charts |
| Ctrl+Alt+H/J/K/L (Option on a Mac) | move the divider |
| Ctrl+T | toggle split direction |
| Ctrl+F | fullscreen |
| Ctrl+1…9 | switch workspace |
| Ctrl+Shift+1…9 | move chart to workspace |
| Esc | close a menu, stop drawing |

New chart, toggle split and close are also buttons in each chart's bottom-right corner
(**+**, **◫**, **✕**); hovering one shows its shortcut.

On a chart: right-click for the menu. Click the symbol in the top-left header to search for
another market right there, or the timeframe for a dropdown of the others. The round tags
after the name are volume and the chart's indicators: click one to hide or show it. Drag to pan and scroll to zoom. Drag the price axis to stretch prices
and the time axis to stretch time; double-click either to reset it.

## Documentation

- [User guide](docs/guide.md): charts, strategies and reports, sweeps, drawings, your own
  data, paper and live trading, where files live.
- [Writing plugins](docs/plugins.md): indicators and strategies in Rust, models and AI,
  external strategies in Python or anything else.
- [Design plan](docs/plan.md): architecture and the decisions behind it.

## Project layout

| Crate | What it does |
|---|---|
| `crates/app` | the app (`persephone`) |
| `crates/cli` | command-line tools (`persephone-cli`) |
| `crates/chart` | the GPU chart widget |
| `crates/tiling` | the dwindle layout |
| `crates/engine` | the only crate that uses Nautilus: backtests and live sessions |
| `crates/plugin-host` | runs WebAssembly plugins and external strategies |
| `crates/sdk` | the plugin SDK |
| `crates/data` | Binance data, the local tick and bar store, the CSV importer |
| `crates/ml` | ONNX models and language models for plugins |
| `crates/core` | shared types |
| `plugins/` | example plugins (a workspace of its own, built for `wasm32-wasip2`) |

`cargo test --workspace` runs the tests. Some build the example plugins first, so the first
run takes a while.

## License

Not chosen yet. Persephone builds on NautilusTrader, which is LGPL-3.0.
