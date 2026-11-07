# Persephone docs

## [The app](app/)

- [User guide](app/guide.md): charts, tiles and workspaces, strategies and backtests, the backtest
  panel and its tests, building and optimizing strategies, your own data, paper and live trading,
  news, and where files live.
- [Writing plugins](app/plugins.md): indicators and strategies in Rust, models and AI.

## [Persephone Lab](lab/)

Everything in Persephone as tools, for agents and scripts.

- [Lab](lab/README.md): connecting over MCP, HTTP or the command line, and what it can and can't do.
- [Every tool](lab/tools.md), generated from the tools themselves.
- [The agents' guide](lab/agents-guide.md): what Lab serves agents through `get_guide`.
- [External models](lab/external-models.md): models that run outside Persephone.

## [Agent teams](agents/)

Teams of Claude agents, designed in the app's Agents view, run as sandboxed Claude Code sessions.

- [Agents](agents/README.md): drawing a team, how it runs, watching it, its folders and sandbox,
  costs, and control from another AI.
- [Setup files](agents/setup.md): a whole team as one JSON file, for another AI to write. With
  [an example](agents/examples/crypto-research-desk.json).
- [The controls' tools](agents/tools.md), generated from the tools themselves.

## [The project](project/)

- [Design plan](project/plan.md): architecture, and the decisions behind it.
- [Running an agent trial](project/agent-trial.md): an AI agent researching with Lab, blind to the
  code, and reporting what's wrong.

`images/` holds the pictures the docs show.
