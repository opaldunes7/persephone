# Agents

Teams of Claude agents, drawn as cards on a canvas in the app, run as Claude Code sessions, and
watched while they work. They research through [Persephone Lab](../lab/README.md), and they write down what's
broken in Persephone and what would make it better.

- [Opening it](#opening-it)
- [What it needs](#what-it-needs)
- [Drawing a team](#drawing-a-team)
- [How a team runs](#how-a-team-runs)
- [Watching a run](#watching-a-run)
- [The folders](#the-folders)
- [The sandbox](#the-sandbox)
- [Costs](#costs)
- [From another AI](#from-another-ai)

## Opening it

Press **Agents** in the top bar, press Ctrl+G, or pick **Agents** from a chart's right-click menu. The
view covers everything under the top bar, and the charts keep running underneath. Close it with
**✕**, Ctrl+G again, or any test tab.

## What it needs

- **Claude Code, logged in.** The app runs your `claude`, which uses your own login, so the app never
  sees a key. If it says Claude Code isn't logged in, run `claude auth login` in a terminal.
- **Lab, built.** Run `cargo build --release -p persephone-lab`. The app looks beside its own program,
  then in the workspace's release build.

You can give either path yourself: Claude Code's under **This computer** in the team's setup, and
Lab's on the Lab card.

## Drawing a team

**Teams** are down the left. **New team** starts one blank or from a starter:

| Starter | What's in it |
|---|---|
| One analyst | One agent: a quant analyst and beta user of Persephone, as in the agent kit |
| Lead and analysts | An Opus lead with two Sonnet analysts, in one session |
| Researcher and reviewer | Two sessions: a researcher, and a reviewer that re-runs its claims, for one round |

**Setup files:** **Import…** makes a team from a JSON file, and **Export…** saves the team on the canvas as one, so a setup that works can be kept, shared, or written by another AI. See [Agent setup files](setup.md).

**Duplicate** copies a design with new agents, so the copy starts with no memory. **Archive** moves a
team's folder out of the way. Nothing is ever deleted.

**On the canvas:**

- Drag a card to move it.
- Drag from a card's dot to another card to link them, as the kind picked under **Link as**.
- Double-click empty space to add an agent there, or press **+ Agent**.
- Drag empty space to move around, and scroll to zoom.
- Click a card or a link to select it. **Delete** takes off what's selected.

**Kinds of link:**

| Link | Drawn | What it does |
|---|---|---|
| Reports to | solid, up to the lead | The lead hands it work and checks what comes back |
| Hands to | dashed arrow | The second agent works from the first one's results |
| Reviews | dotted, ×N | It re-runs the other's claims in Lab, and the other answers; N rounds, 1 to 5 |
| Uses | faint, to the Lab card | It has Lab's tools. Without this link, it only reads and writes files |

An agent has one lead, so a second **Reports to** from it replaces the first.

**The inspector** on the right shows what's selected:

- **An agent:** its name and role, its model (Fable 5.1, Opus 5.5, Sonnet 5.5 or Haiku 5.5) and
  effort, the tool calls it's asked to stay within each session, and a spend cap per session. It also
  says whether it uses Lab, keeps a memory between runs, and writes bugs and ideas for the app.
- **A link:** its kind, the rounds of a review, and **Reverse it**.
- **The Lab card:** where Lab's program is, and whose data it works on. It can have a folder of its
  own (`~/persephone-agent-data` to start with), so the agents' runs don't mix with yours. Or it can
  use your app's data, so what they save shows in your saved results.
- **The team** (nothing selected): its name, its **brief** (what to study), its **principles** (how
  every agent works), how many sessions may run at once, and where Claude Code and the teams are.
  The principles start as the agent kit's method-only list: count every variant, keep costs on,
  compare with holding, and say "nothing worth trading" when that's the truth.

**Check** lists what's wrong with the design, and anything missing on this computer. Click a problem
to select its card. **Run** won't start until the list is empty.

## How a team runs

**Sessions.** Each agent that reports to no one is a session: one `claude -p` in its own folder.

- The agents under it run inside that session as Claude Code sub-agents, nested as deep as you drew
  them.
- Each agent may call only its own reports, so Claude Code's built-in helper agents can't be called.
- Everyone in a session shares one Lab: if any of them uses Lab, the session's top agent can reach
  it too.

**Links inside a session** join agents with the same lead. They become what that lead is told, such
as "give B what A found" or "have C re-run B's results before you accept them".

**Links between sessions** join agents that report to no one, and become the run's steps:

- **Hands to:** B starts after A, with A's report and the rest of its `outbox/` copied into B's
  `inbox/<run>/<a>/`.
- **Reviews:** the reviewer runs on the author's report. Then the author carries on its own
  conversation (`claude --resume`) to answer the review. That repeats for the link's rounds. These
  are the only loops, so every run ends.
- **Order:** sessions with nothing between them run side by side, as many at once as the team
  allows. When a session fails, what waits on it is skipped, and the rest carry on.

**Budgets:**

- Each agent's tool-call budget is in its prompt.
- **Wrap up:** when a session has used its budget (its own and its team's), or you press **Wrap up**,
  its Lab and Agent calls are refused with "Your budget is used". It can still write its journal and
  memory, which is how it ends cleanly.
- **Hard limits:**
  - each session's spend cap, which Claude Code enforces
  - each member's limit on turns
  - a session that keeps going well past its budget is stopped
- **Stop** ends every session at once, with everything it started, Lab included.
- **A run goes on when the window closes.** It runs in a process of its own (`persephone-agents`,
  built with `cargo build --release -p persephone-agents-server`), so quitting the app, or a tool
  that closes windows, doesn't end it. Open the Agents view again to see it live, and press Stop to
  end it. Without `persephone-agents` built, a run goes on inside the app instead, and closing the
  window stops it first.

## Watching a run

- **Cards update live:**
  - The bar down a card's left edge says how it's doing: blue running, amber wrapping up, green done
    and red failed.
  - Its bottom line says what it's doing, its calls out of its budget, and what it has spent.
  - A member's card lights up when its lead hands it work, and its calls count on its own card.
  - The Lab card counts Lab's calls.
- **Activity:** what was said, each tool call with its main arguments, the answers, refusals, and
  work handed out and brought back. It shows the selected agent's, or everyone's with nothing
  selected.
- **Outputs:** the selected agent's report, newest journal, beliefs, bugs and ideas, plus **Open
  folder**. Its `vetoed.md` and `tasks.md` are yours to edit there.
- **App feedback:** every agent's bugs and ideas for Persephone, in one place.
- **Past runs:** pick one at the right of the run bar to read its activity again.

## The folders

Teams are kept in `~/persephone-agents`, unless you say otherwise under **This computer**:

| Path | What |
|---|---|
| `<team>/team.json` | the design |
| `<team>/runs/<run>/` | a run's record: `summary.json`, each step's whole stream (`.jsonl`), and each step's outbox |
| `<team>/<agent>/` | the folder of an agent that reports to no one |

**An agent's folder** is laid out as the agent kit's:

- `CLAUDE.md`, written from its role, its team and its links
- `principles.md`, `subject.md` (the brief) and `vetoed.md`
- `tasks.md`, `memory/`, `app/`, `runs/`, `inbox/` and `outbox/`
- `.claude/settings.json` (its sandbox), `.mcp.json` (its Lab) and `bin/commit.sh`

It's named by a fixed id, so renaming the agent keeps its memory. Its members keep their notes in
`memory/team/<name>.md`.

**What's whose:**

- The app rewrites only its own files: `CLAUDE.md`, `principles.md`, `subject.md`, `.claude/`,
  `.mcp.json`, `bin/` and `.gitignore`.
- The agent's own files are made only when they're missing, so its memory, notes and runs are never
  overwritten.
- Taking an agent off the canvas moves its folder to `~/persephone-agents/archive/`.

**Git and your veto:**

- Each agent's folder is its own git repository, committed after every session.
- `git log` there shows how it learns. To veto something, `git revert` it and add a line to
  `vetoed.md` saying why, so it doesn't relearn it.
- You can also talk to an agent yourself: run `claude` in its folder.

## The sandbox

Every agent gets the agent kit's sandbox, through Claude Code's permissions:

- **Allowed:**
  - Lab, if it uses it
  - reading and editing inside its own folder
  - searching its folder
  - calling its own reports
- **Denied:**
  - a shell and the web
  - Persephone's code
  - `~/.ssh`, `~/.claude`, `~/Library`, `~/Documents`, `~/Desktop` and `~/Downloads`
  - Lab's data folder, so it can't read other agents' calls
  - reading its own `.git`, and changing its `.claude`, `bin` or `inbox/`
  - editing `CLAUDE.md`, `principles.md`, `subject.md` and `vetoed.md`
  - the other agents' folders, and other teams', so hand-offs are the only way between them

The app passes the allowed tools on the command line, so no folder needs trusting first. Lab itself
can't edit code, place orders, or delete your data. Answers too big to send are saved in the agent's
own `lab-answers/`, where it can read them.

It isn't a container: anything outside the folder that isn't denied can still be read. For hard
isolation, run the app as a separate macOS user.

## Costs

Spend is in dollars at API prices, as Claude Code counts it. **Run** shows the most a run can spend:
every step's session cap, added up. A member spends from its lead's session, inside that cap.

## From another AI

Another program or AI can do everything the view does: build a team, change it, check and run it,
watch it, stop it, and read what its agents wrote. They work on the same files, so a change made
from outside shows in the view at once, and a run started from outside lights up the cards there.
It works the other way too: a run started in the app can be watched and stopped from outside.

The controls are `persephone-agents`, a program of their own:

```bash
cargo build --release -p persephone-agents-server
```

That gives `target/release/persephone-agents`, with three ways in:

- **MCP, for AI clients.** Its 24 tools then show up as the client's own. For Claude Code:

  ```bash
  claude mcp add persephone-agents -- /full/path/to/persephone-agents mcp
  ```

- **HTTP, for scripts and custom agents.** Run `persephone-agents serve`, which listens on
  `http://127.0.0.1:8766`. Send `POST /v1/tools/<name>` with the arguments as JSON, or
  `GET /v1/tools` for the list. Each call needs `Authorization: Bearer <token>`, with the token in
  `~/persephone-agents/http-token`. It only answers this machine, and not web pages.
- **The command line:** `persephone-agents call <tool> '<arguments as JSON>'`. Add `--root DIR`
  first to use another teams' folder.

One call can build a whole team:

```bash
persephone-agents call create_team '{
  "name": "BTC desk",
  "brief": "Is there anything worth trading in BTCUSDT on 1h bars, after realistic costs?",
  "agents": [
    {"name": "Head", "role": "Leads the research and writes the report.", "model": "opus", "effort": "high", "spend_cap": 15},
    {"name": "Trend", "role": "Tests trend rules through Lab.", "model": "sonnet", "reports_to": "Head"},
    {"name": "Skeptic", "role": "Re-runs the head's claims with tests that could break them.", "model": "sonnet"}
  ],
  "links": [{"from": "Skeptic", "to": "Head", "kind": "reviews", "rounds": 1}]
}'
```

Then `check_team`, `run_team`, `get_run` while it goes, and `get_outputs` and `get_feedback` for
what the agents wrote. `get_guide` explains the rest, and [the tools' reference](tools.md)
lists every tool and its arguments.

**What it can and can't do:**

- `run_team` starts a run in the background and answers at once. The run goes on even if what
  asked for it stops.
- Only one run of a team goes at a time.
- Each run keeps `plan.json` and a live `state.json` in its folder. Leaving a `stop` or `wrap-up`
  file there asks it to stop or wrap up, which is how `stop_run`, `wrap_up_run` and the app's
  buttons reach a run in another process.
- The agents in a team can't reach any of this. They have no shell and no web, Lab is their only
  tools server, and their sandbox keeps them out of the teams' folder.

