# 🦀 Token Tracker

Track token usage and API-equivalent costs across Pi, Codex, Claude Code, and
Hermes from one Rust CLI.

- Filter reports by agent, provider, model, or time period.
- Persistent local usage history.
- Optional server for totals across machines.

## Quick start

Requires a Unix system and Rust 1.85+ to build. Make sure `~/.cargo/bin` is on your
`PATH`.

```sh
git clone https://github.com/luojam/token-tracker.git
cd token-tracker
cargo install --path .
token-tracker
```

Running without arguments imports new or changed sessions and shows an all-time
usage report. No server or configuration is needed.

## Usage

Local reports refresh session data automatically.

```sh
token-tracker          # All-time report
token-tracker day      # Today
token-tracker week     # This week
token-tracker month    # This month
token-tracker summary  # Token and cost totals only
```

Filter by agent, provider, or model:

```sh
token-tracker week --agent codex
token-tracker month --agent claude --agent pi
token-tracker --provider openai
token-tracker --model gpt-5
```

With a [server configured](docs/deployment.md#connect-clients):

```sh
token-tracker upload         # Upload this machine's usage
token-tracker --server       # Combined totals across machines
token-tracker week --server  # Combined totals for this week
```

Periods use UTC; weeks start Monday. See the [CLI guide](docs/cli.md)
for all options.

### Costs

Costs use recorded values where available and API-price estimates otherwise.

## Local data

Token Tracker reads existing session data; no agent plugins or API keys are
needed.

| Agent | Default session location |
| --- | --- |
| Pi | `~/.pi/agent/sessions` |
| Codex | `~/.codex/sessions` and `~/.codex/archived_sessions` |
| Claude Code | `~/.claude/projects` |
| Hermes | `~/.hermes/state.db` and `~/.hermes/profiles/*/state.db` |

Custom agent directories are also supported; see
[source locations and overrides](docs/cli.md#sources-and-local-data).

| Token Tracker file | Default location |
| --- | --- |
| Usage database | `~/.local/share/token-tracker/usage.db` |
| Machine identity | `~/.local/share/token-tracker/machine-state.db` |
| Configuration | `~/.config/token-tracker/config.toml` |

## Server

The optional self-hosted server collects usage from multiple machines and reports
combined totals. Run `token-tracker upload` on each machine to send its latest
usage, then use `--server` to query the totals.

See [deployment and client setup](docs/deployment.md) for setup instructions.

## Documentation

- [CLI](docs/cli.md): commands, options, filters, configuration, and local data.
- [Server](docs/server.md): behavior, runtime configuration, and API.
- [Deployment](docs/deployment.md): AWS setup, client onboarding, updates, and troubleshooting.
- [Development](docs/development.md): building, checks, and test conventions.
