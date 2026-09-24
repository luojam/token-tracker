# Server

The optional server collects uploaded snapshots and reports totals across the
latest snapshot from each machine.

## Deployment

[Terraform](../infra/main.tf) provisions EC2 and an auth token.
[scripts/deploy.py](../scripts/deploy.py) builds and deploys through SSM, with
Caddy providing HTTPS.

Requires Python 3, Terraform 1.11+, AWS CLI v2, `musl-gcc`, and the Rust
`x86_64-unknown-linux-musl` target, plus AWS credentials permitting Terraform,
S3 uploads, and SSM commands. From the repository root:

```sh
terraform -chdir=infra init
terraform -chdir=infra apply
terraform -chdir=infra output -raw public_ip
```

Point your domain's DNS A record at that IP. Once it resolves, deploy:

```sh
python3 scripts/deploy.py tracker.example.com
```

Rerun to update and check HTTPS health. Options:

- `--provision`: apply infrastructure changes first.
- `--release SHA`: reuse an uploaded binary by SHA-256; rollback needs database compatibility.

For Caddy upgrades, change the version and checksum in
[caddy.env](../infra/deploy/caddy.env), then redeploy.

Troubleshoot using the printed SSM command ID, `/var/log/token-tracker-setup.log`,
or `journalctl -u token-tracker -u caddy`. Timed-out commands may still be running.

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
