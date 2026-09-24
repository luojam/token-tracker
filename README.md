# 🦀 Token Tracker

Track token usage and API-equivalent costs across Pi, Codex, Claude Code, and
Hermes from one Rust CLI.

- Track input, output, and cache tokens without double-counting shared fork history.
- Keep usage history locally, even after deleting source sessions.
- Export usage to SQLite for your own queries.
- Optionally combine totals from multiple machines with a self-hosted server.

## Quick start

Requires a Unix system and Rust 1.85+ to build. Make sure `~/.cargo/bin` is on your
`PATH`.

```sh
git clone https://github.com/luojam/token-tracker.git
cd token-tracker
cargo install --path .
token-tracker
```

Running without arguments imports new or changed sessions and shows a usage
report. No server is needed.

### Configuration

Optionally create `~/.config/token-tracker/config.toml` (or
`$XDG_CONFIG_HOME/token-tracker/config.toml` when `XDG_CONFIG_HOME` is absolute):

```toml
machine_name = "my-computer"
```

This display name is included in exports and does not change machine identity.
The default is no name.

## Sources and local data

| Source | Session location |
| --- | --- |
| Pi | `~/.pi/agent/sessions`, respecting Pi's directory overrides |
| Codex | `~/.codex/sessions` and `~/.codex/archived_sessions`, respecting `CODEX_HOME` |
| Claude Code | `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects` |
| Hermes | `~/.hermes/state.db` and `~/.hermes/profiles/*/state.db`, or `$HERMES_HOME/state.db` when set |

Usage is stored at `~/.local/share/token-tracker/usage.db` (or under an absolute
`XDG_DATA_HOME`). Only usage and session metadata are stored, and imported usage
is retained after source sessions or databases are deleted.

### Cost estimates

Costs use recorded values where available and API-price estimates otherwise.
For subscription plans, these are API-equivalent costs, not your subscription bill.

### SQLite export

Export all locally retained usage:

```sh
token-tracker export export.db
```

Run `token-tracker` first to import current usage; export does not refresh sources.
The parent directory must exist. Add `--force` to replace an existing export;
unrelated files are never overwritten.

## Optional server: combine usage across machines

Upload usage to a self-hosted server for combined totals across the latest
snapshot from each machine. Uploads are explicit, not automatic.

Follow the [server guide](docs/server.md) to deploy and get an auth token, then
import and upload from each machine:

```sh
chmod 600 /path/to/auth.token
token-tracker
token-tracker upload https://tracker.example.com --auth-file /path/to/auth.token
```

Uploads include all locally retained usage without refreshing sources. HTTPS is
required except on loopback.

## Development

Run locally with `cargo run`, or build a release binary at
`target/release/token-tracker` with `cargo build --release`.

See [tests/README.md](tests/README.md) for test layout and conventions.

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo check
cargo test
```
