"""Run resource-bounded mesh growth and congestion checks in an isolated container."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--engine-directory", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--priority-forwarding", action="store_true")
    parser.add_argument("--scenario", choices=["growth", "failover"], default="growth")
    parser.add_argument(
        "--fault", choices=["blackhole", "relay-stop"], default="blackhole"
    )
    parser.add_argument("--growth-seconds", type=float, default=30)
    parser.add_argument("--memory-budget-mib", type=int, default=512)
    parser.add_argument("--load-seconds", type=float, default=10)
    parser.add_argument("--bottleneck-bps", type=int, default=2000000)
    arguments = parser.parse_args()
    if (
        min(
            arguments.growth_seconds,
            arguments.memory_budget_mib,
            arguments.load_seconds,
            arguments.bottleneck_bps,
        )
        <= 0
    ):
        parser.error("Resource and time budgets must be positive")
    qualification = Path(__file__).resolve().parents[1]
    engine = arguments.engine_directory.resolve()
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    build = json.loads((engine / "build.json").read_text())
    actual = {
        name: hashlib.sha256((engine / name).read_bytes()).hexdigest()
        for name in ["easytier-core", "easytier-cli"]
    }
    if arguments.priority_forwarding:
        patch_names = [
            "verified-membership",
            "authenticated-forwarding",
            "traffic-policy",
            "live-revocation",
            "failover",
            "topology-policy",
            "congestion",
        ]
        expected_patches = [
            {
                "name": name + ".patch",
                "sha256": hashlib.sha256(
                    (qualification / "patches" / (name + ".patch")).read_bytes()
                ).hexdigest(),
            }
            for name in patch_names
        ]
        if (
            build.get("priority_forwarding") is not True
            or build.get("patches") != expected_patches
        ):
            parser.error(
                "Priority forwarding requires the current seven-patch build manifest"
            )
    if build["binary_sha256"] != actual:
        parser.error("Candidate binaries differ from build provenance")
    (output / "provenance.json").write_text(json.dumps(build, indent=2) + "\n")
    subprocess.run(
        ["docker", "build", "-t", "avian-qualification:local", str(qualification)],
        check=True,
    )
    command = [
        "docker",
        "run",
        "--rm",
        "--network",
        "none",
        "--privileged",
        "--mount",
        f"type=bind,source={qualification},target=/qualification,readonly",
        "--mount",
        f"type=bind,source={engine},target=/engine,readonly",
        "--mount",
        f"type=bind,source={output},target=/results",
        "--tmpfs",
        "/state:mode=0700",
        "--entrypoint",
        "python3",
        "avian-qualification:local",
        "/qualification/load/"
        + ("runtime.py" if arguments.scenario == "growth" else "failover.py"),
    ]
    budgets = (
        ["growth_seconds", "memory_budget_mib", "load_seconds", "bottleneck_bps"]
        if arguments.scenario == "growth"
        else ["bottleneck_bps"]
    )
    if arguments.scenario == "failover":
        command.extend(["--fault", arguments.fault])
    for name in budgets:
        command.extend(["--" + name.replace("_", "-"), str(getattr(arguments, name))])
    if arguments.priority_forwarding:
        command.append("--priority-forwarding")
    return subprocess.run(command).returncode


if __name__ == "__main__":
    raise SystemExit(main())
