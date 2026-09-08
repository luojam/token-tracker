# Token Tracker

A local CLI for tracking token usage and recorded/estimated costs.

## Usage

Requires a Unix system, Rust 1.85+. Install from this checkout:

```sh
cargo install --path .
token-tracker
```

Or run without installing:

```sh
cargo run --quiet
```

Each run imports new or changed Pi and Codex sessions. Tracks input, output, and
cache tokens, counting shared fork history only once.

Codex cost estimates use bundled API prices and are shown separately from recorded
costs. They may cover only part of your usage and do not reflect subscription bills.

## Local data

Sessions are read from:

- `~/.pi/agent/sessions`, respecting Pi's directory overrides.
- `~/.codex/sessions` and `~/.codex/archived_sessions`, respecting `CODEX_HOME`.

Usage is stored in SQLite at `~/.local/share/token-tracker/usage.db` (or under
`XDG_DATA_HOME` when set to an absolute path).

Only usage and session metadata are stored. Imported usage is kept even after
session files are deleted.

## Development

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo check
cargo test
```
