# Server

The optional server collects uploaded snapshots and reports totals across the
latest snapshot from each machine.

## Deployment

[scripts/deploy.py](../scripts/deploy.py) builds and deploys the server to AWS EC2
through SSM. [Terraform](../infra/main.tf) provisions the infrastructure and auth
token; deployment configures storage, services, and HTTPS through Caddy.

Requires Python 3, Terraform, a configured AWS CLI, `musl-gcc`, and the Rust
`x86_64-unknown-linux-musl` target. Run from the repository directory:

```sh
python3 scripts/deploy.py tracker.example.com --provision
```

Point the domain's DNS to Terraform's `public_ip` output for HTTPS to work.
For subsequent deployments, omit `--provision`.

## Upload

Save the generated token from AWS SSM Parameter Store
(`/token-tracker/auth-token`) to a local file on each machine. The file must be
accessible only by its owner:

```sh
chmod 600 /path/to/auth.token
token-tracker
token-tracker upload https://tracker.example.com --auth-file /path/to/auth.token
```

Run `token-tracker` first to import current usage; upload includes all locally
retained usage but does not refresh sources. HTTPS is required except on loopback.

## API

- `POST /snapshots` accepts [snapshot JSON](../tests/fixtures/export-example.json)
  with `Content-Type: application/json` and `Authorization: Bearer <token>`.
  Duplicate uploads succeed; stale or conflicting revisions return HTTP 409.
- `GET /summary` requires the same bearer token and returns all-time cost and
  token totals across machines.
- `GET /health` is public.
