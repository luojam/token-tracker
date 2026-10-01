# Server

The optional server collects usage snapshots and reports combined totals across
the latest snapshot from each machine. Uploads are explicit: local CLI reports
do not upload, and server reports do not refresh clients.

Each machine has a stable ID and an increasing snapshot version (`export_revision`).
A newer upload replaces that machine's snapshot; retrying the same snapshot succeeds.
Older versions or different snapshots with the same version are rejected.

For AWS setup and client onboarding, see [deployment](deployment.md). For upload
and reporting commands, see the [CLI guide](cli.md).

## Runtime

Build the server with:

```sh
cargo build --release --features server --bin token-tracker-server
```

The binary is `target/release/token-tracker-server`. It listens on the fixed
address `127.0.0.1:3000`; the deployment uses Caddy for public HTTPS.

| Environment variable | Default |
| --- | --- |
| `TOKEN_TRACKER_SERVER_AUTH_FILE` | `/etc/token-tracker/auth.token` |
| `TOKEN_TRACKER_SERVER_DATABASE` | `/var/lib/token-tracker/server/snapshots.db` |
| `TOKEN_TRACKER_MAX_UPLOAD_BYTES` | `33554432` (32 MiB); must be a positive integer |

The process needs a readable token file and writable database storage. It reads
the token at startup, so restart after changing it. The token file must be a
regular file accessible only by its owner. Tokens contain at least 32 ASCII
letters, digits, or `-._~+/=` characters; the file may contain at most 4096 bytes,
including any trailing newline.

All clients share one bearer token and access the same combined dataset.

## API

`POST /snapshots` and `GET /summary` require `Authorization: Bearer <token>`.
Missing or invalid authentication returns HTTP 401. `GET /health` is public.

| Endpoint | Request | Response |
| --- | --- | --- |
| `POST /snapshots` | [Snapshot JSON](../tests/fixtures/export-example.json), with `Content-Type: application/json`. | JSON containing `status` (`published` or `already_published`), `machine_id`, and `export_revision`. |
| `GET /summary` | Optional period and filters below. | JSON cost and token totals across machines. |
| `GET /health` | No parameters. | `ok` followed by a newline. |

Summary parameters:

- `period`: `all_time` (default), `day`, `week`, or `month`. Periods use the current
  UTC calendar; weeks start Monday.
- `agent`, `provider`, `model`: repeatable parameters using the CLI's
  [filter matching rules](cli.md#filters).

Example: `/summary?period=week&agent=codex&agent=pi&provider=openai`.
Unknown parameters, empty values, and repeated `period` return HTTP 400.

Uploads return HTTP 409 for stale or conflicting revisions, 413 for oversized
bodies, 415 for unsupported content types, and 422 for invalid snapshots.
Storage failures return HTTP 500. Error responses contain an `error` field.
