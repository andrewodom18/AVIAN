#!/usr/bin/env python3
"""Build the pinned engine with the unqualified membership candidate."""

import argparse
import hashlib
import json
import shutil
import tomllib
from pathlib import Path
import subprocess
import tempfile

DIRECTORY = Path(__file__).resolve().parent


def prepare_shared_routing(destination):
    repository = DIRECTORY.parent
    version = tomllib.loads((repository / "Cargo.toml").read_text())["workspace"][
        "package"
    ]["version"]
    destination.mkdir()
    shutil.copytree(
        repository / "crates" / "avian-routing" / "src", destination / "src"
    )
    shutil.copyfile(repository / "LICENSE.md", destination / "LICENSE")
    manifest = f"""[package]
name = "avian-routing"
version = "{version}"
edition = "2024"
license-file = "LICENSE"

[dependencies]
serde = {{ version = "1", features = ["derive"] }}
thiserror = "2"
"""
    (destination / "Cargo.toml").write_text(manifest)
    return {
        name: hashlib.sha256((destination / name).read_bytes()).hexdigest()
        for name in ["Cargo.toml", "src/lib.rs", "LICENSE"]
    }


def prepare_shared_network_helper(destination):
    repository = DIRECTORY.parent
    version = tomllib.loads((repository / "Cargo.toml").read_text())["workspace"][
        "package"
    ]["version"]
    destination.mkdir()
    (destination / "src").mkdir()
    for name in ["lib.rs", "client.rs", "transport.rs"]:
        shutil.copyfile(
            repository / "crates" / "avian-network-helper" / "src" / name,
            destination / "src" / name,
        )
    shutil.copyfile(repository / "LICENSE.md", destination / "LICENSE")
    # The engine receives only the IPC client. Server dependencies and privileged
    # operations stay in the separately built helper executable.
    manifest = f"""[package]
name = "avian-network-helper"
version = "{version}"
edition = "2024"
license-file = "LICENSE"

[features]
default = []
server = []

[dependencies]
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
thiserror = "2"
ipnet = {{ version = "2", features = ["serde"] }}

[target.'cfg(target_os = "linux")'.dependencies]
rustix = {{ version = "1", features = ["net", "fs", "process", "event"] }}
socket2 = {{ version = "0.6", features = ["all"] }}
"""
    (destination / "Cargo.toml").write_text(manifest)
    return {
        name: hashlib.sha256((destination / name).read_bytes()).hexdigest()
        for name in [
            "Cargo.toml",
            "src/lib.rs",
            "src/client.rs",
            "src/transport.rs",
            "LICENSE",
        ]
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--authenticated-forwarding",
        action="store_true",
        help="Also apply the isolated per-hop forwarding candidate",
    )
    parser.add_argument("--traffic-policy", action="store_true")
    parser.add_argument("--live-revocation", action="store_true")
    parser.add_argument("--failover", action="store_true")
    parser.add_argument("--topology-policy", action="store_true")
    parser.add_argument(
        "--priority-forwarding",
        action="store_true",
        help="Apply topology routing and bounded per-class forwarding queues",
    )
    parser.add_argument("--measured-routing", action="store_true")
    parser.add_argument("--underlay-binding", action="store_true")
    parser.add_argument("--private-rpc", action="store_true")
    parser.add_argument("--managed-network", action="store_true")
    parser.add_argument(
        "--release",
        action="store_true",
        help="Build optimized engine and helper binaries",
    )
    arguments = parser.parse_args()
    private_rpc = arguments.private_rpc or arguments.managed_network
    underlay_binding = arguments.underlay_binding or private_rpc
    measured_routing = arguments.measured_routing or underlay_binding
    priority_forwarding = arguments.priority_forwarding or measured_routing
    topology_policy = arguments.topology_policy or priority_forwarding
    lock = json.loads((DIRECTORY / "engine.lock.json").read_text())
    patch = DIRECTORY / "patches" / "verified-membership.patch"
    patches = [patch]
    authenticated_forwarding = (
        arguments.authenticated_forwarding
        or arguments.traffic_policy
        or arguments.live_revocation
        or arguments.failover
        or topology_policy
    )
    if authenticated_forwarding:
        patches.append(DIRECTORY / "patches" / "authenticated-forwarding.patch")
    if arguments.traffic_policy or topology_policy:
        patches.append(DIRECTORY / "patches" / "traffic-policy.patch")
    if arguments.live_revocation or topology_policy:
        patches.append(DIRECTORY / "patches" / "live-revocation.patch")
    if arguments.failover or topology_policy:
        patches.append(DIRECTORY / "patches" / "failover.patch")
    if topology_policy:
        patches.append(DIRECTORY / "patches" / "topology-policy.patch")
    if priority_forwarding:
        patches.append(DIRECTORY / "patches" / "congestion.patch")
    if measured_routing:
        patches.append(DIRECTORY / "patches" / "measured-routing.patch")
    if underlay_binding:
        patches.append(DIRECTORY / "patches" / "underlay-binding.patch")
    if private_rpc:
        patches.append(DIRECTORY / "patches" / "private-rpc.patch")
    if arguments.managed_network:
        patches.append(DIRECTORY / "patches" / "managed-network.patch")
    patch_contents = [
        (candidate_patch.name, candidate_patch.read_bytes())
        for candidate_patch in patches
    ]
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        [
            "docker",
            "build",
            "-f",
            str(DIRECTORY / "Dockerfile.build"),
            "-t",
            "avian-engine-build:local",
            str(DIRECTORY),
        ],
        check=True,
    )
    with tempfile.TemporaryDirectory(
        prefix="avian-engine-source-"
    ) as temporary_directory:
        source = Path(temporary_directory) / "easytier"
        planner = source / "avian-routing"
        shared_source = None
        shared_helper = None
        subprocess.run(
            [
                "git",
                "clone",
                "--depth",
                "1",
                "--branch",
                "v" + lock["version"],
                "https://github.com/EasyTier/EasyTier.git",
                str(source),
            ],
            check=True,
        )
        revision = subprocess.check_output(
            ["git", "-C", str(source), "rev-parse", "HEAD"], text=True
        ).strip()
        if revision != lock["source_commit"]:
            raise SystemExit("Upstream tag no longer matches the pinned revision")
        if measured_routing:
            shared_source = prepare_shared_routing(planner)
        if arguments.managed_network:
            shared_helper = prepare_shared_network_helper(
                source / "avian-network-helper"
            )
        for index, (name, contents) in enumerate(patch_contents):
            saved_patch = source / ".git" / f"candidate-{index}.patch"
            saved_patch.write_bytes(contents)
            subprocess.run(
                ["git", "-C", str(source), "apply", "--check", str(saved_patch)],
                check=True,
            )
            subprocess.run(
                ["git", "-C", str(source), "apply", str(saved_patch)], check=True
            )
        subprocess.run(
            [
                "docker",
                "run",
                "--rm",
                "-e",
                "CARGO_BUILD_JOBS=4",
                "-e",
                "RUSTUP_TOOLCHAIN=1.95.0",
                "-e",
                "CARGO_TARGET_DIR=/build",
                "--mount",
                f"type=bind,source={source},target=/source",
                "--mount",
                "type=volume,source=avian-engine-cargo,target=/usr/local/cargo",
                "--mount",
                "type=volume,source=avian-admission-target,target=/build",
                "avian-engine-build:local",
                "cargo",
                "build",
                "--locked",
                "-p",
                "easytier",
                "--bin",
                "easytier-core",
                "--bin",
                "easytier-cli",
                "--no-default-features",
                "--features",
                "tun,aes-gcm",
                *(["--release"] if arguments.release else []),
            ],
            check=True,
        )
        subprocess.run(
            [
                "docker",
                "run",
                "--rm",
                "--mount",
                "type=volume,source=avian-admission-target,target=/build,readonly",
                "--mount",
                f"type=bind,source={output},target=/output",
                "avian-engine-build:local",
                "cp",
                f"/build/{'release' if arguments.release else 'debug'}/easytier-core",
                f"/build/{'release' if arguments.release else 'debug'}/easytier-cli",
                "/output/",
            ],
            check=True,
        )
        if arguments.managed_network:
            subprocess.run(
                [
                    "docker",
                    "run",
                    "--rm",
                    "-e",
                    "CARGO_BUILD_JOBS=4",
                    "-e",
                    "RUSTUP_TOOLCHAIN=1.95.0",
                    "-e",
                    "CARGO_TARGET_DIR=/build",
                    "--mount",
                    f"type=bind,source={DIRECTORY.parent},target=/workspace,readonly",
                    "--mount",
                    "type=volume,source=avian-engine-cargo,target=/usr/local/cargo",
                    "--mount",
                    "type=volume,source=avian-helper-target,target=/build",
                    "-w",
                    "/workspace",
                    "avian-engine-build:local",
                    "cargo",
                    "build",
                    "--locked",
                    "-p",
                    "avian-network-helper",
                    *(["--release"] if arguments.release else []),
                ],
                check=True,
            )
            subprocess.run(
                [
                    "docker",
                    "run",
                    "--rm",
                    "--mount",
                    "type=volume,source=avian-helper-target,target=/build,readonly",
                    "--mount",
                    f"type=bind,source={output},target=/output",
                    "avian-engine-build:local",
                    "cp",
                    f"/build/{'release' if arguments.release else 'debug'}/avian-network-helper",
                    "/output/",
                ],
                check=True,
            )
        hashes = {}
        binaries = ["easytier-core", "easytier-cli"]
        if arguments.managed_network:
            binaries.append("avian-network-helper")
        for name in binaries:
            target = output / name
            binary = target.read_bytes()
            target.chmod(0o755)
            hashes[name] = hashlib.sha256(binary).hexdigest()
        (output / "build.json").write_text(
            json.dumps(
                {
                    "source_commit": revision,
                    "extension": "overlay-address-claims-v1",
                    "patch_sha256": hashlib.sha256(patch_contents[0][1]).hexdigest(),
                    "patches": [
                        {"name": name, "sha256": hashlib.sha256(contents).hexdigest()}
                        for name, contents in patch_contents
                    ],
                    "authenticated_forwarding": authenticated_forwarding,
                    "traffic_policy": arguments.traffic_policy or topology_policy,
                    "live_revocation": arguments.live_revocation or topology_policy,
                    "failover": arguments.failover or topology_policy,
                    "topology_policy": topology_policy,
                    "priority_forwarding": priority_forwarding,
                    "measured_routing": measured_routing,
                    "underlay_binding": underlay_binding,
                    "private_rpc": private_rpc,
                    "managed_network": arguments.managed_network,
                    "build_profile": "release" if arguments.release else "debug",
                    "shared_network_helper_sha256": shared_helper,
                    "peer_protocol": (
                        "avian-peerconn-noise-hop-measured-v1"
                        if measured_routing
                        else "avian-peerconn-noise-hop-topology-v1"
                    ),
                    "shared_routing_sha256": shared_source,
                    "target_os": "linux",
                    "target_arch": subprocess.check_output(
                        [
                            "docker",
                            "run",
                            "--rm",
                            "avian-engine-build:local",
                            "uname",
                            "-m",
                        ],
                        text=True,
                    ).strip(),
                    "qualified_for_cutover": False,
                    "binary_sha256": hashes,
                },
                indent=2,
            )
            + "\n"
        )


if __name__ == "__main__":
    main()
