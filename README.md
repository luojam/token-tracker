# Token Tracker

A local CLI for tracking Pi token usage and recorded costs, with an all-time
terminal summary grouped by provider and model.

## Usage

Requires Rust 1.85+ and Pi v3 session files. Install from this checkout:

```sh
cargo install --path .
token-tracker
```

Or run without installing:

```sh
cargo run --quiet
```

Each run imports new or changed sessions. Tracks input, output, and cache tokens,
counting shared fork history only once.

## Local data

Sessions are read from `~/.pi/agent/sessions`, respecting Pi's directory overrides.
Usage is stored in SQLite at `~/.local/share/token-tracker/usage.db` (or under
`XDG_DATA_HOME` when set to an absolute path).

Only usage and session metadata are stored—not conversation content. Imported
usage is kept even after session files are deleted.

## Development

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```
