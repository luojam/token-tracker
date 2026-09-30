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
(`/token-tracker/auth-token`) to a local file on each machine using AWS credentials
with access to the parameter. The file must be accessible only by its owner:

```sh
umask 077
mkdir -p ~/.config/token-tracker
aws ssm get-parameter --region eu-north-1 \
  --name /token-tracker/auth-token --with-decryption \
  --query Parameter.Value --output text > ~/.config/token-tracker/auth.token
chmod 600 ~/.config/token-tracker/auth.token
token-tracker upload https://tracker.example.com --auth-file ~/.config/token-tracker/auth.token
```

Upload refreshes sources automatically and includes all locally retained usage.
HTTPS is required except on loopback.

## Combined summary

Fetch totals across the latest uploaded snapshot from each machine:

```sh
token-tracker summary --server https://tracker.example.com --auth-file ~/.config/token-tracker/auth.token
```

Save `server_url` and `auth_file` in [config.toml](../README.md#configuration)
to use `token-tracker upload` and `token-tracker summary --server` without
repeating the URL or auth-file path. Command-line values override saved defaults.
Use `token-tracker day --server`, `week --server`, or `month --server` for the
current UTC calendar period. Weeks start Monday. `token-tracker --server`
returns all-time totals.

## API

- `POST /snapshots` accepts [snapshot JSON](../tests/fixtures/export-example.json)
  with `Content-Type: application/json` and `Authorization: Bearer <token>`.
  Duplicate uploads succeed; stale or conflicting revisions return HTTP 409.
- `GET /summary` requires the same bearer token and returns all-time cost and
  token totals across machines. Add `?period=day`, `?period=week`, or
  `?period=month` to filter by the current UTC calendar period (Monday-based weeks).
  Optional `agent`, `provider`, and `model` parameters support [report filters](filters.md).
- `GET /health` is public.
