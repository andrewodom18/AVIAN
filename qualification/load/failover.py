"""Measure control recovery while an observed busy relay fails under competing loads."""

import argparse
import hashlib
import json
from pathlib import Path
import sys
import time

sys.path.insert(0, "/qualification")
from load.runtime import (
    LoadNode,
    analyze_control,
    control_completed,
    credential_record,
    flood_result,
    namespace_command,
    physical_bytes,
    publish_trust,
    resources,
    run,
    stop_worker,
    wait_until,
    worker,
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--priority-forwarding", action="store_true")
    parser.add_argument(
        "--fault", choices=["blackhole", "relay-stop"], default="blackhole"
    )
    parser.add_argument("--bottleneck-bps", type=int, required=True)
    arguments = parser.parse_args()
    report = {
        "scope": "Real isolated Linux forwarding; synthetic public trust fixture.",
        "qualified_for_cutover": False,
        "fault": arguments.fault,
        "binary_sha256": hashlib.sha256(
            Path("/engine/easytier-core").read_bytes()
        ).hexdigest(),
        "priority_forwarding": arguments.priority_forwarding,
    }
    nodes = []
    workers = []
    try:
        run(["ip", "link", "add", "underlay", "type", "bridge"])
        run(["ip", "link", "set", "underlay", "up"])
        source, relay_a, relay_b, destination = [
            LoadNode(name, number)
            for number, name in enumerate(
                ["source", "relay_a", "relay_b", "destination"], 1
            )
        ]
        nodes = [source, relay_a, relay_b, destination]
        relays = [relay_a, relay_b]
        trust = {}
        for node in nodes:
            node.priority_forwarding = arguments.priority_forwarding
            node.create()
            private_key, trust[node.name] = credential_record(node)
            node.configure(
                credential=private_key,
                peers=relays if node in [source, destination] else [],
            )
        publish_trust(nodes, trust)
        # A slower alternate makes the preferred control path deterministic in this fixture.
        namespace_command(
            relay_b.name,
            [
                "tc",
                "qdisc",
                "add",
                "dev",
                f"peer{relay_b.number}",
                "root",
                "netem",
                "delay",
                "250ms",
            ],
        )
        for node in nodes:
            node.start()
        if not wait_until(lambda: source.ping(destination), 25):
            raise RuntimeError("diamond baseline did not become reachable")
        if not wait_until(
            lambda: all(
                sum(row.get("cost") == "p2p" for row in node.command("peer")) >= 2
                for node in [source, destination]
            ),
            10,
        ):
            raise RuntimeError("alternate neighbor connections unavailable")
        idle_before = {node.name: physical_bytes(node) for node in relays}
        idle_started = time.monotonic()
        time.sleep(2)
        idle_duration = time.monotonic() - idle_started
        idle_rates = {
            node.name: (physical_bytes(node) - idle_before[node.name]) / idle_duration
            for node in relays
        }
        received_bytes_path = Path("/state/failover-received.json")
        workers.append(
            worker(
                destination,
                "serve",
                destination.address,
                "--output",
                str(received_bytes_path),
            )
        )
        time.sleep(0.2)
        witness_before = {node.name: physical_bytes(node) for node in relays}
        witness_started = time.monotonic()
        witness = worker(
            source,
            "control",
            destination.address,
            "--duration",
            "2",
            "--output",
            "/state/path-witness.jsonl",
            "--interval-seconds",
            "0.005",
            "--payload-bytes",
            "1100",
        )
        workers.append(witness)
        witness.wait(timeout=5)
        if not control_completed(witness, Path("/state/path-witness.jsonl"), 2):
            raise RuntimeError("Control path witness generator did not complete")
        witness_duration = time.monotonic() - witness_started
        control_workload = {
            node.name: max(
                0,
                physical_bytes(node)
                - witness_before[node.name]
                - idle_rates[node.name] * witness_duration,
            )
            for node in relays
        }
        primary = max(relays, key=lambda node: control_workload[node.name])
        alternate = next(node for node in relays if node is not primary)
        report["control_path_witness"] = {
            "excess_underlay_bytes": control_workload,
            "selected_relay": primary.name,
            "alternate_fixture_delay_ms": 250,
            "duration_seconds": witness_duration,
        }
        if (
            control_workload[primary.name] < 1024
            or control_workload[primary.name] <= 2 * control_workload[alternate.name]
        ):
            raise RuntimeError("control path witness ambiguous")
        for node in relays:
            namespace_command(
                node.name,
                [
                    "tc",
                    "qdisc",
                    "replace",
                    "dev",
                    f"peer{node.number}",
                    "root",
                    "handle",
                    "1:",
                    "tbf",
                    "rate",
                    str(arguments.bottleneck_bps) + "bit",
                    "burst",
                    "16384",
                    "latency",
                    "50ms",
                ],
            )
        namespace_command(
            relay_b.name,
            [
                "tc",
                "qdisc",
                "add",
                "dev",
                f"peer{relay_b.number}",
                "parent",
                "1:1",
                "handle",
                "10:",
                "netem",
                "delay",
                "250ms",
                "limit",
                "64",
            ],
        )
        control_path = Path("/state/failover-control.jsonl")
        started = time.monotonic()
        before = resources(nodes)
        control = worker(
            source,
            "control",
            destination.address,
            "--duration",
            "16",
            "--output",
            str(control_path),
        )
        workers.append(control)
        load_generators = {}
        for mode in ["video", "bulk"]:
            path = Path("/state") / (mode + "-failover.json")
            process = worker(
                source,
                mode,
                destination.address,
                "--duration",
                "16",
                "--offered-bps",
                str(arguments.bottleneck_bps * 2),
                "--output",
                str(path),
            )
            workers.append(process)
            load_generators[mode] = (process, path)
        time.sleep(3)
        active_before = {node.name: physical_bytes(node) for node in relays}
        active_started = time.monotonic()
        time.sleep(2)
        before_fault = {node.name: physical_bytes(node) for node in relays}
        active_duration = time.monotonic() - active_started
        active_workload = {
            node.name: max(
                0,
                before_fault[node.name]
                - active_before[node.name]
                - idle_rates[node.name] * active_duration,
            )
            for node in relays
        }
        report["path_witness"] = {
            "background_underlay_bytes_per_second": idle_rates,
            "active_interval_seconds": active_duration,
            "excess_workload_bytes": active_workload,
            "faulted_relay": primary.name,
            "alternate_relay": alternate.name,
            "selection": "Control-only socket traffic witness, corroborated under competing load",
        }
        if (
            active_workload[primary.name] < 1024
            or active_workload[primary.name] <= 2 * active_workload[alternate.name]
        ):
            raise RuntimeError(
                "workload path ambiguous; refusing to select fault from generic route display"
            )
        before_fault_received = json.loads(received_bytes_path.read_text())
        fault_at = time.monotonic()
        if arguments.fault == "relay-stop":
            primary.stop()
        else:
            for node in [source, destination]:
                for chain, flag in [("OUTPUT", "-d"), ("INPUT", "-s")]:
                    namespace_command(
                        node.name,
                        [
                            "iptables",
                            "-I",
                            chain,
                            flag,
                            primary.physical_address,
                            "-j",
                            "DROP",
                        ],
                    )
        control.wait(timeout=20)
        ended = time.monotonic()
        generator_results = {
            mode: flood_result(process, path, 16, arguments.bottleneck_bps * 2)
            for mode, (process, path) in load_generators.items()
        }
        generators_completed = control_completed(control, control_path, 16) and all(
            result["generator_passed"] for result in generator_results.values()
        )
        after_fault_received = json.loads(received_bytes_path.read_text())
        other_classes_progress = all(
            after_fault_received[port] > before_fault_received[port] > 0
            for port in ["8000", "19001"]
        )
        report["generators"] = generator_results
        report["control_generator_completed"] = control_completed(
            control, control_path, 16
        )
        report["received_payload_bytes_before_fault"] = before_fault_received
        report["received_payload_bytes_after_fault"] = after_fault_received
        report["video_bulk_progress_across_fault"] = other_classes_progress
        records = [json.loads(line) for line in control_path.read_text().splitlines()]
        deliveries = [record for record in records if "received" in record]
        prior = [record for record in deliveries if record["received"] < fault_at]
        after = [record for record in deliveries if record["received"] >= fault_at]
        times = ([prior[-1]["received"]] if prior else [fault_at]) + [
            record["received"] for record in after
        ]
        gaps = [right - left for left, right in zip(times, times[1:])]
        active_end = next(
            (
                record["sending_finished"]
                for record in records
                if "sending_finished" in record
            ),
            started + 16,
        )
        gaps.append(max(0, active_end - times[-1]))
        post_duration = ended - fault_at
        post_workload = {
            node.name: max(
                0,
                physical_bytes(node)
                - before_fault[node.name]
                - idle_rates[node.name] * post_duration,
            )
            for node in relays
        }
        alternate_observed = (
            post_workload[alternate.name] > 1024
            and post_workload[alternate.name] > 2 * post_workload[primary.name]
        )
        report["path_witness"]["post_fault_excess_workload_bytes"] = post_workload
        report["path_witness"]["alternate_carries_workload"] = alternate_observed
        report["control"] = analyze_control(control_path, started, ended)
        report["recovery"] = {
            "delivery_before_fault": len(prior),
            "delivery_after_fault": len(after),
            "first_delivery_after_fault_seconds": (
                after[0]["received"] - fault_at if after else None
            ),
            "maximum_delivery_gap_seconds": max(gaps),
            "maximum_gap_objective_seconds": 2,
        }
        report["resources_before"] = before
        report["resources_after"] = resources(nodes)
        report["status"] = (
            "pass"
            if (
                prior
                and after
                and max(gaps) <= 2
                and alternate_observed
                and generators_completed
                and other_classes_progress
            )
            else "fail"
        )
    except Exception as error:
        report["status"] = "error"
        report["error"] = str(error)
    finally:
        for process in workers:
            stop_worker(process)
        for node in nodes:
            node.stop()
        Path("/results/runtime.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2))
    return 0 if report["status"] == "pass" else 2


if __name__ == "__main__":
    raise SystemExit(main())
