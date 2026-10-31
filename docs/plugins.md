# Writing plugins

Indicators and strategies of your own come in two kinds:

- **WebAssembly plugins**, written in Rust with `persephone-sdk`. They run sandboxed inside
  Persephone at close to native speed, can run ONNX models and ask language models, and reload
  the moment you rebuild them.
- **External strategies**: any program, in any language, that answers over a socket. Use these
  for a full Python ML stack, an agent framework, or anything that can't be WebAssembly.

Both show up in the chart menu next to the built-ins. They work everywhere the built-ins do:
backtests, sweeps, and paper and live trading.

- [A first plugin](#a-first-plugin)
- [Indicators](#indicators)
- [Strategies](#strategies)
- [The sandbox](#the-sandbox)
- [ONNX models](#onnx-models)
- [Language models](#language-models)
- [External strategies](#external-strategies)
- [Backtest plugins (planned)](#backtest-plugins-planned)

## A first plugin

```bash
persephone-cli plugin new hull-ma                       # an indicator crate in ./hull-ma
persephone-cli plugin new my-strategy --kind strategy   # or a strategy
persephone-cli plugin build hull-ma                     # build it and install it
```

`plugin new` creates a small crate that already works: a moving average, or an EMA cross. It
comes set up for the `wasm32-wasip2` target. `plugin build` compiles a release build and
copies the `.wasm` file into the plugins folder (`persephone-cli plugin dir` prints where). A
running Persephone loads it within a moment, and charts using it recompute. Edit, run
`plugin build` again, and watch the chart change.

If something is wrong, the plugin shows in the Indicators page with the reason, and charts
using it show the error in their legend. Nothing crashes.

You can also build by hand with `cargo build --release --target wasm32-wasip2` and copy
`target/wasm32-wasip2/release/<name>.wasm` into the plugins folder. The plugin's id is its
file name, and saved layouts refer to it by that.

**Examples**, in `plugins/examples/`:

| Plugin | Kind | Shows |
|---|---|---|
| `sma` | indicator | the minimum: one line over the prices |
| `zscore` | indicator | its own pane, levels and a fixed range |
| `ema-cross` | strategy | the minimum strategy; trades exactly like the built-in EMA Cross |
| `ml-trend` | strategy | an ONNX model's call on each bar (with `train.py` to train one) |
| `llm-daily` | strategy | asking a language model once a day |

Build them all with `cargo build --release --target wasm32-wasip2` in `plugins/`.

## Indicators

An indicator describes itself once, then computes all of its values for the whole chart in
one call:

```rust
use persephone_sdk::{Bar, Indicator, IndicatorInfo, Param, export_indicator, ta};

struct Sma;

impl Indicator for Sma {
    fn describe() -> IndicatorInfo {
        IndicatorInfo::overlay("Simple Moving Average", "SMA")
            .param(Param::period("Length", 20))
            .line("SMA")
    }

    fn compute(params: &[f64], bars: &[Bar]) -> Result<Vec<Vec<f64>>, String> {
        let closes: Vec<f64> = bars.iter().map(|b| b.close).collect();
        Ok(vec![ta::sma(&closes, params[0] as usize)])
    }
}

export_indicator!(Sma);
```

The crate needs `persephone-sdk` with the `indicator` feature, and `crate-type = ["cdylib"]`.
`plugin new` sets both up.

**Describing it** (`IndicatorInfo`):
- `overlay(name, short)` draws on the prices; `pane(name, short)` gets a pane of its own.
- `.param(...)` adds a parameter the user can edit:
  - `Param::period(name, default)` is a whole number from 1 to 1000;
  - `Param::number(name, default, min, max)` is any number in that range.
- `.line(name)` or `.histogram(name)` adds an output series, in the order `compute` returns
  them.
- `.fill(upper, lower)` shades between two outputs (by index), as Bollinger bands do.
- `.level(value)` adds a dashed reference line; `.range(low, high)` fixes the pane's scale,
  e.g. 0 to 100.

**Computing it:** return one series per output, each exactly as long as `bars`. Use
`f64::NAN` where there's no value yet. `params` come in the order you described them. An
`Err` shows its message in the chart's legend.

`bars` are the chart's bars, oldest first, with `ts` as each bar's open time in nanoseconds
since 1970 (UTC). `persephone_sdk::ta` has `sma`, `ema` and an incremental `Ema`.

## Strategies

A strategy sees one closed bar at a time and says which position it wants. Persephone places
the orders.

```rust
use persephone_sdk::{Bar, Param, Strategy, StrategyInfo, export_strategy, ta::Ema};

struct EmaCross {
    fast: Ema,
    slow: Ema,
    fast_above: Option<bool>,
}

impl Strategy for EmaCross {
    fn describe() -> StrategyInfo {
        StrategyInfo::new("EMA Cross (plugin)", "Trend: follow fast/slow EMA crosses")
            .param(Param::period("Fast", 10))
            .param(Param::period("Slow", 30))
    }

    fn start(params: &[f64]) -> Result<Self, String> {
        if params[0] >= params[1] {
            return Err("Fast must be shorter than Slow".into());
        }
        Ok(Self {
            fast: Ema::new(params[0] as usize),
            slow: Ema::new(params[1] as usize),
            fast_above: None,
        })
    }

    fn on_bar(&mut self, bar: &Bar) -> Option<i8> {
        self.fast.update(bar.close);
        self.slow.update(bar.close);
        let above = self.fast.value()? > self.slow.value()?;
        let target = match self.fast_above {
            Some(false) if above => Some(1),
            Some(true) if !above => Some(-1),
            _ => None,
        };
        self.fast_above = Some(above);
        target
    }
}

export_strategy!(EmaCross);
```

This crate needs the `strategy` feature.

**The lifecycle:**
- `start` gets the user's parameters (an `Err` stops the backtest with that message).
- `on_bar` is called for every bar, oldest first, as each bar closes. Its `ts` is the bar's
  close time.
- Return `Some(1)` for long, `Some(-1)` for short, `Some(0)` for flat, or `None` to keep the
  current position.

**What Persephone handles:**
- Every strategy gets a **Size %** parameter after yours. Each position is worth that share of
  the balance, and Persephone handles it, so `start` never sees it.
- Orders are market orders, filled at the close of the bar that triggered them (or on the
  ticks that follow, on imported tick data).
- Going from long to short is one order.
- When paper or live trading, `start` and `on_bar` run the same way. On spot accounts (testnet,
  real money), a short becomes flat.

## The sandbox

Every plugin call runs in a fresh, locked-down WebAssembly instance:
- **Nothing outside:** no files, no network, no environment variables.
- **Memory:** at most 256 MB.
- **An instruction budget:** an infinite loop is stopped rather than freezing the app. There's
  about a second of work for an indicator's `compute`, and much less per `on_bar`.
- **Determinism:** the clock is frozen and randomness is seeded, so the same inputs always
  give the same results and backtests are reproducible.
- **Panics:** a panic stops that backtest or indicator, with your panic message shown.

Persephone serves two things across the sandbox's edge: ONNX models and language models.

## ONNX models

Train a model anywhere (PyTorch, scikit-learn, LightGBM, …), export it to ONNX, and save it as
`<name>.onnx` in the models folder (`persephone-cli models dir`; `models list` shows what's
there). Then, from an indicator or a strategy:

```rust
use persephone_sdk::ml;

let outputs = ml::run("trend", &ml::Tensor::row(&features))?;   // features: &[f32]
let probability = outputs[0].data[0];
```

Models run natively, outside the sandbox, through a pure-Rust ONNX runtime. They're loaded
once and shared, and inputs and outputs are `f32` tensors (`Tensor::row`, `Tensor::rows`).
`persephone-cli models example` writes a starter `trend.onnx` for the `ml-trend` example, and
`plugins/examples/ml-trend/train.py` trains a real one on your bars.

## Language models

Strategies can ask Claude, OpenAI models, or local models through Ollama:

```rust
use persephone_sdk::ai;

let reply = ai::complete(
    &ai::Request::new("claude-opus-5-5", prompt)
        .system("Reply with one word: LONG, SHORT or FLAT.")
        .effort(ai::Effort::Low),
)?;
```

**Models:** a Claude model id, `openai/<model>`, or `ollama/<model>`.

**Permission:** prompts leave the machine and calls can cost money, so a strategy that uses
`ai` can't reach a model until you click **Allow it to ask AI models** in its report. Until
then, `complete` returns an error. The menu marks such strategies "asks AI models".

**Recording:** every reply is recorded. A backtest asks each distinct question once, and
reruns give the same answers for free.
- Each backtest may make only so many *new* calls (200 by default).
- Offline mode uses recorded replies only.
- Ask sparingly, say once a day rather than every bar; `llm-daily` shows how.

**Look-ahead:** a model released after the bars it's asked about may already know how they
turned out. The report warns when a backtest covers dates before the model's release. Judge
such strategies on bars after that date, or by paper trading them.

**Setup:** keys come from the environment, `ANTHROPIC_API_KEY` (or `ANTHROPIC_AUTH_TOKEN`) and
`OPENAI_API_KEY`, or from `ai.json` in the app folder. Every field is optional:

```json
{
  "anthropic_api_key": "…",
  "openai_api_key": "…",
  "ollama_base_url": "http://localhost:11434/v1",
  "offline": false,
  "max_new_calls": 200
}
```

`persephone-cli ai status` shows what's set up (without showing keys).
`persephone-cli ai clear-recordings` forgets recorded replies.

## External strategies

An external strategy is a program that listens on a local socket. Persephone sends it one bar
at a time and waits for its answer before sending the next, so a backtest runs in lockstep.
It's exactly as deterministic as your program is.

### Hooking one up

1. Start your program; it listens on, say, `127.0.0.1:7878`.
2. Put a file named `<name>.external` in the plugins folder, holding the address:

   ```text
   # my-model.external
   tcp://127.0.0.1:7878
   ```

   A Unix socket works too: `unix:///tmp/my-model.sock`.
3. Persephone asks the program to describe itself, and the strategy appears in the menu, marked
   "runs at tcp://…".

If the program isn't running when Persephone loads the file, the Indicators page shows the
error. Start the program, then save the `.external` file again to retry.

From the command line, without a file:

```bash
persephone-cli backtest --external tcp://127.0.0.1:7878 --params 10,30 --symbol BTCUSDT --timeframe 1h
persephone-cli live --external tcp://127.0.0.1:7878 --symbol BTCUSDT --timeframe 1m
```

### The protocol

JSON, one object per line each way: Persephone sends a request, and your program answers with
one line.

| Persephone sends | Your program answers |
|---|---|
| `{"type":"describe"}` | `{"name":"…","description":"…","params":[{"name":"Fast","default":10,"min":1,"max":500,"integer":true}]}` |
| `{"type":"start","params":[10.0,30.0]}` | `{}` |
| `{"type":"bar","ts":…,"open":…,"high":…,"low":…,"close":…,"volume":…}` | `{"target":1}`: `1` long, `-1` short, `0` flat, `null` keep |
| `{"type":"end"}` | `{"notes":[{"text":"…","warning":false}]}`, or `{}` |

**Connections:**
- Describing uses a connection of its own.
- Each backtest, sweep chart or live session opens another connection and sends `start`, every
  bar, then `end`. A live session just closes the connection when it stops.
- Several connections can be open at once, so serve each on its own thread.

**Messages:**
- Parameters need `name`, `default`, `min` and `max`; `integer` is optional. `params` in
  `start` leave out Size %, which Persephone handles.
- `ts` is the bar's close time in nanoseconds since 1970, UTC.
- Notes appear at the bottom of the backtest report.
- Any answer may be `{"error":"…"}` instead, which stops the run with that message.

**Timing:** Persephone waits up to 60 seconds for each answer. A model call per bar is fine; a
program that hangs fails the backtest instead of freezing it.

### A Python example

[`plugins/external/ema_cross.py`](../plugins/external/ema_cross.py) is a complete external
strategy using only Python's standard library, with
[`ema_cross.external`](../plugins/external/ema_cross.external) to go with it:

```bash
python3 plugins/external/ema_cross.py          # listens on 127.0.0.1:7878
cp plugins/external/ema_cross.external "$(persephone-cli plugin dir)/"
```

"EMA Cross (Python)" then shows up with the strategies. Replace `EmaCross.on_bar` with
anything: pandas, scikit-learn, PyTorch, a call to an agent.

## Backtest plugins (planned)

The backtest panel's settings fix what's the same everywhere: capital, a commission, a constant
slippage and spread, latency, leverage, direction and trading sessions. What depends on the
market, the broker or your own theory will be plugins. None of these kinds exist yet; the
backtest panel's plugin search lists them, and adding one makes a tag, but a tag has no effect
on results until that kind exists.

| Kind | Would see | Would decide | For example |
|---|---|---|---|
| Slippage model | each order, and the bar or ticks it meets | how far its fill price moves | slippage growing fast once an order is more than 5% of the bar's volume |
| Fill model | each order, and the best bid and ask | the liquidity it can take: a simulated order book | partial fills; limit orders that fill only once the price trades through them |
| Spread and latency | the time, and data you bring (such as an economic calendar) | the spread to assume and how late orders arrive | spreads 10 pips wider and orders 200 ms late when CPI is released |
| Position sizing | the account, the market's recent bars and each signal | how big the position is | Kelly, volatility or ATR targeting, risk per trade |
| Holding costs | positions held over each rollover | what they cost | your broker's historical swap or funding rates |
| Commission schedule | each fill and the volume traded so far | its fee | fees that fall as monthly volume grows |
| Session calendar | the date | whether the market is open | exchange holidays and half days |

The stock slippage, spread and latency already sit on the simulated exchange's own fill and
latency hooks, which take any model, so plugins of the first three kinds will replace the stock
settings there rather than adding a second mechanism.
