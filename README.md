# Token Tracker

A local CLI for tracking token usage and recorded/estimated costs.

## Usage

Requires a Unix system. Building from source requires Rust 1.85+. Make sure `~/.cargo/bin` is on your `PATH`.

From the repository directory, install the executable into `~/.cargo/bin` and run with:

```sh
cargo install --path .
token-tracker
```

Or run from the repository without installing:

```sh
cargo run --quiet
```

To build a release binary at `target/release/token-tracker`:

```sh
cargo build --release
```

### SQLite export

Export all locally retained usage to a SQLite database:

```sh
token-tracker export export.db
token-tracker export export.db --force
```

Run `token-tracker` first to import current usage; export does not refresh sources.
The parent directory must exist. Use `--force` to replace an existing export;
unrelated files are never overwritten.

### Server upload

Upload all locally retained usage to a server:

```sh
token-tracker upload https://tracker.example.com --auth-file /path/to/auth.token
```

Use the server's token in a file with permissions `600`. HTTPS is required except
on loopback. Run `token-tracker` first to import current usage; upload does not
refresh sources.

### Configuration

Optionally create `~/.config/token-tracker/config.toml` (or
`$XDG_CONFIG_HOME/token-tracker/config.toml` when `XDG_CONFIG_HOME` is an absolute
path):

```toml
machine_name = "my-computer"
```

`machine_name` is an optional display name included in exports; it does not change
machine identity. Missing files or an omitted name use the default (no name).
Invalid TOML, unknown options, and unreadable files produce an error. The CLI
never creates or rewrites this file.

### Pi

### Codex

Codex cost estimates use bundled API prices. Unknown service tiers use normal
(standard) rates. Missing cache-write counts are priced as ordinary input, which
can underestimate cost slightly.

Supported models include `gpt-6-astra`, `gpt-5.6-sol`, `gpt-5.6-terra`,
`gpt-5.6-luna`, `gpt-5.5`, and `gpt-5.4-mini`, with Standard and Fast pricing.

### Claude Code

Claude Code estimates use bundled API prices.

Supported models are `claude-opus-5` (Standard and Fast), plus `claude-fable-5`,
`claude-fable-5-1`, `claude-sonnet-5`, and `claude-haiku-4-5-20251001` (Standard).

### Hermes

Requires modern SQLite accounting with `sessions` and `session_model_usage`
tables (version-30 shape). Includes auxiliary tasks; events count accounting
buckets rather than API calls.

Subscription costs use API-equivalent estimates. OpenAI aggregates assume
short-context rates, which may underestimate long-context usage.

## Local data

Running without arguments imports new or changed Pi, Codex, Claude Code, and Hermes sessions.
Tracks input, output, and cache tokens, counting shared fork history only once.

Sessions are read from:

- `~/.pi/agent/sessions`, respecting Pi's directory overrides.
- `~/.codex/sessions` and `~/.codex/archived_sessions`, respecting `CODEX_HOME`.
- `~/.claude/projects`, or `$CLAUDE_CONFIG_DIR/projects`.
- `~/.hermes/state.db` and `~/.hermes/profiles/*/state.db`, or
  `$HERMES_HOME/state.db` when set.

Usage is stored in SQLite at `~/.local/share/token-tracker/usage.db` (or under
`XDG_DATA_HOME` when set to an absolute path).

Only usage and session metadata are stored. Imported usage is kept even after
session files or Hermes sessions/databases are deleted.

## Server (in progress...) 

Run the server on `127.0.0.1:3000`:

```sh
cargo run --features server --bin token-tracker-server
```

Create `/etc/token-tracker/auth.token` (or the path set by `TOKEN_TRACKER_SERVER_AUTH_FILE`)
with a random token (`openssl rand -hex 32`),
owned by the server user with permissions `600`. Restart after changing it.

`POST /snapshots` accepts [snapshot JSON](tests/fixtures/export-example.json) with
`Content-Type: application/json` and `Authorization: Bearer <token>`.
Duplicate uploads succeed; stale or conflicting revisions return HTTP 409.
`GET /health` is public.

`GET /summary` requires the same bearer token and returns all-time totals across
the latest uploaded snapshot from every machine:

```json
{
  "total_cost_usd": "123.456",
  "tokens": {
    "input": 1000,
    "output": 200,
    "cache_write": 300,
    "cache_read": 400
  }
}
```

| Environment variable | Default |
| --- | --- |
| `TOKEN_TRACKER_SERVER_AUTH_FILE` | `/etc/token-tracker/auth.token` |
| `TOKEN_TRACKER_SERVER_DATABASE` | `/var/lib/token-tracker/snapshots.db` |
| `TOKEN_TRACKER_MAX_UPLOAD_BYTES` | `33554432` (32 MiB) |

The database directory must exist and be writable by the server user.

### Musl build for EC2 Amazon Linux compatibility

Install requirements:

```sh
sudo dnf install musl-gcc
rustup target add x86_64-unknown-linux-musl
```

Build the compatible binary locally before moving it onto the EC2 instance:

```sh
CC_x86_64_unknown_linux_musl=musl-gcc \
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc \
cargo build --locked --release \
  --target x86_64-unknown-linux-musl \
  --features server \
  --bin token-tracker-server
```

### Copy the binary onto ec2

Make sure it is executable:

```sh
chmod +x /tmp/token-tracker-server
```

Run from `/tmp/` during dev:

```sh
nohup /tmp/token-tracker-server > /tmp/token-tracker-server.log 2>&1 < /dev/null &
```

## Infra

`infra/main.tf` uses Terraform to define the EC2 instance, networking,
SSM access, and an encrypted 10 GB data volume mounted at `/var/lib/token-tracker`
by the startup script. Caddy was installed manually on EC2.

## Development

See [tests/README.md](tests/README.md) for test layout and conventions.

```sh
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo check
cargo test
```
