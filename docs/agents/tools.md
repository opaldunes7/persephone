# Persephone agents' controls

Generated from the tools themselves: `persephone-agents tools --markdown`.

## `get_guide`

How teams work and how to use these tools. Read it first.

No arguments.

## `list_teams`

Every team: id, name, its agents, and a run that's going, if any.

No arguments.

## `get_team`

A team's whole design: brief, principles, agents, links and Lab, how it would run, the most a run can spend, and what Check finds wrong with it.

- `team` (string, required): The team, by its id or name.

## `create_team`

Makes a team, from a starter or built whole in one call: give `agents` and `links` and it's laid out on the canvas, ready to check and run. It shows in the app's Agents view at once.

- `name` (string, required): The team's name.
- `starter` (string): blank (the default), one_analyst, lead_and_analysts or researcher_and_reviewer.
- `brief` (string): What the team should study.
- `principles` (string): How every agent works: method, not market views. Leave out for the agent kit's.
- `at_once` (integer): How many sessions may run at once, 1 to 4.
- `agents` (array): Agents to add, each an object: name (required), role, model, effort, budget, spend_cap, memory, app_notes, uses_lab (true unless said), reports_to (another's name).
- `links` (array): Links between them, each an object: from, to (names), kind (hands_to or reviews; reports_to too), and rounds for reviews.

## `update_team`

Changes a team's name, brief, principles or how many sessions run at once.

- `team` (string, required): The team, by its id or name.
- `name` (string): A new name.
- `brief` (string): What the team should study.
- `principles` (string): How every agent works.
- `at_once` (integer): How many sessions may run at once, 1 to 4.

## `duplicate_team`

Copies a team's design, with new agents that start with no memory.

- `team` (string, required): The team, by its id or name.
- `name` (string): The copy's name.

## `archive_team`

Moves a team's folder to the archive. Nothing is deleted.

- `team` (string, required): The team, by its id or name.

## `add_agent`

Adds an agent to a team. Its card goes under its lead, or beside the others.

- `team` (string, required): The team, by its id or name.
- `name` (string, required): Its name.
- `role` (string): What it does, in its own words: the heart of its prompt.
- `model` (string): fable, opus, sonnet or haiku (Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5).
- `effort` (string): low, medium, high, xhigh or max.
- `budget` (integer): Tool calls it's asked to stay within each session.
- `spend_cap` (number): The most one of its sessions may spend, in dollars at API prices. Only for an agent that reports to no one.
- `memory` (boolean): Whether it keeps a memory from one run to the next.
- `app_notes` (boolean): Whether it writes bugs and ideas for the app.
- `uses_lab` (boolean): Whether it has Lab's tools.
- `reports_to` (string_or_null): Its lead, by name or id: they run as one session, the lead handing it work. Null or empty for none.
- `at` (array): Where its card's centre is on the canvas, [x, y].

## `update_agent`

Changes an agent: any of its fields, its name, or its lead.

- `team` (string, required): The team, by its id or name.
- `agent` (string, required): The agent, by its name or id.
- `name` (string): A new name. Its folder and memory stay.
- `role` (string): What it does, in its own words: the heart of its prompt.
- `model` (string): fable, opus, sonnet or haiku (Fable 5.1, Opus 5.5, Sonnet 5.5, Haiku 5.5).
- `effort` (string): low, medium, high, xhigh or max.
- `budget` (integer): Tool calls it's asked to stay within each session.
- `spend_cap` (number): The most one of its sessions may spend, in dollars at API prices. Only for an agent that reports to no one.
- `memory` (boolean): Whether it keeps a memory from one run to the next.
- `app_notes` (boolean): Whether it writes bugs and ideas for the app.
- `uses_lab` (boolean): Whether it has Lab's tools.
- `reports_to` (string_or_null): Its lead, by name or id: they run as one session, the lead handing it work. Null or empty for none.
- `at` (array): Where its card's centre is on the canvas, [x, y].

## `remove_agent`

Takes an agent off a team, with its links. Its folder, if it had one, goes to the archive.

- `team` (string, required): The team, by its id or name.
- `agent` (string, required): The agent, by its name or id.

## `link`

Links two agents. reports_to: `from` joins `to`'s team, replacing any lead it had. hands_to: `to` works from what `from` found. reviews: `from` re-runs `to`'s claims and `to` answers, for `rounds`.

- `team` (string, required): The team, by its id or name.
- `from` (string, required): An agent, by name or id.
- `to` (string, required): An agent, by name or id.
- `kind` (string, required): reports_to, hands_to or reviews.
- `rounds` (integer): For reviews: 1 to 5. Default 1.

## `unlink`

Takes off the links from one agent to another: all of them, or those of a kind.

- `team` (string, required): The team, by its id or name.
- `from` (string, required): An agent, by name or id.
- `to` (string, required): An agent, by name or id, or Lab.
- `kind` (string): reports_to, hands_to, reviews or uses.

## `set_lab`

Says which Lab the team's agents use: its program, and whose data it works on.

- `team` (string, required): The team, by its id or name.
- `program` (string): Lab's program; empty for the one beside the app.
- `data` (string): own (a folder of the agents' own) or app (your app's, so what they save shows there).
- `folder` (string): With own: the folder. Default ~/persephone-agent-data.

## `tidy_layout`

Lays a team's cards out as a tree: leads above their teams, sessions side by side, Lab below.

- `team` (string, required): The team, by its id or name.

## `check_team`

What's wrong with a team before it runs, and whether this computer has what it needs (Claude Code logged in, Lab built).

- `team` (string, required): The team, by its id or name.

## `run_team`

Starts a run of a team in the background, if Check finds nothing wrong, and answers with its name at once. Watch it with get_run.

- `team` (string, required): The team, by its id or name.

## `get_run`

A run as it goes, or as it ended: each step's status, calls, spend and what it's doing, and its latest activity.

- `team` (string, required): The team, by its id or name.
- `run` (string): The run; the latest if left out.
- `lines` (integer): How many of the latest activity lines. Default 30.

## `list_runs`

A team's runs, newest first, with how each ended.

- `team` (string, required): The team, by its id or name.

## `get_activity`

A run's activity, a page at a time: what each agent said, its tool calls and their answers, work handed out and brought back, and refusals.

- `team` (string, required): The team, by its id or name.
- `run` (string, required): The run.
- `agent` (string): Only this agent's, by name or id.
- `from` (integer): The first line to give. Default 0.
- `limit` (integer): The most lines to give. Default 100.

## `stop_run`

Stops a run now, with everything it started.

- `team` (string, required): The team, by its id or name.
- `run` (string): The run; the one going if left out.

## `wrap_up_run`

Has a run's sessions do their end-of-session steps, and start no more.

- `team` (string, required): The team, by its id or name.
- `run` (string): The run; the one going if left out.

## `get_outputs`

What an agent has written: its report, newest journal, beliefs, bugs and ideas, tasks and vetoed list. A member's are its notes.

- `team` (string, required): The team, by its id or name.
- `agent` (string, required): The agent, by its name or id.

## `get_feedback`

Every agent's bugs and ideas for Persephone, in one place.

- `team` (string, required): The team, by its id or name.

## `export_team`

A team's whole setup as a file: its brief, principles, every agent's settings, the links, Lab and the layout, by name. Import it with import_team to get the same setup again, here or on another computer. Agents' memory isn't in it.

- `team` (string, required): The team, by its id or name.
- `path` (string): Also write it to this file.

## `import_team`

Makes a team from a setup file, as export_team writes it or as written by hand or by an AI (docs/agents/setup.md says how). Every key is checked, so a typo is caught rather than ignored. Its agents start with no memory.

- `setup` (object): The setup itself.
- `path` (string): Or a file holding it.
- `name` (string): A name for the team instead of the file's, such as when that one's taken.

## `set_notes`

Writes an agent's tasks.md (jobs for it to work through) or vetoed.md (what you reverted from its memory, and why). Only for an agent that reports to no one.

- `team` (string, required): The team, by its id or name.
- `agent` (string, required): The agent, by its name or id.
- `file` (string, required): tasks or vetoed.
- `text` (string, required): The whole file.
