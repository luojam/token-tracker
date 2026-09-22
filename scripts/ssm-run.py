#!/usr/bin/env python3
"""Run one command on the Terraform-managed instance using the local AWS profile."""

import argparse
import json
import shlex
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def run_json(command, timeout=45):
    result = subprocess.run(
        command, capture_output=True, text=True, timeout=timeout, check=False
    )
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return json.loads(result.stdout)


def show_output(result):
    for key, stream in (
        ("StandardOutputContent", sys.stdout),
        ("StandardErrorContent", sys.stderr),
    ):
        output = result.get(key, "")
        if output:
            print(output, end="" if output.endswith("\n") else "\n", file=stream)


def positive_seconds(value):
    seconds = int(value)
    if seconds < 1 or seconds > 172800:
        raise argparse.ArgumentTypeError("must be between 1 and 172800 seconds")
    return seconds


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ready-timeout", type=positive_seconds, default=600)
    parser.add_argument(
        "--timeout",
        type=positive_seconds,
        default=900,
        help="remote execution timeout in seconds (local wait adds 90 seconds)",
    )
    parser.add_argument(
        "--script", type=Path, help="local Bash script; remaining arguments go to it"
    )
    parser.add_argument(
        "command", nargs=argparse.REMAINDER, help="command and literal arguments"
    )

    args = parser.parse_args()

    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if args.script:
        command = ["bash", "-c", args.script.read_text(), str(args.script), *command]
    if not command:
        parser.error("provide a command or --script")

    outputs = run_json(["terraform", f"-chdir={ROOT / 'infra'}", "output", "-json"])
    region = outputs["region"]["value"]
    instance = outputs["instance_id"]["value"]
    aws = [
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
        "20",
        "ssm",
    ]

    deadline = time.monotonic() + args.ready_timeout
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError(f"SSM readiness timed out for {instance}")

        info = run_json(
            [
                *aws,
                "describe-instance-information",
                "--filters",
                f"Key=InstanceIds,Values={instance}",
            ],
            timeout=min(45, remaining),
        )
        if any(
            item["InstanceId"] == instance and item["PingStatus"] == "Online"
            for item in info["InstanceInformationList"]
        ):
            break

        time.sleep(min(5, max(0, deadline - time.monotonic())))

    request = {
        "DocumentName": "AWS-RunShellScript",
        "InstanceIds": [instance],
        "TimeoutSeconds": 60,
        "Parameters": {
            "commands": [shlex.join(command)],
            "executionTimeout": [str(args.timeout)],
        },
    }
    try:
        submission = run_json(
            [*aws, "send-command", "--cli-input-json", json.dumps(request)]
        )
        command_id = submission["Command"]["CommandId"]
    except (Exception, KeyboardInterrupt) as error:
        raise RuntimeError(
            f"Could not confirm command submission on {instance}: {error}. "
            "The submission outcome is unknown; the remote command may be running. "
            "Check SSM command history before retrying."
        ) from error
    print(f"SSM command {command_id} on {instance}", file=sys.stderr, flush=True)

    deadline = time.monotonic() + args.timeout + 90
    result = {}
    try:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("local result wait expired")

            try:
                result = run_json(
                    [
                        *aws,
                        "get-command-invocation",
                        "--command-id",
                        command_id,
                        "--instance-id",
                        instance,
                    ],
                    timeout=min(45, remaining),
                )
            except RuntimeError as error:
                if "InvocationDoesNotExist" not in str(error):
                    raise
            else:
                status = result["Status"]
                if status not in {"Pending", "InProgress", "Delayed", "Cancelling"}:
                    if status != "Success" or result["ResponseCode"] != 0:
                        raise RuntimeError(
                            f"remote status {status}, "
                            f"exit code {result['ResponseCode']}"
                        )
                    return 0

            time.sleep(min(5, max(0, deadline - time.monotonic())))
    except (TimeoutError, subprocess.TimeoutExpired, KeyboardInterrupt) as error:
        raise RuntimeError(
            f"Command {command_id} on {instance}: local wait ended ({error}). "
            "The remote command may still be running; "
            "inspect its result before retrying."
        ) from error
    except Exception as error:
        raise RuntimeError(f"Command {command_id} on {instance}: {error}") from error
    finally:
        show_output(result)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (Exception, KeyboardInterrupt) as error:  # noqa: BLE001
        print(f"ssm-run.py: {error}", file=sys.stderr)
        sys.exit(1)
