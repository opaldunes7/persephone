# Persephone Lab

Lab gives anything outside the app, such as an AI agent or a script, the same access to Persephone
as you have: it can read every data point and run every test. It runs on its own, without the app
open, so research can go on for hours.

**Lab is a sandbox, not a referee.** It has no research rules: nothing is sealed, and any strategy,
span or number of tries is allowed. An agent's own rules belong in that agent's setup. Every call is
one named tool with explicit arguments, so that setup can check a call and block it before it runs.
For example, a pre-tool hook could refuse any call whose `to` is after a chosen date.

Those rules only hold if the agent has no other way to the data, so a locked-down agent needs its
shell and web access removed too. Binance's history is public, so an agent with either could simply
fetch it.

**It can't:**
- Give out API keys. It doesn't read `ai.json`, and it can't call language models, so a strategy
  plugin that asks one gets no answer and nothing here can spend money on them.
- Place orders. It's built without the engine's live trading.
- Delete or overwrite your data. It can add markets, plugins and results, but can't remove imported
  markets or change your layout or drawings.

## Getting it

```bash
cargo build --release -p persephone-lab
```

The program is `target/release/persephone-lab`. It uses the same folders as the app, so it sees the
same markets, plugins and models.

## Three ways in

They all reach the same tools and give the same answers.

### MCP, for agents

For Claude Code:

```bash
claude mcp add persephone-lab -- /full/path/to/persephone-lab mcp
```

Other MCP clients can run `persephone-lab mcp` the same way. It talks over standard input and output.

### HTTP, for scripts and custom agents

```bash
persephone-lab serve --port 8765
```

It only listens on this machine. The token is in the `lab/http-token` file in the app's data folder,
and only you can read it.

```bash
curl -X POST http://127.0.0.1:8765/v1/tools/get_bars \
  -H "Authorization: Bearer $(cat "$HOME/Library/Application Support/persephone/lab/http-token")" \
  -d '{"symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "limit": 5}'
```

`GET /v1/tools` lists the tools and their arguments. A failed call answers `400` with `{"error": ...}`.

### The command line, for quick checks

```bash
persephone-lab tools
```

```bash
persephone-lab call run_backtest '{"symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "strategy": {"builtin": "ema-cross"}}'
```

## The tools

[The full list](lab-tools.md) is generated from the tools themselves. In short:

| For | Tools |
|---|---|
| Markets and data | `list_markets`, `get_instrument`, `get_history_range`, `get_bars`, `get_ticks`, `export_data`, `import_data`, `fetch_archive` |
| Indicators and models | `list_indicators`, `compute_indicator`, `list_models`, `run_model` |
| Strategies | `list_strategies`, `describe_strategy`, `validate_logic`, `install_plugin` |
| Running tests | `run_backtest`, `run_monte_carlo`, `run_random_blocks`, `run_optimize` |
| Reading a run | `get_report`, `get_trades`, `get_fills`, `get_equity`, `get_returns`, `get_calendar`, `list_runs`, `compare_runs` |
| Long jobs | `get_job`, `cancel_job` |
| The app | `get_layout`, `get_chart_settings`, `get_drawings`, `save_result`, `list_results` |
| What's happened | `get_call_log` |

- **Big answers come in pages.** Pass a page's `next_cursor` as `cursor` to carry on. `export_data`
  writes any amount to a CSV or Parquet file, including for training models elsewhere.
- **Slow work becomes a job.** A call waits `wait_seconds` (30 by default), then answers with a
  `job_id` to check with `get_job` if it isn't finished. The command line waits as long as it takes.
- **Runs are saved.** `run_backtest` gives a `run_id` for reading the details later, even after a
  restart.
- **The call log** in `lab/calls.jsonl` records every call, with its tool and arguments. Lab enforces
  nothing with it, but it's there for an agent's rules, such as counting attempts.

## Strategies without code

A strategy can be written as JSON and tested straight away, with nothing to compile:

```json
{
  "name": "Fast over slow",
  "params": { "fast": 10, "slow": 30 },
  "long":  { "crosses_above": [{ "ema": { "length": "$fast" } }, { "ema": { "length": "$slow" } }] },
  "short": { "crosses_below": [{ "ema": { "length": "$fast" } }, { "ema": { "length": "$slow" } }] }
}
```

Pass it as `{"logic": {...}, "params": {...}}` wherever a strategy goes. `validate_logic` with no
arguments lists everything a tree can be made of, and with one says where it's wrong.

- **Values:** a number, a bar's `open`, `high`, `low`, `close` or `volume`, a parameter as `$name`, an
  indicator (`sma`, `ema`, `rsi`, `atr`, `highest`, `lowest`, `roc`, `zscore`), an earlier value
  (`{"ago": {"of": "close", "bars": 1}}`), or arithmetic (`add`, `sub`, `mul`, `div`). Indicators read
  the close unless given `of`, and can read another indicator.
- **Rules:** `gt`, `lt`, `ge`, `le`, `crosses_above`, `crosses_below`, `and`, `or`, `not`.
- **What they do:** `long` and `short` ask for that position. On a bar with both or neither, `flat`
  asks to close, and otherwise the position stays as it is.
- **No looking ahead:** a tree only ever gets one closed bar at a time, so it can't. `sma`, `ema`, `rsi`
  and `atr` are the same ones charts and built-in strategies use.

## Seeing results in the app

`save_result` saves a strategy, with its settings and how it did. It appears in the backtest panel's
strategy list straight away, marked **Lab**. Pick it to put it on the chart, then use Backtest, the
calendar and trade replay as usual. If the app isn't open, it's there next time.
