# Server deployment and updates

The supplied deployment runs on AWS EC2. [Terraform](../infra/main.tf) creates
the infrastructure and auth token; [deploy.py](../scripts/deploy.py) builds the
server and installs it, Caddy, and systemd services through AWS Systems Manager
(SSM). No SSH setup is needed.

Run local commands below from the repository root. Replace `tracker.example.com`
with a domain you control.

## First deployment

### 1. Prepare tools and credentials

Install Rust 1.85+, Python 3, Terraform 1.11+, AWS CLI v2, and `musl-gcc`. The build
uses an x86_64 Linux musl toolchain; on Debian/Ubuntu, `musl-gcc` is provided by
`musl-tools`. Add the Rust target:

```sh
rustup target add x86_64-unknown-linux-musl
aws sts get-caller-identity
```

Use AWS credentials with permission to provision the resources in `infra/main.tf`,
upload deployment files to S3, run and inspect SSM commands, and read/decrypt the
auth parameter. Use the same account/profile for Terraform and deployment.

Review `infra/main.tf` before applying: it uses region `eu-north-1`, availability
zone `eu-north-1a`, an EC2 `t3.small`, and a 10 GiB data volume. Public inbound
ports are 80 and 443.

### 2. Provision infrastructure

```sh
terraform -chdir=infra init
terraform -chdir=infra apply
terraform -chdir=infra output -raw public_ip
```

Review and approve Terraform's plan. Preserve the local Terraform state in
`infra/`: subsequent deployments read its outputs. This step prepares the host;
it does not install the application.

### 3. Configure DNS

Point your domain's DNS A record at the public IP above. Wait for public DNS to
resolve to that address before deploying. Caddy uses the domain to obtain an
HTTPS certificate; ports 80 and 443 must remain reachable.

### 4. Install and verify

```sh
python3 scripts/deploy.py tracker.example.com
curl --fail --show-error https://tracker.example.com/health
```

The script builds the current local checkout, uploads the binary and installer
to S3, waits for SSM and host setup, then installs and restarts both services.
It prints the binary's release SHA-256 and SSM command ID. Save the release SHA
if you want to deploy that binary again.

The installer checks HTTPS locally on the instance. The separate `curl` command
checks public access from your machine and should print `ok`.

## Connect clients

On each client, install the CLI using the [quickstart](../README.md#quick-start).
Retrieve the token using AWS credentials with permission to read and decrypt
`/token-tracker/auth-token`:

```sh
umask 077
mkdir -p ~/.config/token-tracker
aws ssm get-parameter --region eu-north-1 \
  --name /token-tracker/auth-token --with-decryption \
  --query Parameter.Value --output text > ~/.config/token-tracker/auth.token
chmod 600 ~/.config/token-tracker/auth.token
token-tracker upload https://tracker.example.com --auth-file ~/.config/token-tracker/auth.token
token-tracker summary --server https://tracker.example.com --auth-file ~/.config/token-tracker/auth.token
```

If you changed the infrastructure region, use that region here too. Clients
need AWS access only to retrieve the token; upload and reporting use the token.

Save `server_url` and `auth_file` in the CLI's [configuration](cli.md#configuration)
to use `token-tracker upload` and `token-tracker summary --server` without
repeating them. Repeat uploads whenever you want the server's totals refreshed.

## Updates and rollback

Update your local checkout to the source you want to deploy, then run:

```sh
python3 scripts/deploy.py tracker.example.com
```

This builds the server on your machine, uploads it, and restarts both services,
briefly interrupting requests. It also deploys the current `infra/deploy/` files.
Failed health checks do not trigger automatic rollback.

Add `--provision` to run Terraform init/apply for infrastructure changes. Review
the plan, including any instance replacement.

To deploy a previously recorded binary:

```sh
python3 scripts/deploy.py tracker.example.com --release RELEASE_SHA
```

Replace `RELEASE_SHA` with the uploaded binary's full SHA-256 checksum. This skips
the build but still installs the current installer, service configuration, and
Caddy version. Rollback requires database compatibility.

For Caddy upgrades, update the version and checksum in
[caddy.env](../infra/deploy/caddy.env), then redeploy. For server runtime settings,
edit [server.env](../infra/deploy/server.env) and redeploy; see the
[variable reference](server.md#runtime). Manual edits on the host are overwritten.

## Persistent data

The separate EBS volume is mounted at `/var/lib/token-tracker`. It holds the
server database at `server/snapshots.db` and Caddy state under `caddy/`. Ordinary
application deployments preserve this data. Terraform can reattach the volume
when replacing the instance; the replacement still needs an application deployment.

Binary rollback does not restore data, and there is no automated backup configured.
Terraform destroy removes the managed data volume and deployment bucket.

## Troubleshooting

To inspect a deployment, use its printed SSM command ID and the instance ID from
Terraform, replacing `COMMAND_ID` below:

```sh
aws ssm get-command-invocation --region "$(terraform -chdir=infra output -raw region)" \
  --instance-id "$(terraform -chdir=infra output -raw instance_id)" \
  --command-id COMMAND_ID
```

For host logs, run this SSM command from your local machine:

```sh
aws ssm send-command --region "$(terraform -chdir=infra output -raw region)" \
  --instance-ids "$(terraform -chdir=infra output -raw instance_id)" \
  --document-name AWS-RunShellScript \
  --parameters '{"commands":["tail -n 100 /var/log/token-tracker-setup.log","journalctl -u token-tracker -u caddy -n 100 --no-pager"]}' \
  --query Command.CommandId --output text
```

Inspect the returned command ID with `get-command-invocation` above. Setup logs
cover disk mounting and SSM preparation; service logs cover application startup
and HTTPS. For certificate failures, check DNS and public access to ports 80/443.
If SSM never becomes ready, inspect EC2 status and networking in the AWS console.
Timed-out deployment commands may still be running; inspect their status before retrying.
