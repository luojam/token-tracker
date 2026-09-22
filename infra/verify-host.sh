#!/bin/bash
set -euo pipefail

volume_id=$1
[[ "$volume_id" =~ ^vol-[0-9a-f]+$ ]] || { echo "Invalid volume ID" >&2; exit 1; }

cloud_status=0
cloud_output=$(cloud-init status --wait --long) || cloud_status=$?
printf '%s\n' "$cloud_output"
test "$cloud_status" -eq 0
grep -qx 'status: done' <<< "$cloud_output"

mount_dir=/var/lib/token-tracker
if ! mountpoint -q "$mount_dir"; then
  echo "Missing data mount at $mount_dir" >&2
  exit 1
fi

device=$(findmnt -nro SOURCE --mountpoint "$mount_dir")
serial=$(lsblk -dn -o SERIAL "$device" | tr -d '[:space:]-')
if [ "$serial" != "${volume_id//-/}" ]; then
  echo "Wrong volume mounted at $mount_dir; expected $volume_id" >&2
  exit 1
fi

test "$(findmnt -nro FSTYPE --mountpoint "$mount_dir")" = ext4
systemctl is-active --quiet amazon-ssm-agent
aws --version
curl --version
findmnt --mountpoint "$mount_dir"
