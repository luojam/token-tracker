#!/usr/bin/env python3
"""Build and install Token Tracker on the Terraform-managed EC2 instance."""

import argparse
import hashlib
import json
import os
import shlex
import subprocess
import sys
import tarfile
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def checksum(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run_ssm(aws, instance, command):
    # A newly provisioned instance may take a few minutes to register with SSM.
    deadline = time.monotonic() + 600
    while True:
        if time.monotonic() >= deadline:
            raise RuntimeError(f"SSM did not become ready on {instance}")

        info = aws(
            "ssm",
            "describe-instance-information",
            "--filters",
            f"Key=InstanceIds,Values={instance}",
        )
        if any(
            item["PingStatus"] == "Online" for item in info["InstanceInformationList"]
        ):
            break
        time.sleep(5)

    request = {
        "DocumentName": "AWS-RunShellScript",
        "InstanceIds": [instance],
        "TimeoutSeconds": 60,
        "Parameters": {"commands": [command], "executionTimeout": ["900"]},
    }
    result = aws("ssm", "send-command", "--cli-input-json", json.dumps(request))
    command_id = result["Command"]["CommandId"]
    print(f"SSM command: {command_id}", flush=True)

    # SSM runs commands asynchronously; poll until the remote process finishes.
    deadline = time.monotonic() + 1020
    while time.monotonic() < deadline:
        try:
            result = aws(
                "ssm",
                "get-command-invocation",
                "--command-id",
                command_id,
                "--instance-id",
                instance,
            )
        except subprocess.CalledProcessError as error:
            if "InvocationDoesNotExist" not in error.stderr:
                raise
            time.sleep(5)
            continue

        if result["Status"] in {"Pending", "InProgress", "Delayed", "Cancelling"}:
            time.sleep(5)
            continue

        print(result.get("StandardOutputContent", ""), end="")
        print(result.get("StandardErrorContent", ""), end="", file=sys.stderr)
        if result["Status"] != "Success" or result["ResponseCode"] != 0:
            raise RuntimeError(
                f"SSM command {command_id}: {result['Status']}, exit {result['ResponseCode']}"
            )
        return

    raise RuntimeError(f"Timed out waiting for {command_id}; it may still be running")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("domain")
    parser.add_argument(
        "--provision", action="store_true", help="run Terraform init/apply first"
    )
    parser.add_argument(
        "--release", help="deploy an existing release SHA instead of building"
    )
    args = parser.parse_args()

    # Reuse an existing release, or build one identified by its binary checksum.
    release = args.release
    if not release:
        subprocess.run(
            [
                "cargo",
                "build",
                "--locked",
                "--release",
                "--target",
                "x86_64-unknown-linux-musl",
                "--target-dir",
                str(ROOT / "target"),
                "--features",
                "server",
                "--bin",
                "token-tracker-server",
            ],
            cwd=ROOT,
            env={
                **os.environ,
                "CC_x86_64_unknown_linux_musl": "musl-gcc",
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER": "musl-gcc",
            },
            check=True,
        )
        binary = ROOT / "target/x86_64-unknown-linux-musl/release/token-tracker-server"
        release = checksum(binary)

    terraform = ["terraform", f"-chdir={ROOT / 'infra'}"]
    if args.provision:
        subprocess.run([*terraform, "init"], check=True)
        subprocess.run([*terraform, "apply"], check=True)

    # Read deployment targets from Terraform's current state.
    outputs = json.loads(
        subprocess.check_output([*terraform, "output", "-json"], text=True)
    )
    region = outputs["region"]["value"]
    bucket = outputs["deployment_bucket_name"]["value"]
    instance = outputs["instance_id"]["value"]
    parameter = outputs["auth_parameter_name"]["value"]

    def aws(*arguments):
        result = subprocess.run(
            [
                "aws",
                "--region",
                region,
                "--output",
                "json",
                "--no-cli-pager",
                "--no-cli-auto-prompt",
                "--cli-connect-timeout",
                "10",
                "--cli-read-timeout",
                "60",
                *arguments,
            ],
            text=True,
            capture_output=True,
            check=True,
            timeout=120,
        )
        return json.loads(result.stdout) if result.stdout.strip() else None

    if not args.release:
        aws(
            "s3",
            "cp",
            str(binary),
            f"s3://{bucket}/releases/{release}/token-tracker-server",
            "--only-show-errors",
        )

    # Upload the current installer and service configs, even when reusing a release.
    with tempfile.TemporaryDirectory() as temp:
        bundle = Path(temp) / "bundle.tar.gz"
        with tarfile.open(bundle, "w:gz") as archive:
            archive.add(ROOT / "infra/deploy", arcname=".")

        bundle_sha = checksum(bundle)
        bundle_uri = f"s3://{bucket}/bundles/{bundle_sha}.tar.gz"
        aws("s3", "cp", str(bundle), bundle_uri, "--only-show-errors")

    install_args = shlex.join(
        [
            region,
            bucket,
            parameter,
            args.domain,
            release,
        ]
    )

    # On EC2, wait for setup to finish, verify the downloaded bundle, then install.
    remote = f"""set -euo pipefail
umask 077
timeout 600 cloud-init status --wait
export AWS_DEFAULT_REGION={shlex.quote(region)} AWS_PAGER=''
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
aws s3 cp {shlex.quote(bundle_uri)} "$work/bundle.tar.gz" --only-show-errors
printf '%s  %s\\n' {bundle_sha} "$work/bundle.tar.gz" | sha256sum --check --status
tar -xzf "$work/bundle.tar.gz" --no-same-owner -C "$work"
bash "$work/install.sh" {install_args}
"""
    print(f"Deploying {release} to {args.domain}", flush=True)
    run_ssm(aws, instance, shlex.join(["bash", "-c", remote]))


if __name__ == "__main__":
    try:
        main()
    except (
        OSError,
        RuntimeError,
        subprocess.SubprocessError,
        KeyboardInterrupt,
    ) as error:
        print(f"deploy: {getattr(error, 'stderr', None) or error}", file=sys.stderr)
        sys.exit(1)
