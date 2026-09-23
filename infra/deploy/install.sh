#!/bin/bash
set -euo pipefail
set +x
umask 077

[[ $# == 5 && $EUID == 0 ]] || { echo 'Expected: REGION BUCKET PARAMETER DOMAIN RELEASE_SHA (as root)' >&2; exit 1; }
region=$1 bucket=$2 parameter=$3 domain=$4 release=$5
export AWS_DEFAULT_REGION="$region" AWS_PAGER="" AWS_CLI_AUTO_PROMPT=off
bundle=$(cd -- "$(dirname -- "$0")" && pwd)
source "$bundle/caddy.env"
exec 9>/run/lock/token-tracker-deploy.lock
flock -w 120 9
mountpoint -q /var/lib/token-tracker

install -d -m 755 /etc/token-tracker /etc/caddy /opt/token-tracker/releases
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for user in token-tracker caddy; do
  getent group "$user" >/dev/null || groupadd --system "$user"
  id "$user" &>/dev/null || useradd --system --gid "$user" --no-create-home --shell /sbin/nologin "$user"
done
chown root:root /var/lib/token-tracker
chmod 755 /var/lib/token-tracker
install -d -o token-tracker -g token-tracker -m 700 /var/lib/token-tracker/server
chown -R token-tracker:token-tracker /var/lib/token-tracker/server
install -d -o caddy -g caddy -m 700 /var/lib/token-tracker/caddy
chown -R caddy:caddy /var/lib/token-tracker/caddy

aws ssm get-parameter --name "$parameter" --with-decryption \
  --query Parameter.Value --output text > "$work/auth.token"
install -m 600 -o token-tracker -g token-tracker "$work/auth.token" /etc/token-tracker/auth.token
install -m 644 "$bundle/server.env" /etc/token-tracker/server.env

server_dir=/opt/token-tracker/releases/$release
aws s3 cp "s3://$bucket/releases/$release/token-tracker-server" "$work/server" --only-show-errors
printf '%s  %s\n' "$release" "$work/server" | sha256sum --check --status
install -d -m 755 "$server_dir"
install -m 755 "$work/server" "$server_dir/token-tracker-server"
ln -sfn "$server_dir" /opt/token-tracker/current.new
mv -fT /opt/token-tracker/current.new /opt/token-tracker/current

curl --fail --silent --show-error --location --connect-timeout 10 --max-time 180 --retry 3 \
  "https://github.com/caddyserver/caddy/releases/download/v$CADDY_VERSION/caddy_${CADDY_VERSION}_linux_amd64.tar.gz" \
  -o "$work/caddy.tar.gz"
printf '%s  %s\n' "$CADDY_SHA" "$work/caddy.tar.gz" | sha256sum --check --status
tar -xzf "$work/caddy.tar.gz" -C "$work" --no-same-owner caddy
install -m 755 "$work/caddy" /usr/local/bin/caddy
sed "s/@DOMAIN@/$domain/g" "$bundle/Caddyfile.tmpl" > "$work/Caddyfile"
install -m 644 "$work/Caddyfile" /etc/caddy/Caddyfile

for service in token-tracker caddy; do
  install -m 644 "$bundle/$service.service" "/etc/systemd/system/$service.service"
done
systemctl daemon-reload
systemctl enable token-tracker caddy

systemctl restart token-tracker caddy
curl --fail --silent --show-error --max-time 5 --retry 30 --retry-delay 2 \
  --retry-all-errors --retry-max-time 150 \
  --resolve "$domain:443:127.0.0.1" "https://$domain/health"
echo "Deployed $release to $domain"
