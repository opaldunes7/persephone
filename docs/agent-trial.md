# Running an agent trial

An agent trial is an AI agent doing real research with Lab, with no sight of Persephone's code, then
reporting what it found, what went wrong and what it missed. Six trials so far have each found real
bugs (see `docs/plan.md`). Run one on your own machine, where Binance is reachable, and paste its
report back to whoever is working on Persephone.

## Setup

Build Lab from the branch you want tried:

```bash
git fetch && git checkout claude/exciting-gates-mnu16u
```

```bash
cargo build --release -p persephone-lab
```

Give the agent an empty folder of its own, so it can't see the code:

```bash
mkdir -p ~/persephone-trial/.claude && cd ~/persephone-trial
```

Connect Lab to Claude Code there, with the full path to the program you just built:

```bash
claude mcp add persephone-lab -- /full/path/to/persephone/target/release/persephone-lab mcp
```

Then choose how blind it is, in `~/persephone-trial/.claude/settings.json`. Replace
`/full/path/to/persephone` with where the repository is; the leading `//` marks an absolute path.
Claude Code's searches follow its Read rules, so that one rule keeps the code out of sight.

**Strict** (recommended): Lab is its only way to market data, so any look-ahead it finds is Lab's.
It can't run scripts, so it does its arithmetic itself.

```json
{
  "permissions": {
    "allow": ["mcp__persephone-lab"],
    "deny": [
      "Bash",
      "WebFetch",
      "WebSearch",
      "Read(//full/path/to/persephone/**)"
    ]
  }
}
```

**Loose:** the same, without `"Bash"` in `deny`, so it can write and run scripts in its folder, as
Trial 6 did. It could then fetch Binance's public history itself, around Lab, so a finding about
look-ahead is weaker.

Optionally, open the app with the News sidebar on for a while first, so `get_news` has headlines to
read. Feeds only carry their last few dozen items, so a fresh install has only recent news.

Start Claude Code in `~/persephone-trial` and paste the brief below.

## The brief

```text
You have Persephone Lab, a backtesting sandbox, as MCP tools (persephone-lab). Call get_guide first
and read what it points you to. You can't see Persephone's code and shouldn't try to.

Goal: find a strategy for BTCUSDT and/or ETHUSDT (spot, or -PERP perpetuals with their funding),
on 1h or 4h bars, that is worth trading with real money after costs. If there isn't one, say so:
"nothing worth trading" is a good answer when it's true.

Try hard not to fool yourself. Before believing any result, use all of these, and say what each
told you:
- walk-forward on what follows (run_walk_forward or run_portfolio_walk_forward), against
  buy_and_hold_whole, not just the summed figures;
- run_shifted and run_random_entries, giving variants_tried honestly: every setting, rule, span
  and market you tried before picking this one, counted as you go;
- the deflated Sharpe ratio where a tool gives one, against the plain Sharpe ratio;
- run_monte_carlo, knowing what it does and doesn't show;
- realistic costs: fees, slippage, and fill_timing "next_open", with what each kind of cost took
  (slippage_paid, impact_paid and the rest);
- a portfolio of BTC and ETH together (run_portfolio), with get_portfolio_trades and its beta;
- at least one strategy written as a logic tree.

Keep a running count of every variant tried. Say plainly whenever you ran a test after looking at
results it was meant to check.

Whenever an answer looks wrong, contradicts another tool's, or isn't what the guide says, note the
exact call (tool and arguments), what it said, and what you expected and why, then carry on. These
problems are as valuable as the finding.

Stop after about 2-3 hours, or 300 tool calls, whichever comes first, and write the report:

1. Finding: your verdict on the edge, with the figures behind it (returns, falls, Sharpe and
   deflated Sharpe, out-of-sample results, costs, how many variants you tried).
2. Problems: numbered. Each with the exact call, what it said, what you expected, and why.
3. Missing: tools, figures or options you wanted and didn't have.
4. What worked well: short.
Say where any notes or scripts you wrote are.
```

## What to send back

The whole report as the agent wrote it, and which setup (strict or loose) it ran under. Every
problem is then checked against the code and sorted into fixed, partly fixed, not fixed or not a
bug, with the evidence, before anything is changed.
