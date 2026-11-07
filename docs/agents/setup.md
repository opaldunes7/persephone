# Agent setup files

A setup file is one JSON file holding a whole team of agents: what it studies, how its agents
work, every agent's role, model and limits, who reports to whom, who hands work to whom, who
reviews whom, which Lab they use, and where their cards sit. Import it and you have that exact
team, ready to run.

This page is written for you and for another AI. To have an AI write one, give it this page and
say what you need, for example:

> Read docs/agents/setup.md. Write me a Persephone agent setup file for a team that studies
> whether funding rates on BTC and ETH perpetuals predict returns. Use a Fable lead, three Haiku
> analysts, and a Sonnet reviewer for the lead. Keep each session under $5.

- [Importing and exporting](#importing-and-exporting)
- [The format](#the-format)
- [What makes a team runnable](#what-makes-a-team-runnable)
- [How it will run](#how-it-will-run)
- [An example](#an-example)

## Importing and exporting

**In the app:** open the Agents view (Ctrl+G). Under **Setup files** in the teams list,
**Import…** makes a team from a file, and **Export…** saves the team on the canvas as one. A file
whose team name is taken comes in as "Name 2". A file with a mistake is refused, saying what's
wrong and where.

**From the command line:**

```bash
persephone-agents import my-team.json
persephone-agents import my-team.json --name "Another name"
persephone-agents export "My team" my-team.json
```

**From another AI**, through the `persephone-agents` tools (see [Agents](README.md#from-another-ai)):
`import_team` with `setup` (the JSON itself) or `path`, and `export_team` with `team` and an
optional `path`.

**What's in a file, and what isn't:**
- **In it:** the design: everything in this format.
- **Not in it:**
  - your agents' memory, journals and reports, so an imported team starts fresh
  - where Lab's program is on your computer: each computer finds its own

## The format

Every key is checked: a misspelt key, or a value of the wrong kind, is refused with its place, as
in `Agent 2 has "modle", which a setup doesn't take`. Agents and links name each other by `name`.

### The team

| Key | Kind | Default | What it is |
|---|---|---|---|
| `format` | text | | `"persephone-agents/1"`. Optional, but write it. |
| `name` | text | **required** | The team's name. |
| `brief` | text | `""` | What the team studies: the question, markets, timeframes, spans. **Needed to run.** |
| `principles` | text | the agent kit's | How every agent works: method, not market views. Leave it out for the kit's eight principles: count every variant, say when a test came after seeing results, accept "nothing worth trading", costs on by default, compare with holding, one period proves little, report tool defects with the exact call, no live trading. |
| `at_once` | whole number | 2 | How many sessions may run at the same time, 1 to 4. |
| `lab` | object | own data | Which data Lab works on. See below. |
| `agents` | list | none | The agents. See below. |
| `links` | list | none | Hand-offs and reviews between agents. See below. |
| `view` | object | | Where the canvas looks: `{"at": [x, y], "zoom": 1.0}`. Used only when every agent has `at`. |

### `lab`

| Key | Kind | Default | What it is |
|---|---|---|---|
| `data` | text | `"own"` | `"own"`: a folder of the agents' own, so their runs don't mix with yours. `"app"`: your app's data, so what they save shows in your saved results. |
| `folder` | text | `~/persephone-agent-data` | With `own`: the folder. `~` is the home folder. |
| `at` | `[x, y]` | below the agents | Where the Lab card sits. |

### Each agent

| Key | Kind | Default | What it is |
|---|---|---|---|
| `name` | text | **required** | Unique in the team, and with at least one letter or digit. Others name it by this. |
| `role` | text | `""` | What it does, in its own words: the heart of its prompt. Say what it works on, how, and what it brings back. **Needed to run.** |
| `model` | text | `"sonnet"` | `"fable"`, `"opus"`, `"sonnet"` or `"haiku"`: Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5. |
| `effort` | text | `"medium"` | `"low"`, `"medium"`, `"high"`, `"xhigh"` or `"max"`. |
| `budget` | whole number | 80 | Tool calls it's asked to stay within each session. Once a session has used its own and its team's budgets, its Lab calls are refused and it wraps up. |
| `spend_cap` | number | 10 | The most one of its sessions may spend, in dollars at API prices. It only matters for an agent that reports to no one: a member spends from its lead's session. |
| `memory` | true or false | true | Whether it keeps a memory from one run to the next. |
| `app_notes` | true or false | true | Whether it writes bugs and ideas for Persephone. |
| `uses_lab` | true or false | true | Whether it has Lab's tools. Without them, it works only from files and what it's handed. |
| `reports_to` | text or null | null | Its lead's name. They run as one session: the lead hands it work and checks what comes back. Null for an agent at the top. |
| `at` | `[x, y]` | laid out | Where its card's centre sits. Leave it out on every agent and the cards are laid out as a tree, leads above their teams. |

### Each link

| Key | Kind | Default | What it is |
|---|---|---|---|
| `from` | text | **required** | An agent's name. |
| `to` | text | **required** | An agent's name. |
| `kind` | text | **required** | `"hands_to"`: `to` works from what `from` found. `"reviews"`: `from` re-runs `to`'s claims in Lab, and `to` answers. `"reports_to"` works too, but `reports_to` on the agent says it more plainly. |
| `rounds` | whole number | 1 | For `reviews`: 1 to 5 rounds of review and answer. |

## What makes a team runnable

An import succeeds even when the team can't run yet: the app's **Check**, or `check_team`, says
what to fix. To be runnable at once:

- **The brief** says what to study.
- **Every agent** has a role. Every agent at the top has a `spend_cap` above 0, and every budget is
  at least 1.
- **Leads:** an agent has at most one lead, and leads don't loop: no A → B → A.
- **Hand-offs and reviews join agents in the same place:** two agents with the same lead, or two
  agents that report to no one. They don't join a lead and its own team.
- **Hand-offs don't loop.** The only loops are reviews, and each has its rounds.
- **A reviewer between sessions isn't reviewed itself.** Two agents don't review each other, and a
  pair is joined by a hand-off or a review, not both.
- **On this computer:** Claude Code is installed and logged in, and Lab is built, if any agent uses
  Lab.

## How it will run

- **Sessions:** each agent that reports to no one runs as one Claude Code session in its own
  sandboxed folder: Lab, its own files, no shell, no web. Its team runs inside that session, and the
  lead hands each member work. Members can have teams of their own.
- **Inside a session,** `hands_to` and `reviews` between agents with the same lead become what the
  lead is told, such as "give B what A found" or "have C re-run B's results".
- **Between sessions,** they set the order. A hand-off starts the second session after the first,
  with its report. A review runs the reviewer on the author's report, then the author on the
  review, for each round.
- **The most a run can spend** is the `spend_cap` of every step's session, added up: a reviewed
  agent's session runs once for its work and once more for each round's answer. On a Claude
  subscription, the figures are API-price equivalents that count against your plan's usage limits,
  not charges.

**Choosing models:**
- Leads and reviewers, who judge, do well on Opus or Fable.
- Analysts, who mostly call Lab, do well on Sonnet or Haiku.
- Bigger teams and higher effort use more of your allowance.

## An example

[examples/crypto-research-desk.json](examples/crypto-research-desk.json) is a crypto research desk:
- Head, on Opus, leads two Sonnet analysts in one session.
- Skeptic, in a session of its own, reviews the head's report for one round.

It leaves out every `at`, so its cards are laid out as a tree. It imports ready to run, which the
tests check:

```json
{
  "format": "persephone-agents/1",
  "name": "Crypto research desk",
  "brief": "Is there anything worth trading in BTCUSDT and ETHUSDT, spot or perpetual, on 1h or 4h bars, after realistic costs? ...",
  "at_once": 2,
  "lab": {"data": "own"},
  "agents": [
    {"name": "Head", "role": "Leads the research. ...", "model": "opus", "effort": "high", "budget": 60, "spend_cap": 15},
    {"name": "Trend", "role": "Tests trend-following rules ...", "model": "sonnet", "budget": 80, "reports_to": "Head"},
    {"name": "Carry", "role": "Tests funding-rate and basis ideas ...", "model": "sonnet", "budget": 80, "reports_to": "Head"},
    {"name": "Skeptic", "role": "Checks the head's report by re-running its claims ...", "model": "sonnet", "effort": "high", "budget": 60, "spend_cap": 8, "app_notes": false}
  ],
  "links": [{"from": "Skeptic", "to": "Head", "kind": "reviews", "rounds": 1}]
}
```
