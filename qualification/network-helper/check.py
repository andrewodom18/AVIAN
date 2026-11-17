#!/usr/bin/env python3
"""Run the helper/engine functional checks inside a disposable Docker network."""

import argparse
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--helper-volume", default="avian-helper-target")
    parser.add_argument("--engine-volume", default="avian-managed-target")
    parser.add_argument("--image", default="avian-qualification:local")
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    source = Path(__file__).resolve().parent
    # All capabilities and network mutations stay inside this newly created
    # network-none container. Nothing is installed on the operator's host.
    bootstrap = (
        'import os, runpy; os.symlink("/engine-build/debug", "/engine"); '
        'runpy.run_path("/qualification/run.py", run_name="__main__")'
    )
    completed = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--privileged",
            "--network",
            "none",
            "--entrypoint",
            "python3",
            "-e",
            "AVIAN_HELPER_CONTAINER=1",
            "--mount",
            f"type=bind,source={source},target=/qualification,readonly",
            "--mount",
            f"type=volume,source={arguments.helper_volume},target=/build,readonly",
            "--mount",
            f"type=volume,source={arguments.engine_volume},target=/engine-build,readonly",
            arguments.image,
            "-c",
            bootstrap,
        ],
        capture_output=True,
        text=True,
        timeout=120,
    )
    if completed.returncode:
        raise SystemExit(completed.stderr or completed.stdout or "qualification failed")
    report = json.loads(completed.stdout.strip())
    if not report.get("passed") or len(report.get("gates", {})) != 20:
        raise SystemExit("the complete helper and engine fixture did not pass")
    report["scope"] = "isolated Linux helper and unprivileged engine functional checks"
    report["installed_service_qualified"] = False
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    print(f"Passed {len(report['gates'])} isolated functional checks")


if __name__ == "__main__":
    main()
