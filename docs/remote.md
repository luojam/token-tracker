# Remote usage

Each machine uploads a snapshot of daily usage to a private R2 bucket. Reports
combine snapshots locally; machines must not share sessions. Snapshots contain
tokens, costs, pricing versions, and a machine label, without session names,
paths, messages, or event IDs.

## Setup

Requires Node.js 22+ and a Cloudflare account with R2 enabled. From `worker/`:

```sh
npm ci
npx wrangler login
npx wrangler r2 bucket create token-tracker-snapshots
npm run deploy
npx wrangler secret put SYNC_TOKEN
```

Keep the bucket private. Generate a token with `openssl rand -hex 32` and use it
for the Worker's `SYNC_TOKEN` and each machine's
`~/.config/token-tracker/auth.toml`:

```toml
remote_token = "your-token"
```

Restrict access with `chmod 600 ~/.config/token-tracker/auth.toml`. An absolute
`XDG_CONFIG_HOME` replaces `~/.config`. In `config.toml` beside it, set:

```toml
machine_name = "laptop"
remote_url = "https://token-tracker-remote.<account>.workers.dev"
```

```sh
token-tracker sync
token-tracker --remote
token-tracker week --remote --agent codex
```

Only `sync` uploads. It refreshes local sources and replaces the machine's
snapshot, including historical corrections. Preserve `machine-state.db` to keep
the same identity. After a conflict or uncertain upload, rerun `sync` to read the
remote state and rebuild the snapshot.

Remote reports use uploaded data without refreshing local sources. They show
snapshot ages and partial pricing, count each machine once, and omit session/event
totals. Downloads are cached by remote URL in `remote-cache/` beside `usage.db`.
Failed refreshes return an error instead of a stale report.

## Local testing

Set `SYNC_TOKEN=<token>` in `worker/.dev.vars`, run
`npm run dev`, and set `remote_url = "http://localhost:8787"`.
