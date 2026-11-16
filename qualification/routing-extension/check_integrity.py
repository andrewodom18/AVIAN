#!/usr/bin/env python3
"""Run the routing-metadata integrity gate against the pinned engine source."""

import argparse
import json
from pathlib import Path
import subprocess
import tempfile
import uuid


ENGINE_COMMIT = "8428a89d2dabc94c97d370ec607c6ca142473626"
FAILURE_MESSAGE = (
    "authenticated connection accepted modified routing metadata; "
    "class restrictions cannot trust this header"
)


def execute(arguments, *, directory=None):
    return subprocess.run(
        arguments, cwd=directory, capture_output=True, text=True, check=True
    ).stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True, help="Local pinned engine checkout")
    parser.add_argument("--image", default="avian-engine-build:local")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=900)
    arguments = parser.parse_args()
    if arguments.timeout <= 0:
        parser.error("timeout must be positive")
    revision = execute(["git", "rev-parse", "HEAD"], directory=arguments.source).strip()
    if revision != ENGINE_COMMIT:
        parser.error(f"source must be at {ENGINE_COMMIT}")
    patch = Path(__file__).resolve().with_name("routing-integrity-test.patch")
    # Clone committed source so unrelated experiments in the supplied checkout are excluded.
    with tempfile.TemporaryDirectory(prefix="avian-routing-integrity-") as temporary:
        checkout = Path(temporary) / "source"
        execute(["git", "clone", "--no-hardlinks", str(arguments.source.resolve()), str(checkout)])
        execute(["git", "apply", "--check", str(patch)], directory=checkout)
        execute(["git", "apply", str(patch)], directory=checkout)
        container_name = f"avian-integrity-{uuid.uuid4().hex[:12]}"
        command = [
            "docker", "run", "--rm", "--name", container_name,
            "-v", f"{checkout}:/work",
            "-v", "avian-engine-cargo:/usr/local/cargo",
            "-v", "avian-routing-target:/build",
            "-w", "/work",
            "-e", "RUSTUP_TOOLCHAIN=1.95.0",
            "-e", "CARGO_TARGET_DIR=/build",
            "-e", "CARGO_BUILD_JOBS=4",
            "-e", "CARGO_PROFILE_DEV_DEBUG=0",
            "-e", "CARGO_INCREMENTAL=0",
            arguments.image,
            "cargo", "test", "--locked", "-p", "easytier", "--lib",
            "--no-default-features", "--features", "tun,aes-gcm",
            "avian_routing_metadata_must_be_authenticated", "--", "--nocapture",
        ]
        try:
            completed = subprocess.run(
                command, capture_output=True, text=True, timeout=arguments.timeout
            )
            log = completed.stdout + completed.stderr
            passed_test = (
                "test peers::peer_conn::tests::avian_routing_metadata_must_be_authenticated ... ok"
            )
            if completed.returncode == 0 and passed_test in log:
                status = "passed"
            elif FAILURE_MESSAGE in log:
                status = "failed"
            else:
                status = "error"
            exit_code = completed.returncode
        except subprocess.TimeoutExpired as error:
            subprocess.run(
                ["docker", "rm", "--force", container_name], capture_output=True, check=False
            )
            status = "error"
            exit_code = None
            output = error.stdout or b""
            errors = error.stderr or b""
            log = "Qualification timed out.\n" + (
                output.decode(errors="replace") if isinstance(output, bytes) else output
            ) + (errors.decode(errors="replace") if isinstance(errors, bytes) else errors)
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        log_path = arguments.output.with_suffix(".log")
        log_path.write_text(log)
        result = {
            "gate": "routing_metadata_integrity",
            "engine": "EasyTier",
            "version": "2.6.4",
            "commit": ENGINE_COMMIT,
            "status": status,
            "runtime_exit_code": exit_code,
            "baseline_delivery_verified": status in {"passed", "failed"},
            "modified_routing_metadata_accepted": None if status == "error" else status == "failed",
            "payload_plaintext_modified": None if status == "error" else False,
            "network_control_enabled": False,
            "log": str(log_path),
        }
        arguments.output.write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))
        return {"passed": 0, "failed": 2, "error": 1}[status]


if __name__ == "__main__":
    raise SystemExit(main())
