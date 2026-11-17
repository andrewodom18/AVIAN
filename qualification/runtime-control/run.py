"""Run runtime-control checks with pinned binaries in disposable Linux namespaces."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine-directory", type=Path, required=True)
    parser.add_argument("--service-directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--priority-forwarding", action="store_true")
    parser.add_argument("--services", action="store_true")
    arguments = parser.parse_args()
    qualification_directory = Path(__file__).resolve().parents[1]
    engine_directory = arguments.engine_directory.resolve()
    service_directory = arguments.service_directory.resolve()
    output_directory = arguments.output.resolve()
    output_directory.mkdir(parents=True, exist_ok=True)
    provenance = json.loads((engine_directory / "build.json").read_text())
    for binary_name in ["easytier-core", "easytier-cli"]:
        if (
            hashlib.sha256((engine_directory / binary_name).read_bytes()).hexdigest()
            != provenance["binary_sha256"][binary_name]
        ):
            parser.error("Engine binary hash mismatch")
    provenance["service_binary_sha256"] = {
        binary_name: hashlib.sha256(
            (service_directory / binary_name).read_bytes()
        ).hexdigest()
        for binary_name in ["aviand", "avianctl"]
    }
    provenance["runtime_configuration"] = {
        "priority_forwarding": arguments.priority_forwarding,
        "services": arguments.services,
    }
    (output_directory / "provenance.json").write_text(
        json.dumps(provenance, indent=2) + "\n"
    )
    subprocess.run(
        [
            "docker",
            "build",
            "-t",
            "avian-qualification:local",
            str(qualification_directory),
        ],
        check=True,
    )
    runtime_arguments = (
        ["--priority-forwarding"] if arguments.priority_forwarding else []
    )
    if arguments.services:
        runtime_arguments.append("--services")
    return subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--network",
            "none",
            "--privileged",
            "--mount",
            f"type=bind,source={qualification_directory},target=/qualification,readonly",
            "--mount",
            f"type=bind,source={engine_directory},target=/engine,readonly",
            "--mount",
            f"type=bind,source={service_directory},target=/services,readonly",
            "--mount",
            f"type=bind,source={output_directory},target=/results",
            "--tmpfs",
            "/state:mode=0700",
            "--entrypoint",
            "python3",
            "avian-qualification:local",
            "/qualification/runtime-control/runtime.py",
            *runtime_arguments,
        ]
    ).returncode


if __name__ == "__main__":
    raise SystemExit(main())
