#!/usr/bin/env python3
"""Build the pinned engine with the unqualified membership candidate."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

DIRECTORY = Path(__file__).resolve().parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--authenticated-forwarding", action="store_true",
                        help="Also apply the isolated per-hop forwarding candidate")
    parser.add_argument("--traffic-policy", action="store_true")
    parser.add_argument("--live-revocation", action="store_true")
    parser.add_argument("--failover", action="store_true")
    parser.add_argument("--topology-policy", action="store_true")
    arguments = parser.parse_args()
    lock = json.loads((DIRECTORY / "engine.lock.json").read_text())
    patch = DIRECTORY / "patches" / "verified-membership.patch"
    patches = [patch]
    authenticated_forwarding = arguments.authenticated_forwarding or arguments.traffic_policy or arguments.live_revocation or arguments.failover or arguments.topology_policy
    if authenticated_forwarding:
        patches.append(DIRECTORY / "patches" / "authenticated-forwarding.patch")
    if arguments.traffic_policy or arguments.topology_policy:
        patches.append(DIRECTORY / "patches" / "traffic-policy.patch")
    if arguments.live_revocation or arguments.topology_policy:
        patches.append(DIRECTORY / "patches" / "live-revocation.patch")
    if arguments.failover or arguments.topology_policy:
        patches.append(DIRECTORY / "patches" / "failover.patch")
    if arguments.topology_policy:
        patches.append(DIRECTORY / "patches" / "topology-policy.patch")
    patch_contents = [(candidate_patch.name, candidate_patch.read_bytes()) for candidate_patch in patches]
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    subprocess.run(["docker", "build", "-f", str(DIRECTORY / "Dockerfile.build"),
                    "-t", "avian-engine-build:local", str(DIRECTORY)], check=True)
    with tempfile.TemporaryDirectory(prefix="avian-engine-source-") as temporary_directory:
        source = Path(temporary_directory)
        subprocess.run(["git", "clone", "--depth", "1", "--branch", "v" + lock["version"],
                        "https://github.com/EasyTier/EasyTier.git", str(source)], check=True)
        revision = subprocess.check_output(["git", "-C", str(source), "rev-parse", "HEAD"], text=True).strip()
        if revision != lock["source_commit"]:
            raise SystemExit("Upstream tag no longer matches the pinned revision")
        for index, (name, contents) in enumerate(patch_contents):
            saved_patch = source / ".git" / f"candidate-{index}.patch"
            saved_patch.write_bytes(contents)
            subprocess.run(["git", "-C", str(source), "apply", "--check", str(saved_patch)], check=True)
            subprocess.run(["git", "-C", str(source), "apply", str(saved_patch)], check=True)
        subprocess.run([
            "docker", "run", "--rm", "-e", "CARGO_BUILD_JOBS=4", "-e", "RUSTUP_TOOLCHAIN=1.95.0",
            "-e", "CARGO_TARGET_DIR=/build",
            "--mount", f"type=bind,source={source},target=/source",
            "--mount", "type=volume,source=avian-engine-cargo,target=/usr/local/cargo",
            "--mount", "type=volume,source=avian-admission-target,target=/build",
            "avian-engine-build:local", "cargo", "build", "--locked", "-p", "easytier",
            "--bin", "easytier-core", "--bin", "easytier-cli", "--no-default-features", "--features", "tun,aes-gcm",
        ], check=True)
        subprocess.run([
            "docker", "run", "--rm",
            "--mount", "type=volume,source=avian-admission-target,target=/build,readonly",
            "--mount", f"type=bind,source={output},target=/output",
            "avian-engine-build:local", "cp", "/build/debug/easytier-core",
            "/build/debug/easytier-cli", "/output/",
        ], check=True)
        hashes = {}
        for name in ["easytier-core", "easytier-cli"]:
            target = output / name
            binary = target.read_bytes()
            target.chmod(0o755)
            hashes[name] = hashlib.sha256(binary).hexdigest()
        (output / "build.json").write_text(json.dumps({
            "source_commit": revision,
            "extension": "overlay-address-claims-v1",
            "patch_sha256": hashlib.sha256(patch_contents[0][1]).hexdigest(),
            "patches": [{"name": name, "sha256": hashlib.sha256(contents).hexdigest()}
                        for name, contents in patch_contents],
            "authenticated_forwarding": authenticated_forwarding,
            "traffic_policy": arguments.traffic_policy or arguments.topology_policy,
            "live_revocation": arguments.live_revocation or arguments.topology_policy,
            "failover": arguments.failover or arguments.topology_policy,
            "topology_policy": arguments.topology_policy,
            "qualified_for_cutover": False,
            "binary_sha256": hashes,
        }, indent=2) + "\n")


if __name__ == "__main__":
    main()
