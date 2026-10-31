## models

Using a model you trained: testing its predictions, and what a time means in each tool.

```json
{"tool": "run_backtest", "args": {
  "symbol": "BTCUSDT", "bars": "1h", "from": "2024-01-01", "to": "2024-03-01",
  "strategy": {"positions": [[1704067200000000000, 1], [1704153600000000000, -1], [1704240000000000000, 0]]},
  "settings": {"slippage_percent": {"on": true, "value": 0.01}, "fill_timing": "next_open"}
}}
```

**Three ways in, simplest first:**

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

**What a time means, by tool.** Getting this wrong by one bar invalidates a model's test:

| Where | It's when |
|---|---|
| `ts` in `get_bars`, `export_data`, `positions` and `get_equity` | The bar opens. `get_equity`'s value is at that bar's close |
| `ts` in `get_fills`, and `opened` and `closed` in `get_trades` | The bar it filled at closes, so a list's entry fills one bar's length after its `ts` |
| `ts` sent to a program over a socket, and `hour` and `weekday` | The bar closes |
| `bar`, `entry_bar` and `exit_bar` | Not a time: numbers counting warm-up bars |

So a prediction made from a bar's close has that bar's `ts` in the list, and fills at its close, or at the next bar's open with
`fill_timing: "next_open"`. To check the timing, give a list perfect foresight on a short span, then move it a bar each
way: the return should collapse.

To see why a model loses, read `gross_return` beside `net_return` and `break_even_cost_per_fill`: a small edge that
trades every bar can look good before costs and lose everything after them.
