#!/usr/bin/env python3
"""Run the authenticated-hop suite against exact pinned and patched engine source."""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile
import uuid

DIRECTORY = Path(__file__).resolve().parent
QUALIFICATION = DIRECTORY.parent
REQUIRED_TESTS = {
    "avian_hop_roundtrip_replay_and_out_of_order",
    "avian_hop_rejects_tamper_without_consuming_nonce",
    "avian_hop_connection_and_direction_keys_are_distinct",
    "avian_hop_relay_preserves_inner_frame_and_reseals",
    "avian_hop_replay_window_and_message_bounds",
    "avian_hop_peer_connection_rejects_wire_tampering_and_replay",
    "avian_hop_secure_server_rejects_legacy_handshake",
    "avian_hop_mtu_accounts_for_envelopes_and_ipv6_minimum",
    "avian_hop_rejects_legacy_noise_prologue",
}


def execute(arguments, directory=None):
    return subprocess.check_output(arguments, cwd=directory, text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--image", default="avian-engine-build:local")
    parser.add_argument("--timeout", type=int, default=900)
    parser.add_argument("--network-policy", action="store_true")
    parser.add_argument("--topology-policy", action="store_true")
    parser.add_argument("--priority-forwarding", action="store_true")
    arguments = parser.parse_args()
    topology_policy = arguments.topology_policy or arguments.priority_forwarding
    if arguments.timeout <= 0:
        parser.error("timeout must be positive")
    lock = json.loads((QUALIFICATION / "engine.lock.json").read_text())
    source = arguments.source.resolve()
    revision = execute(["git", "rev-parse", "HEAD"], source)
    if revision != lock["source_commit"]:
        parser.error("source revision differs from the pinned engine")
    patches = [
        QUALIFICATION / "patches" / "verified-membership.patch",
        QUALIFICATION / "patches" / "authenticated-forwarding.patch",
    ]
    required_tests = set(REQUIRED_TESTS)
    test_filter = "avian_hop"
    if arguments.network_policy or topology_policy:
        patches.extend(
            QUALIFICATION / "patches" / name
            for name in [
                "traffic-policy.patch",
                "live-revocation.patch",
                "failover.patch",
            ]
        )
        required_tests.update(
            [
                "avian_policy_classification_and_metered_restrictions",
                "avian_policy_rejects_authentication_only_peer",
            ]
        )
        test_filter = "avian_"
    if topology_policy:
        patches.append(QUALIFICATION / "patches" / "topology-policy.patch")
        required_tests.update(
            [
                "avian_topology_remote_restrictions_and_direct_bypass",
                "avian_topology_unknown_metrics_and_cycles",
                "avian_topology_parallel_connection_cost_matches_class_eligibility",
                "avian_topology_control_ports_are_explicit_and_bidirectional",
                "avian_topology_duplicate_advertisement_does_not_refresh_expired_policy",
                "avian_topology_provider_without_policy_support_fails_closed",
            ]
        )
    if arguments.priority_forwarding:
        patches.append(QUALIFICATION / "patches" / "congestion.patch")
        required_tests.update(
            [
                "avian_congestion_bulk_capacity_does_not_consume_control_reserve",
                "avian_congestion_full_reliable_classes_do_not_block_shared_dispatch",
                "avian_congestion_weighted_service_preserves_class_fifo_and_bulk_progress",
            ]
        )
    for patch in patches:
        if not patch.is_file():
            parser.error(f"required patch is missing: {patch}")
    patch_contents = {patch.name: patch.read_bytes() for patch in patches}
    container = "avian-hop-test-" + uuid.uuid4().hex[:12]
    report = {
        "gate": "authenticated_hop_forwarding",
        "source_commit": revision,
        "patch_sha256": {
            name: hashlib.sha256(contents).hexdigest()
            for name, contents in patch_contents.items()
        },
        "status": "error",
        "qualified_for_cutover": False,
        "scope": "engine tests; platform, traffic policy, and hardware qualification remain separate",
    }
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    log_path = arguments.output.with_suffix(".log")
    log = ""
    try:
        with tempfile.TemporaryDirectory(prefix="avian-hop-source-") as temporary:
            checkout = Path(temporary) / "source"
            execute(["git", "clone", "--no-hardlinks", str(source), str(checkout)])
            for patch in patches:
                snapshot = Path(temporary) / patch.name
                snapshot.write_bytes(patch_contents[patch.name])
                execute(["git", "apply", "--check", str(snapshot)], checkout)
                execute(["git", "apply", str(snapshot)], checkout)
            command = [
                "docker",
                "run",
                "--rm",
                "--name",
                container,
                "--mount",
                f"type=bind,source={checkout},target=/source",
                "--mount",
                "type=volume,source=avian-engine-cargo,target=/usr/local/cargo",
                "--mount",
                "type=volume,source=avian-hop-target,target=/build",
                "-w",
                "/source",
                "-e",
                "RUSTUP_TOOLCHAIN=1.95.0",
                "-e",
                "CARGO_TARGET_DIR=/build",
                "-e",
                "CARGO_BUILD_JOBS=4",
                "-e",
                "CARGO_PROFILE_DEV_DEBUG=0",
                "-e",
                "CARGO_INCREMENTAL=0",
                arguments.image,
                "cargo",
                "test",
                "--locked",
                "-p",
                "easytier",
                "--lib",
                "--no-default-features",
                "--features",
                "tun,aes-gcm",
                test_filter,
                "--",
                "--nocapture",
            ]
            report["command"] = command
            completed = subprocess.run(
                command, capture_output=True, text=True, timeout=arguments.timeout
            )
            log = completed.stdout + completed.stderr
            baseline_command = command.copy()
            baseline_command[baseline_command.index(test_filter)] = (
                "peer_conn_secure_mode"
            )
            baseline_command[baseline_command.index(container)] = (
                container + "-baseline"
            )
            baseline = subprocess.run(
                baseline_command,
                capture_output=True,
                text=True,
                timeout=arguments.timeout,
            )
            log += baseline.stdout + baseline.stderr
            results = re.findall(
                r"^test (\S*avian_\S*) \.\.\. (ok|FAILED)$", log, re.MULTILINE
            )
            baseline_results = re.findall(
                r"^test (\S*peer_conn_secure_mode\S*) \.\.\. (ok|FAILED)$",
                log,
                re.MULTILINE,
            )
            report["tests"] = [
                {"name": name, "status": status} for name, status in results
            ]
            report["baseline_tests"] = [
                {"name": name, "status": status} for name, status in baseline_results
            ]
            executed = {name.rsplit("::", 1)[-1] for name, _ in results}
            report["missing_required_tests"] = sorted(required_tests - executed)
            report["exit_code"] = completed.returncode
            if (
                completed.returncode == 0
                and baseline.returncode == 0
                and results
                and baseline_results
                and not report["missing_required_tests"]
                and all(status == "ok" for _, status in results + baseline_results)
            ):
                report["status"] = "passed"
            elif any(status == "FAILED" for _, status in results + baseline_results):
                report["status"] = "failed"
    except subprocess.TimeoutExpired as problem:
        subprocess.run(
            ["docker", "rm", "--force", container], capture_output=True, check=False
        )
        subprocess.run(
            ["docker", "rm", "--force", container + "-baseline"],
            capture_output=True,
            check=False,
        )
        log = "Qualification timed out.\n"
        for output in [problem.stdout, problem.stderr]:
            if output:
                log += (
                    output.decode(errors="replace")
                    if isinstance(output, bytes)
                    else output
                )
    except subprocess.CalledProcessError as problem:
        log += f"Source preparation failed: {problem}\n"
    log_path.write_text(log)
    report["log"] = str(log_path.resolve())
    arguments.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return {"passed": 0, "failed": 2, "error": 1}[report["status"]]


if __name__ == "__main__":
    raise SystemExit(main())
