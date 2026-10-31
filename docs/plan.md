# Persephone: a tiled, chart-only backtesting app in Rust

## Context

`~/dev/persephone` is empty, so this starts from scratch. The goal is a backtesting app with almost no interface:

- It opens to a single full-window chart that looks like TradingView.
- You left-click to add things: indicators, strategies, a symbol, a timeframe.
- A keyboard shortcut splits the window into more charts, the way Hyprland's "dwindle" layout tiles windows.
- Every backtest runs on **NautilusTrader through its Rust crates, with no Python**.
- Users can write any indicator or strategy they like, including ones that use ML models or AI APIs.
- Two things are fixed and not user-configurable: the Nautilus engine and the Hyprland-style tiling.

Decisions you made:
- **Charts:** our own native GPU chart, styled like TradingView.
- **Plugins:** WebAssembly (WASM), written in Rust first.
- **Data:** crypto first, plus an importer for your old CSV tick files.
- **License:** undecided, so the codebase stays free of GPL code.

## Verdict on your stack

| Idea | Verdict | Notes |
|---|---|---|
| Rust | ✅ | Good fit. One binary for the GPU, the engine and the plugin sandbox. |
| Nautilus in pure Rust | ✅ with caveats | v2 lets you write `Strategy`/`DataActor` and run `BacktestEngine` in Rust with no Python. **But:** the crates are at 0.66 and v2 is still a release candidate, so the API changes between versions. Tearsheets are Python-only, so we build our own stats view on `nautilus-analysis`. It needs **Rust ≥ 1.99**; you have 1.97.1, so run `rustup update`. |
| Hyprland | ⚠️ reinterpreted | Hyprland is a Linux compositor and can't run on macOS. We rebuild its dwindle layout and animations **inside the app**, which also makes it work on every OS. |
| TradingView chart | ⚠️ replaced | TradingView's charts are JavaScript and need a webview. We draw our own chart on the GPU with the same look. All tiles share one GPU device, so the cap is a budget constant (16), not a hard GPU limit. |
| Customizable algorithms, ML and AI | ✅ via WASM | Plugins reload live and are sandboxed and reproducible. The app runs ML inference and LLM calls and offers them to plugins. |

**Improvements I'm adding:**
1. **Bar-index x-axis**, like TradingView, so gaps in old CSV data don't leave holes in the chart.
2. **Click vs drag:** a left-click without movement opens the add menu, and a drag pans. This keeps your left-click idea without breaking panning.
3. **Record and replay for LLM calls:** responses are cached so backtests are reproducible and cheap to rerun.
4. **A look-ahead warning for LLMs.** A model trained after your backtest period already "knows" what happened, so LLM-strategy results over that period are inflated. Score them only on data after the model's training cutoff.
5. **Parameter-sweep grid** (later): the same strategy in N tiles with different parameters, compared side by side.

## Architecture

```
┌──────────────────────── persephone (iced 0.14 app, UI thread) ───────────────────────┐
│  keymap (fixed) ─► tiling (dwindle BSP) ─► tile rects (animated)                       │
│  each tile = ChartView: Shader widget (wgpu candles/series) + Canvas overlay (axes/text)│
│  left-click menu ─► add indicator / strategy / symbol / timeframe                      │
└──────────────┬─────────────────────────────────────────────────────▲─────────────────┘
               │ BacktestRequest / IndicatorRequest (channels)        │ events + reports
┌──────────────▼─────────────────────────────────────────────────────┴─────────────────┐
│ engine (ONLY crate that depends on Nautilus) — one OS thread per backtest              │
│   BacktestEngine + SimulatedVenue + WasmStrategy adapter (DataActor + Strategy)        │
│   built-in indicators via nautilus-indicators; stats via nautilus-analysis             │
└──────────────┬───────────────────────────────────────────────────────────────────────┘
               │ Persephone types only (Bar, Tick, Action, Fill)
┌──────────────▼─────────────┐   ┌──────────────────────┐   ┌──────────────────────────┐
│ plugin-host (wasmtime)      │◄─►│ ml (ort / LLM + cache)│   │ data (Parquet catalog,    │
│ hot reload, fuel, caps      │   └──────────────────────┘   │ Binance/Bybit, CSV ticks) │
└────────────────────────────┘                               └──────────────────────────┘
```

Nautilus's Rust core is single-threaded: it uses `Rc`/`RefCell` internally. So each backtest owns one dedicated OS thread, and its wasmtime `Store` lives on that same thread. The UI and the workers talk only over channels (`crossbeam-channel`, bridged into an iced `Subscription`).

## Workspace layout

```
persephone/
├─ Cargo.toml              workspace; all nautilus-* pinned to "=0.66.0"
├─ rust-toolchain.toml     stable ≥ 1.99, target wasm32-wasip2
├─ wit/persephone.wit      the plugin contract (indicator + strategy worlds)
├─ crates/
│  ├─ app/          bin `persephone`: iced app, fixed keymap, add menu, stats overlay, layout persistence
│  ├─ tiling/       pure logic: dwindle BSP tree → rects, focus/swap/resize/toggle-split, MAX_TILES=16
│  ├─ chart/        viewport math, wgpu pipelines, iced Shader + Canvas widgets, LOD, theme
│  ├─ engine/       Nautilus wrapper: backtest workers, WasmStrategy adapter, reports
│  ├─ plugin-host/  wasmtime component host, file watcher, capabilities, Wasm{Indicator,Strategy}
│  ├─ sdk/          `persephone-sdk`, the plugin authors' crate: traits + export_indicator!/export_strategy!
│  ├─ data/         ParquetDataCatalog access, exchange fetchers, CSV importer, tick→bar aggregation
│  ├─ ml/           ONNX inference (`ort`), LLM clients, record/replay cache
│  └─ cli/          `persephone-cli`: fetch, import csv, plugin new/build, headless backtest
└─ plugins/examples/ sma (indicator), ema-cross (strategy)
```

**Key dependencies:** `iced` 0.14 (wgpu, canvas, advanced), `wasmtime` (component model), `wit-bindgen` (in the SDK), `notify`, `crossbeam-channel`, `reqwest`, `csv`, `ort`, `serde` + `ron` (layout file), `dirs`, `tracing`, `anyhow`/`thiserror`, `proptest`, `criterion`.

## Design details

### 1. Tiling (`crates/tiling`), fixed and not configurable
- A binary tree in which each node is either a split (axis, ratio) or a leaf (a tile id).
- **Dwindle rule:** a new tile splits the focused tile along its *longer* side, as Hyprland does. *(Built:)* if the halves would be smaller than 320×200, the largest tile splits instead, so 16 charts end as a 4×4 grid rather than a spiral of slivers. New tiles copy the focused tile's market.
- Operations: insert, close (the sibling takes the freed space), focus by direction (nearest tile whose center lies in that direction), swap, resize the ratio, toggle the split axis, fullscreen.
- **Cap:** `MAX_TILES = 16`. Past that, the insert is refused and the tile border flashes.
- Moving or resizing tiles animates them with an eased transition from the old rectangle to the new one (about 150 ms), using iced 0.14's animation support. This is a custom layout widget, not iced's `PaneGrid`. *(Built:)* 180 ms ease-out, new tiles pop in from 88% size; tiles are absolutely positioned with `pin` inside a `stack`. A lone chart fills the window with no gap or border; focus follows the mouse, as on Hyprland.
- The layout is saved to `~/Library/Application Support/persephone/layout.ron` and restored at launch. *(Built:)* saved after every structural change; launching with `persephone SYMBOL TIMEFRAME` starts a one-off single chart that isn't saved.

**Keymap** (every command is Control plus a key, changed from Option on request; ⌘ is avoided because macOS uses it):

| Keys | Action |
|---|---|
| Ctrl+Enter | new chart (dwindle split) |
| Ctrl+Q | close tile |
| Ctrl+H/J/K/L or arrows | move focus |
| Ctrl+Shift+H/J/K/L | swap tile |
| Ctrl+Alt+H/J/K/L | resize |
| Ctrl+T | toggle split axis |
| Ctrl+F | fullscreen tile |
| Ctrl+1..9 | switch workspace (Ctrl+Shift+1..9 moves the focused chart there) |

### 2. Chart (`crates/chart`)
- **Viewport:** the x-axis counts bars, not clock time. Pan and zoom only change a uniform (offset and scale), so moving around never re-uploads data to the GPU.
- **wgpu pipelines**, inside iced's `Shader` widget, sharing iced's device:
  - candles and OHLC bars as instanced quads (body and wick)
  - lines as screen-space thick segments
  - histograms (volume, MACD)
  - band fills (Bollinger)
  - trade markers
- **LOD:** past about 2 bars per pixel, bars are merged into one per pixel column, from a cached lower-resolution copy.
- **Overlay:** an iced `Canvas` stacked on top draws the price and time axes, crosshair, last-price label, indicator legends and the stats overlay. Text goes on this layer, which keeps the GPU pipelines text-free.
- **Panes:** the main price pane, plus optional sub-panes for oscillators and the equity curve, split vertically inside the tile.
- **Fixed dark theme** modeled on TradingView's.

### 3. Interaction (`crates/app`)
- **Left-click** (press and release, under 4 px of movement) on the chart opens a small popover menu at the cursor: **Indicator ▸ / Strategy ▸ / Symbol ▸ / Timeframe ▸**. The lists include the built-in items and every plugin that's loaded.
- Clicking an indicator or strategy legend opens its parameter popover, with a remove option.
- *(Built in M3:)* the menu has drill-down pages: root (Indicators ›, Symbol ›, and one-click timeframe chips), indicators grouped "On the chart" / "In a pane below", and a symbol search (popular pairs; Enter loads any Binance spot pair). Strategy ▸ joins in M4. Escape, another click or any tiling key closes it. A tile keeps its indicators across symbol/timeframe changes, they're saved in the layout, and a new tile copies the market but starts without indicators.
- Drag pans, scroll zooms, and dragging the price axis scales price. Nothing else is on screen.
- **Stats overlay:** after a backtest, a compact line appears in the corner (PnL, Sharpe, max drawdown, win rate, trade count). Clicking it expands the full report inside the tile.
- *(Built in M4:)* the menu's **Strategy ›** page offers EMA Cross, RSI Reversion and Donchian Breakout (one strategy per chart). Adding or editing one backtests it on a worker thread against the chart's bars (100,000 starting balance, Binance spot fees, positions sized as a % of the starting balance); a generation counter drops results from superseded runs. The chart gets arrows with Long/Short/Close tags at each fill, a green/red tint while a position is open, and an Equity pane with the starting balance as a dashed level. The summary pill (return, drawdown, win rate, trades; just the return in narrow charts) opens a report popover with editable parameters, the full statistics and a remove button. Strategies are saved with the layout and rerun when bars load or the symbol/timeframe changes.

### 4. Nautilus bridge (`crates/engine`)
- This is the only crate that imports `nautilus-*`. Everything it exposes uses Persephone's own types (`BacktestRequest`, `BacktestEvent`, `BacktestReport`), so Nautilus version changes stay inside this crate.
- **Setup:** `BacktestEngine` with a simulated venue per run (cash or margin account, exchange-specific maker/taker fees, L1 book for bars, L2 when tick data is available). The instrument comes from the catalog, and data comes from `ParquetDataCatalog`. Long ranges go through `BacktestNode` streaming (`streaming` feature).
- **`WasmStrategy`** is a Rust struct with a `StrategyCore`, wired up with `nautilus_strategy!` and implementing `DataActor` + `Strategy`:
  - `on_bar`, `on_trade_tick` and `on_quote_tick` forward the event to the guest.
  - The guest returns `Vec<Action>` (market, limit, stop, bracket, cancel, close).
  - The adapter turns each action into `self.order().market(..)` or similar plus `submit_order`.
  - Order events (`on_order_filled` and the rest) go back to the guest.
- **Worker output:** it streams fills, position changes and equity samples to the UI while running, then sends a final report computed with `nautilus-analysis` (`PortfolioStatistics`).
- **Built-in indicators** come from `nautilus-indicators` (SMA, EMA, RSI, MACD, Bollinger, ATR, VWAP, Keltner, Donchian and more), shown in the menu next to plugins. *(Built in M3:)* SMA, EMA, Bollinger Bands, Donchian Channels, RSI (Wilder), MACD, Stochastic and ATR, computed by `persephone_engine::indicators` from raw values so the chart shows exactly what strategies see; the catalog (params, limits, panes, levels) lives in `persephone_core::indicator`. Overlays draw on the price pane and widen its fit; the rest get their own panes with their own scales, drawn by the same GPU pipeline (anti-aliased lines, band fills, histograms) clipped per pane.
- **Starting point:** `crates/backtest/examples/engine_ema_cross.rs` in the Nautilus repo is the reference for the first headless run.
- *(Built in M4:)* strategies are `Signal`s (closed bar in, wanted position out); one Nautilus strategy, `SignalStrategy`, turns them into market orders, so adding a strategy never touches Nautilus — M5's WASM strategies will plug into the same trait. The report's equity curve and max drawdown are marked to market from the fills (and agree with Nautilus's realized PnL in tests); Nautilus still supplies Sharpe, Sortino, profit factor and win rate.

- *(Built in M6:)* tick-level backtests. A `BacktestRequest` can carry a `TickFeed`. The strategy still decides on the chart's bars, but the venue fills from ticks (`bar_execution` off): at the ask and bid for quotes, at the latest trade for trades, on the first tick after the bar closes. Ticks stream through Nautilus's streaming mode a day at a time (quiet days bundled to about a million ticks), each batch with the bars that close inside it, so memory stays flat however long the history; a release build replays about 1.3M ticks a second. Every tick and bar gets a deep book, because a trade's size or a bar's volume isn't book depth and Nautilus would otherwise split fills and slip the remainder a tick. Currency codes Nautilus doesn't know are registered on first use. In the app, a local chart fills on ticks up to 10M of them (about 8 s) and on bars beyond that, and the report says which.

### 5. Plugins (`wit/`, `crates/plugin-host`, `crates/sdk`)

The plugin contract (`persephone:plugin@0.1.0`), roughly:

```wit
interface types {
  record bar   { ts: u64, open: f64, high: f64, low: f64, close: f64, volume: f64 }
  record tick  { ts: u64, price: f64, size: f64, side: side }
  variant param-value { int(s64), float(f64), boolean(bool), text(string) }
  record plot  { name: string, kind: plot-kind, pane: pane-target, color: u32 }
  variant action { market(order-req), limit(order-req), stop(order-req), cancel(string), close-all }
}
world indicator {
  export describe: func() -> indicator-meta;          // name, params, plots
  export init:     func(params: list<tuple<string, param-value>>);
  export compute:  func(bars: list<bar>) -> list<list<f64>>;  // batch for charts; NaN = gap
  export update:   func(bar: bar) -> list<f64>;               // incremental
}
world strategy {
  import ml; import ai; import indicators; import log;        // capability-gated
  export describe: func() -> strategy-meta;
  export on-start: func(params: list<tuple<string, param-value>>);
  export on-bar:   func(bar: bar, pos: position-view) -> list<action>;
  export on-tick:  func(t: tick, pos: position-view) -> list<action>;
  export on-fill:  func(f: fill) -> list<action>;
}
```

- **Writing a plugin (SDK):** the author writes a plain Rust struct that implements `Indicator` or `Strategy`, then calls `export_indicator!(MyInd)`. A build with `cargo build --target wasm32-wasip2 --release` produces a component.
- **CLI:** `persephone-cli plugin new <name> --kind indicator|strategy` creates a new plugin crate, and `plugin build` compiles it.
- **Hot reload:** `notify` watches `…/persephone/plugins/`.
  - When a `.wasm` file changes, the host compiles it (with wasmtime's on-disk cache), swaps the instance, and recomputes any chart using it.
  - When a strategy changes, its backtests rerun.
  - So saving a file updates the chart without restarting the app.
  - A later dev mode watches plugin source folders and runs the build itself.
- **Safety:**
  - Epoch interruption or fuel stops infinite loops.
  - `ResourceLimiter` caps memory.
  - A trap shows an error badge on the tile instead of crashing the app.
  - Plugins get no filesystem or network access by default.
  - `ml`, `ai` and `net` are capabilities a plugin declares in its manifest, and the user grants them on first load.
- **Determinism:** plugins have no wall clock and no unseeded randomness, so the same inputs always give the same backtest.
- **Other languages:** Python (componentize-py), JS and Go can target the same WIT later. The Rust SDK is the one we support first.
- *(Built in M5:)* the shipped contract is smaller than the sketch above, matching what M4's strategies need: parameters are plain numbers (`param` with default, min, max and an integer flag), an indicator exports `describe` plus a batch `compute(params, bars) -> result<list<list<f64>>, string>`, and a strategy exports `describe`, `start(params) -> result<_, string>` and `on-bar(bar) -> option<s8>` (wanted position: 1 long, -1 short, 0 flat, none = keep). Orders, ticks, fills and the `ml`/`ai` imports come with M6–M7. Strategy plugins plug into M4's `Signal` trait (`persephone_core::signal`), so they backtest through the same Nautilus `SignalStrategy` as the built-ins; the host appends the "Size %" parameter itself.
  - **SDK** (`persephone-sdk`, features `indicator`/`strategy`): `Indicator` and `Strategy` traits, builders for their descriptions, `export_indicator!`/`export_strategy!`, and a small `ta` module (`sma`, `ema`, incremental `Ema`) that matches Nautilus's numbers exactly. Examples in `plugins/examples`: `sma`, `zscore` (a pane with ±2 levels) and `ema-cross`.
  - **Host** (`crates/plugin-host`, wasmtime 50 + WASI p2): every call runs in a fresh sandbox with no files or network, a fuel budget (a spinning plugin is stopped within seconds) and a 256 MB memory cap; a panic comes back as its message, read from the plugin's stderr. The clock plugins see is stopped at the Unix epoch and their randomness is seeded, so the same inputs always give the same backtest (the `clock` fixture checks this). Sleeping isn't metered by fuel, so a plugin that sleeps holds up its own chart or backtest for as long as it asks. Compiled plugins are cached in memory by file and modification time.
  - **App:** the menu's Indicators and Strategy pages gain a Plugins section (with files that failed to load and their reasons, plus the plugins folder). A plugin indicator that fails shows its error in the legend; a strategy that fails says "couldn't run" and its report has the message. `notify` watches the plugins folder; 300 ms after a `.wasm` changes, plugins reload, charts using them recompute and their backtests rerun. Saved layouts store plugins as `plugin:<id>` with their values, kept until the plugin loads.
  - **CLI:** `persephone-cli plugin new <name> --kind indicator|strategy` writes a standalone crate (pinned to `wasm32-wasip2`, so rustup installs the target on first build); `plugin build [path]` compiles it, loads it with the real host and installs it only if it loads, printing what it is (`Installed strategy "Trend Follower" (Length, Size %)`); `plugin dir` prints the folder.
  - **Verified:** the WASM `ema_cross` gives exactly the same fills and equity curve as the built-in EMA Cross, and the WASM SMA matches Nautilus's to 1e-12. Fixtures `panics`, `spins` and `hungry` are contained, and `clock` gets the same answers every run.

### 6. ML and AI (`crates/ml`)
- **`ml`:** `load-model(name)` and `run(model, tensor) -> tensor`, backed by `ort` (ONNX Runtime with CoreML on Mac). The workflow is: train in PyTorch, sklearn or LightGBM, export to ONNX, drop the file into `models/`, and call it from a strategy.
- **`ai`:** `complete(request)` sends a request to an LLM provider (Claude, OpenAI, or local models through Ollama).
  - During backtests, a record/replay cache keyed by the hash of model, prompt and params makes reruns deterministic and free.
  - The look-ahead warning above is built into the report.
- **Later escape hatch:** an "external strategy" that talks to any process over a local socket and runs in lockstep during backtests. This covers full Python ML stacks or agent frameworks. *(Built after M7; see below.)*
- *(Built in M7:)* both are WIT imports the host serves: `ml` for indicators and strategies, `ai` for strategies. The SDK wraps them as `persephone_sdk::ml::run` and `persephone_sdk::ai::complete`.
  - **Models** (`persephone_ml::models`): **tract** instead of `ort`. It's pure Rust, so there's no ONNX Runtime library to download at build time, and it's deterministic on the CPU. It covers what strategy models use, including ONNX-ML tree ensembles for gradient-boosting exports, though fewer operators than ONNX Runtime and no CoreML. `run(name, tensor)` loads `…/persephone/models/<name>.onnx`, compiles it once per input shape, recompiles when the file changes, and returns every output as f32. `persephone_ml::models::logistic_regression` writes a small ONNX model without Python, used by the tests and by `persephone-cli models example`.
  - **Language models** (`persephone_ml::ai`): model names choose the provider. `claude-…` goes to the Messages API over raw HTTPS (there's no Rust SDK). `openai/…` and `ollama/…` go to OpenAI-compatible chat endpoints.
    - **Claude requests:** they send `anthropic-version: 2026-10-21`, never sampling parameters (that version rejects them), and `fallbacks: "default"` with its beta header on the models that support it (Fable 5.1, Opus 5.5, Opus 5, Sonnet 5.5). Refusals are reported as errors, 429/500/529 are retried with backoff, and thinking blocks are skipped for the text. The SDK example defaults to `claude-opus-5-5` at low effort.
    - **Keys:** from `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN` / `OPENAI_API_KEY`, or `…/persephone/ai.json`, which also holds base URLs, `offline` and `max_new_calls`.
    - **Recordings:** every reply is stored under the exact request in `…/persephone/ai-cache/`, with the request kept beside it so a hash collision can't return the wrong reply. Reruns are free and identical, offline mode uses recordings only, and each backtest may make 200 new calls by default.
  - **Permission:** whether a strategy asks models comes from its component's imports (unused imports are left out of components). Until the user presses "Allow it to ask AI models" in the strategy report, its calls fail; the grant is saved in `ai-grants.json` and rerunning is automatic. The strategy page marks such plugins.
  - **Look-ahead warning:** no knowledge-cutoff dates are hard-coded. Instead the report compares the earliest bar a model was asked about with the model's release date from the provider's models endpoint (cached, so it works offline). A model can't know anything after its release, so if any of those bars came before it, the report warns. Models with no release date (Ollama) always get the warning. Reports also count calls, recorded answers, unanswered questions and a hit budget, and the summary shows `!` when a note is a warning.
  - **Examples:** `ml-trend` (an ONNX model's probability of an up bar from the last four returns, with `train.py` for PyTorch) and `llm-daily` (asks once a UTC day for LONG/SHORT/FLAT from recent daily closes). Test fixtures are `ml-probe` and `ai-probe`; the AI tests run against a stand-in HTTP server, so no test needs a key or the network.
  - **CLI:** `persephone-cli models dir|list|example`, `persephone-cli ai status|clear-recordings`.
  - Not built yet: the external-strategy socket.

### 7. Data (`crates/data`)
- **One local store:** Nautilus `ParquetDataCatalog` in `…/persephone/catalog`. Charts and backtests both read from it.
- **Crypto:**
  - Binance and Bybit historical klines and trades come from their public REST APIs, using the Nautilus adapter HTTP clients where they support historical requests, and `reqwest` otherwise.
  - For years of history, the bulk monthly CSV archives at data.binance.vision go through the **same CSV importer** as your files.
- **Your CSV ticks** (`persephone-cli import csv <path>`, plus an in-app version):
  - It detects the delimiter and header, and the timestamp format (epoch s/ms/µs/ns or ISO-8601).
  - You map columns (timestamp, price, size, side, or bid/ask) and save the mapping as a profile so it can be reused.
  - Instrument specs (tick size, lot size) are entered once per symbol.
  - Files are read in chunks, never fully loaded into memory, and written to the catalog as `TradeTick` or `QuoteTick` partitioned by date.
- **Charting tick data:** ticks are aggregated into time, tick-count or volume bars, and the results are cached. Backtests can replay either bars or raw ticks; ticks give more realistic fills.
- *(Built in M6:)* the store is our own Parquet layout rather than Nautilus's `ParquetDataCatalog`. The catalog would have brought DataFusion (a very heavy build) and Nautilus into the data crate, against the rule that only `engine` touches Nautilus. Ticks are converted to Nautilus types in the engine instead, and the files stay plain Parquet that pandas, Polars or DuckDB can read.
  - **Store** (`persephone_data::store`, `…/persephone/store/<VENUE>/<SYMBOL>/`): `market.json` (instrument, trades or quotes, fee, range, count), one ZSTD Parquet file of ticks per UTC day (timestamps delta-encoded, floats byte-split), and monthly files of 1-minute bars written at import. Charts roll those bars up to their timeframe and never read ticks. Importing a file again replaces the ticks in the time span it covers, so nothing doubles, while files that each hold part of a day add up.
  - **Importer** (`persephone_data::import`): detection reads the first 256 KB. It works out the separator (`,` `;` tab `|`), the header row, decimal commas, and the time column and format: epoch s/ms/µs/ns, year-first text in any spelling (`2014-01-02T03:04:05.123Z`, `2014.01.02 03:04:05`, HistData's `20140102 030405123`), day- or month-first dates, or separate date and time columns. Offsets written in the text are honoured, and `--tz` takes a fixed offset or an IANA zone with daylight saving. Header names pick columns when recognisable; otherwise values do: ids rise every row, a bid and ask stay close with one always on top, finer epoch units beat trade ids that look like seconds, and true/false is Binance's `is_buyer_maker`. Tested on Bitcoincharts, Binance trades and aggTrades, HistData, TrueFX, Dukascopy, GainCapital, Kraken and MetaTrader-style exports. Files can be plain, `.gz` or `.zip` and stream a day at a time (4.8M rows/s on a release build). Rows that don't fit are skipped and reported with their line numbers, and layouts save as profiles. Columns, kind, separator, header and time format can all be overridden.
  - **CLI:** `persephone-cli import FILES… --symbol EURUSD [--venue …] [--tz …] [--dry-run] [--profile …|--save-profile …]`, `persephone-cli data list|remove|dir`, and `backtest --local SYMBOL.VENUE [--bar-fills]`.
  - **App:** the symbol page has a Local section, local tiles are saved with their venue, the chart header marks them `(local)`, and `persephone EURUSD.LOCAL` opens one. Dropping CSV files on a chart opens an import panel showing what was detected, with symbol, venue and time zone boxes and live progress; when it finishes, the chart switches to the new market.
  - *(Built later:)* tick-count and volume bars for local markets (`1000t`, `2.5v`), and `persephone-cli fetch binance` for Binance's monthly or daily archives.
  - *(Built later:)* **bar files and longer timeframes.** The importer also takes OHLC(V) bar files of any spacing from a minute to a month: header names (`open`/`o`, `high`, `low`, `close`) or, without a header, four neighbouring columns where the second is always highest and the third lowest. Date-only time columns are accepted for daily and longer bars. The spacing is the median gap between rows (`Spacing`: minutes that divide a day, a week or a month), worked out from the first file's sample at import too, so profiles stay spacing-free. Such a market is `"kind": "bars"` with a `"spacing"` in `market.json` (`MarketKind::Bars`), its bars stored as they are in `bars/<YYYY>.parquet`, moved to their spacing's open time; it charts at its spacing and the timeframes made of it, and backtests fill on bars. Charts gained `1w` (from Monday) and `1M` (from the 1st), UTC like Binance's; `Timeframe::open_time`/`close_time` give calendar-correct bar edges everywhere (roll-ups, closes, Binance paging), and weeks and months roll up across files through days. Live trading stays on 1m–1d.
  - *(Built later:)* the symbol search ends with **Import data…**, which opens the system's file picker (`rfd`) and then the same import panel as dropping files; the panel says when the files add to an existing market, or can't because it holds something else. Each Local row has a **×** that asks first (with the space it frees), then deletes the market's folder from the store on a worker thread; charts showing it switch to BTCUSDT.

### Backtest panel *(built after M7)*
- **Panel:** **Backtest** in a strategy's report opens a panel across the window's top for that chart (`crates/app/src/backtest/`): Start/Pause/⟲/Step, the equity so far and its statistics, a two-handled span slider over the market's whole history (weekly closes, log scale), a long logarithmic speed slider (1× to 10× over its left half, up to 1,000,000×, then Max: the rest in two seconds), and in its left 30% a strategy dropdown, the settings and a plugin search that adds tags. **Ctrl+B** or the right-click menu opens it. It's resizable, opens at 30% of the window and drives one chart, outlined.
- **Playback reveals a finished run.** The backtest runs once over the span; a playhead moves through market time and the chart shows only bars closed by then (`ChartState::set_playhead`: whole bars by index, studies clipped on the GPU at the last whole bar, the bar being built drawn by the overlay). Nothing reruns on pause, seek or speed changes. A chart's span is part of its `Market` (bars load for `[from, to)` plus 500 warm-up bars, saved with the layout); `BacktestRequest::trade_from` keeps warm-up bars from trading.
- **Fixed ("stock") settings**, each with a switch plus a master switch, saved per chart: capital, commission (% or per order), slippage and an assumed spread in ticks (a `CostFillModel` on Nautilus's open `FillModel` hook, offsetting a one-level simulated book), latency (`StaticLatencyModel`), leverage (capital × Size % × leverage; no margin calls), direction, inverting signals, and trading sessions (IANA zones; outside a session signals close but don't open). These are `TradingRules`, which live trading uses too.
- **Intra-bar precision:** Standard fills at the bars and reveals whole bars. High fills on ticks (imported tick markets) or 1-minute bars (Binance and 1-minute imports, loaded in the background, capped at 1M; Nautilus matches on the finest bars it's given), and at up to 20 bars a second builds the current bar from them, or from a simulated open→low→high→close path when there's nothing finer.
- **Plugins, planned:** slippage, fill (order book), spread and latency, position sizing, holding costs, commission schedules and session calendars can be added as tags from the panel's search (saved per chart, no effect yet) and are described in `docs/plugins.md`; none are built. Pyramiding and entry order types wait for a strategy contract with orders.
- **Robustness tests** (`backtest/robustness.rs`), chosen under **Test** in the settings and saved per chart; results aren't saved, and go when the backtest changes. **Monte Carlo** rebuilds round trips from the fills (netted, reversals split, fees shared, an open position at the last close, so they sum to the report's profit) and draws them with replacement on a worker thread; adding profits is fair because sizes come from starting capital, not compounding. **Random blocks** picks stretches of the loaded span with a seeded generator and backtests each on the chart's bars, with up to 500 warm-up bars, on at most CPUs − 1 workers (one for external strategies, none for AI strategies), each block on a new thread as the engine keeps state per thread. A `Fan` canvas shows both. Walk-forward isn't built; sweeps cover sensitivity and paper trading covers forward testing.
- **Optimize** (`backtest/optimize.rs`) tries one or two strategy parameters over Min/Max/Step, scored by return, by Monte Carlo's 5th percentile or by random blocks' median, on the same worker pool and at most 5,000 backtests. Values are ranked by their average with their neighbours, and by default picked on the span's first 70% with the top 5 rerun on the last 30% (warm-up bars before it, baseline capital). Bars or a heatmap show the results, with no-score values grey; applying one keeps the results and can be undone.
- **Calendar** (`backtest/calendar.rs`): months by year, days by week, then a day's trades (`robustness::round_trips`), by the UTC day each closed. Clicking a trade pauses and moves the playhead to 10 bars before it opened (`ChartState::show_playhead` brings the chart there); nothing reruns.
- **Why a test can't run** is worked out on every draw (`App::test_problem`): Run is disabled and a red line says why and how to fix it, with a Widen span button when that would. A batch's Run becomes Cancel while it runs.
- **Hover text** explains nearly every control and figure in the panel; sliders have theirs on their labels, so it doesn't cover them while dragging.
- Along the way: tick and volume bars filling on bars no longer crash Nautilus (they go by a time name), and Binance requests time out after 30 s.

### Persephone Lab *(built after M7)*
- **An open sandbox for agents** (`crates/lab`, binary `persephone-lab`): every data point and test as named tools, reached by MCP over stdio (`rmcp`), local HTTP or the command line, all through one `dispatch`. It deliberately has no research rules. An agent's rules belong in its own setup, which can decide from a call's structured arguments before it runs; Lab only logs calls.
- **Three boundaries:** no API keys (and no language models, so nothing can spend money), no orders (built without the engine's `live` feature), and no deleting or overwriting data. HTTP is local only, with a token and Host and Origin checks.
- **Shared code** moved to `crates/research` so the app and Lab can't drift: settings, market loading, Monte Carlo, random blocks, optimize, the calendar and saved results.
- **Logic trees** (`research/logic.rs`): strategies as JSON that implement `SignalFactory`, so they run through the existing `BacktestRequest`. They only get one closed bar at a time, and `sma`, `ema`, `rsi` and `atr` use Nautilus's own through `indicators::Streaming`.
- **Results:** `save_result` writes a file the app watches with `notify`; they join the panel's strategy list under the kind `Plugin("lab:<id>")`, so they save with a layout like plugins.

## Milestones (each ends with something you can run)

| # | Milestone | Done when |
|---|---|---|
| M0 | Foundation | Workspace, toolchain (`rustup update`), and `persephone-cli backtest` runs the built-in EmaCross through `engine` and prints stats. |
| M1 | One chart | The app opens one GPU chart of Binance BTCUSDT 1m from the catalog, with pan, zoom, axes and crosshair. |
| M2 | Tiling | Dwindle layout, full keymap, animations, the 16-tile cap, and the layout restored at launch. |
| M3 | Add menu and built-in indicators | Left-click menu; built-in indicators as overlays and sub-panes; switching symbol and timeframe. |
| M4 | Backtests on charts | Adding a strategy runs it on a worker thread, with trade markers, position shading, an equity sub-pane and the stats overlay. |
| M5 | WASM plugins | WIT, SDK, host and hot reload. The example SMA indicator and EMA-cross strategy work as plugins. |
| M6 | CSV tick import | Your files import, chart as aggregated bars, and run tick-level backtests. This doesn't depend on M2–M5 and can move earlier. |
| M7 | ML and AI | `ml` (ONNX) and `ai` (LLM plus cache) capabilities, each with an example strategy. |
| Later | Built after M7 | Workspaces (Ctrl+1..9), parameter-sweep grid, crosshair synced across tiles, paper and live trading through Nautilus's live node, drawing tools, tick and volume bars, Binance archive fetching, external strategies over a socket. See below. |

## Built after M7
- **Workspaces:** Ctrl+1..9 switches between nine sets of tiles, each with its own layout; Ctrl+Shift+1..9 moves the focused chart. All of them are saved. Charts on hidden workspaces keep backtesting and catch up when shown.
- **Synced crosshair:** hovering a bar shows a vertical line at the same time on every other chart, whatever its timeframe.
- **Drawing tools:** trend lines, horizontal lines and rectangles from the menu's Draw page. They're anchored to time and price, so they show on every timeframe of the market, and they're saved per market. Click a drawing to remove it.
- **Parameter sweeps:** "Sweep a parameter…" in a strategy's report opens 2–9 charts on a free workspace, one per value, and the report compares their returns.
- **Tick and volume bars:** local markets can be cut into a bar every N ticks or every so much traded size. These bars close at their last tick, so backtests never see a bar before it's complete.
- **Binance archives:** `persephone-cli fetch binance BTCUSDT --from 2024-01 --to 2024-06` downloads data.binance.vision's monthly (or daily) aggTrades or trades, checks each file against its published SHA-256, and imports them.
- **Paper and live trading** (`persephone_engine::live`, behind the engine's `live` feature):
  - The same strategies run on Nautilus's `LiveNode`, on Binance's public kline streams, which need no keys.
  - Paper orders fill in Nautilus's sandbox, the same way backtest orders do. A test replays bars through the live node and checks the fills against a backtest of the same bars.
  - **App:** a strategy's report has "Paper trade on live bars". It warms the strategy up on the latest bars, appends closed bars to the chart, shows the session's trades from where it started, and puts its profit in the chart's corner. "Trade on Binance's testnet" appears when `BINANCE_TESTNET_API_KEY` and `BINANCE_TESTNET_API_SECRET` are set.
  - **CLI:** `persephone-cli live [--testnet | --real-money]`. Real money needs `BINANCE_API_KEY`/`BINANCE_API_SECRET` and the exact confirmation phrase typed at a terminal, and it's only in the CLI.
  - Keys are read by Nautilus from the environment, never by Persephone.
  - On spot accounts (testnet and real money), strategies go flat instead of short. Stopping a session closes its position.
- **Menus and axes** (changed on request): the menu opens on a right click, and a left click
  only closes it. Clicking the header's symbol turns it into a search box with its own dropdown
  of matches; clicking the timeframe drops down the timeframes. The
  time axis drags like the price axis. The menu starts with an **Active** list that shows or
  hides each indicator, the strategy and the drawings, saved with the layout. Each indicator
  also has a round tag after the chart's name that shows or hides it with one click, and so
  does volume (the first tag).
- **Corner buttons:** each chart has **+** (new chart), **◫** (flip split) and **✕** (close)
  in the corner between its axes, each naming its shortcut on hover.
- **External strategies** (`persephone_plugin_host::external`):
  - A `NAME.external` file in the plugins folder names a process listening on TCP or a Unix socket. It speaks JSON lines: `describe`, `start`, one `bar` at a time, then `end`.
  - Persephone waits for each answer, so a backtest runs in lockstep. External strategies appear alongside the plugins and work everywhere they do: backtests, sweeps and live sessions.
  - `plugins/external/ema_cross.py` is a standard-library Python example. `persephone-cli backtest --external tcp://127.0.0.1:7878` runs one headless.

## Risks
- **Nautilus API churn** (0.x, v2 still a release candidate). Pin to an exact version and keep everything Nautilus inside `engine`. Expect some upgrade work every few releases, and read the crate examples, because the Rust docs are thinner than the Python ones.
- **The GPU chart is the biggest single piece of work.** Keep M1 to the essentials: text stays on the Canvas layer, and drawing tools come later.
- **LGPL (license undecided):** Flowsurface (GPL) is for ideas only, never copied code. If you go closed-source later, get a legal review of the LGPL rules for statically linking Nautilus.
- **Per-event WASM cost** in tick-level backtests. Measure it at M5 with a criterion benchmark (native vs WASM strategy, bars/sec). Batch `compute` already covers the chart path.

## Verification
- **`cargo test --workspace`:**
  - **tiling:** proptest checks that rects cover the screen with no overlaps or gaps, the cap holds, and focus and swap go in the right direction.
  - **chart:** unit tests for the viewport math and LOD bucketing.
  - **data:** CSV fixtures in each timestamp format, and tick→bar aggregation against hand-computed bars.
  - **plugin-host:** the example WASM SMA matches `nautilus-indicators` SMA on the same bars, a trapping plugin is contained, and an infinite loop is stopped.
  - **engine:** a headless EmaCross run through our wrapper gives the same fills and PnL as Nautilus's own example. The WASM ema-cross gives the same result as the native one.
- **Headless smoke test:** `persephone-cli backtest --strategy plugins/examples/ema-cross.wasm --symbol BTCUSDT --tf 1m`.
- **Manual app checklist** per milestone (`cargo run -p persephone`):
  - Ctrl+Enter up to 16 tiles; the 17th is refused.
  - Focus, swap and resize all work.
  - Left-click → add RSI.
  - Add a strategy → markers and stats appear.
  - Edit plugin code and rebuild → the chart updates without a restart.
  - The debug frame-time overlay stays under 8 ms with 16 tiles.

## Sources
- [NautilusTrader repo](https://github.com/nautechsystems/nautilus_trader) · [Rust concepts doc](https://nautilustrader.io/docs/latest/concepts/rust/) · [nautilus-analysis 0.66](https://docs.rs/nautilus-analysis/latest/nautilus_analysis/) · [nautilus-indicators 0.66](https://docs.rs/nautilus-indicators/latest/nautilus_indicators/)
- [Flowsurface](https://github.com/flowsurface-rs/flowsurface), an iced charting app (prior art, GPL) · [iced 0.14 release](https://github.com/iced-rs/iced/releases/tag/0.14.0) · [iced Shader widget](https://docs.iced.rs/iced/widget/shader/index.html)
- [Lightweight Charts license notes](https://github.com/TradingView-Host/lightweight-charts) · [WASM plugin performance notes (uni-plugin-wasm)](https://docs.rs/uni-plugin-wasm)
