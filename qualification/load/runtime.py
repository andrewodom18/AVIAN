"""Measure real mesh growth, control delivery under load, and revocation under load."""

import argparse
import base64
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import secrets
import subprocess
import sys
import time

sys.path.insert(0, "/qualification")
from runtime import Node, namespace_command, run, wait_until


class LoadNode(Node):
    priority_forwarding = False

    @property
    def address(self):
        return str(
            ipaddress.IPv4Address(int(ipaddress.IPv4Address("10.64.0.0")) + self.number)
        )

    @property
    def physical_address(self):
        return str(
            ipaddress.IPv4Address(
                int(ipaddress.IPv4Address("172.20.0.0")) + self.number
            )
        )

    def create(self):
        super().create()
        namespace_command(
            self.name, ["ip", "address", "flush", "dev", f"peer{self.number}"]
        )
        namespace_command(
            self.name,
            [
                "ip",
                "address",
                "add",
                self.physical_address + "/16",
                "dev",
                f"peer{self.number}",
            ],
        )

    def configure(self, **options):
        super().configure(**options)
        path = Path("/state") / (self.name + ".toml")
        path.write_text(
            path.read_text().replace(
                f'ipv4 = "{self.address}"', f'ipv4 = "{self.address}/16"'
            )
        )

    def start(self):
        self.log_handle = Path(f"/state/{self.name}.log").open("w")
        environment = {
            key: value for key, value in os.environ.items() if not key.startswith("ET_")
        }
        environment["ET_CREDENTIAL"] = self.credential
        environment["ET_REQUIRE_VERIFIED_MEMBERSHIP"] = "1"
        environment["ET_FAST_FAILOVER"] = "1"
        if self.priority_forwarding:
            environment["ET_PRIORITY_FORWARDING"] = "1"
        self.process = subprocess.Popen(
            [
                "ip",
                "netns",
                "exec",
                self.name,
                "/engine/easytier-core",
                "--config-file",
                f"/state/{self.name}.toml",
                "--console-log-level",
                "warn",
            ],
            stdout=self.log_handle,
            stderr=subprocess.STDOUT,
            env=environment,
        )


def physical_bytes(node):
    interface = json.loads(
        namespace_command(
            node.name, ["ip", "-s", "-j", "link", "show", "dev", f"peer{node.number}"]
        ).stdout
    )[0]
    return interface.get("stats64", interface.get("stats"))["tx"]["bytes"]


def resources(nodes):
    ticks = 0
    memory_kib = 0
    active_processes = 0
    for node in nodes:
        if node.process is None or node.process.poll() is not None:
            continue
        active_processes += 1
        process_path = Path("/proc") / str(node.process.pid)
        fields = (process_path / "stat").read_text().split()
        ticks += int(fields[13]) + int(fields[14])
        memory_kib += node.memory_kib() or 0
    return {
        "engine_processes": active_processes,
        "memory_kib": memory_kib,
        "cpu_seconds": ticks / os.sysconf("SC_CLK_TCK"),
        "underlay_tx_bytes": sum(physical_bytes(node) for node in nodes),
    }


def credential_record(node):
    private_key = base64.b64encode(secrets.token_bytes(32)).decode()
    private_der = bytes.fromhex("302e020100300506032b656e04220420") + base64.b64decode(
        private_key
    )
    public_der = subprocess.run(
        ["openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
        input=private_der,
        check=True,
        capture_output=True,
    ).stdout
    return private_key, {
        "pubkey": base64.b64encode(public_der[-32:]).decode(),
        "groups": [],
        "allow_relay": True,
        "allowed_proxy_cidrs": [],
        "overlay_addresses": [node.address],
        "reusable": False,
        "expiry_unix": int(time.time()) + 600,
        "created_at_unix": int(time.time()),
    }


def publish_trust(nodes, trust):
    for node in nodes:
        destination = Path(f"/state/{node.name}-credentials.json")
        temporary = destination.with_suffix(".tmp")
        temporary.write_text(
            json.dumps({"format": "avian-verified-membership-v1", "credentials": trust})
        )
        temporary.chmod(0o600)
        temporary.replace(destination)


def worker(node, *arguments):
    return subprocess.Popen(
        [
            "ip",
            "netns",
            "exec",
            node.name,
            "python3",
            "/qualification/load/traffic.py",
            *arguments,
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def stop_worker(process):
    if process.poll() is None:
        process.terminate()
    try:
        process.wait(timeout=2)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def control_completed(process, path, duration):
    records = [json.loads(line) for line in path.read_text().splitlines()]
    metadata = next((record for record in records if "total_sent" in record), {})
    return (
        process.returncode == 0
        and metadata.get("completed") is True
        and metadata.get("sending_finished", 0) - metadata.get("started", 0) >= duration
        and metadata.get("total_sent", 0) >= duration * 16
    )


def flood_result(process, path, duration, offered_bps):
    process.wait(timeout=3)
    result = json.loads(path.read_text()) if path.exists() else {}
    expected_datagrams = duration * offered_bps / (1100 * 8)
    result["generator_passed"] = (
        process.returncode == 0
        and result.get("completed") is True
        and result.get("finished", 0) - result.get("started", 0) >= duration
        and result.get("sent_bytes", 0) >= expected_datagrams * 1100 * 0.95
    )
    return result


def analyze_control(path, started, ended):
    records = [json.loads(line) for line in path.read_text().splitlines()]
    deliveries = [record for record in records if "received" in record]
    sent = next(
        (record["total_sent"] for record in records if "total_sent" in record), None
    )
    active_end = next(
        (
            record["sending_finished"]
            for record in records
            if "sending_finished" in record
        ),
        ended,
    )
    times = (
        [started]
        + [
            record["received"]
            for record in deliveries
            if record["received"] <= active_end
        ]
        + [active_end]
    )
    rtts = sorted(record["round_trip_seconds"] for record in deliveries)
    return {
        "sent": sent,
        "received": len(deliveries),
        "loss_fraction": (
            None
            if not sent
            else max(0, 1 - len({record["sequence"] for record in deliveries}) / sent)
        ),
        "maximum_delivery_gap_seconds": max(
            right - left for left, right in zip(times, times[1:])
        ),
        "maximum_round_trip_seconds": max(rtts) if rtts else None,
        "p95_round_trip_seconds": (
            rtts[min(len(rtts) - 1, int(len(rtts) * 0.95))] if rtts else None
        ),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--priority-forwarding", action="store_true")
    parser.add_argument("--growth-seconds", type=float, required=True)
    parser.add_argument("--memory-budget-mib", type=int, required=True)
    parser.add_argument("--load-seconds", type=float, required=True)
    parser.add_argument("--bottleneck-bps", type=int, required=True)
    arguments = parser.parse_args()
    report = {
        "scope": "Real Linux processes and packet forwarding; synthetic public trust fixture; no signed-enrollment claim.",
        "binary_sha256": hashlib.sha256(
            Path("/engine/easytier-core").read_bytes()
        ).hexdigest(),
        "qualified_for_cutover": False,
        "priority_forwarding": arguments.priority_forwarding,
        "growth": [],
        "gates": [],
        "budgets": {
            "growth_admission_seconds": arguments.growth_seconds,
            "memory_admission_mib": arguments.memory_budget_mib,
            "load_seconds": arguments.load_seconds,
            "join_completion_seconds": 15,
        },
    }
    nodes = []
    workers = []
    trust = {}
    try:
        run(["ip", "link", "add", "underlay", "type", "bridge"])
        run(["ip", "link", "set", "underlay", "up"])
        source, relay, destination = [
            LoadNode(name, index)
            for name, index in [("source", 1), ("relay", 2), ("destination", 3)]
        ]
        nodes = [source, relay, destination]
        for node in nodes:
            node.priority_forwarding = arguments.priority_forwarding
        keys = {}
        for node in nodes:
            node.create()
            keys[node.name], trust[node.name] = credential_record(node)
            node.configure(
                credential=keys[node.name], peers=[] if node is relay else [relay]
            )
        publish_trust(nodes, trust)
        for node in nodes:
            node.start()
        if not wait_until(lambda: source.ping(destination), 25):
            raise RuntimeError("baseline mesh did not become reachable")
        baseline = resources(nodes)
        idle_started = time.monotonic()
        time.sleep(2)
        idle_ended = time.monotonic()
        idle_after = resources(nodes)
        report["background"] = {
            "duration_seconds": idle_ended - idle_started,
            "underlay_tx_bytes": idle_after["underlay_tx_bytes"]
            - baseline["underlay_tx_bytes"],
            "cpu_seconds": idle_after["cpu_seconds"] - baseline["cpu_seconds"],
            "resources": idle_after,
        }
        growth_started = time.monotonic()
        while time.monotonic() - growth_started < arguments.growth_seconds:
            sample = resources(nodes)
            sample["elapsed_seconds"] = time.monotonic() - growth_started
            report["growth"].append(sample)
            largest_process_kib = max((node.memory_kib() or 0) for node in nodes)
            reserved_join_kib = largest_process_kib + 32 * 1024
            sample["reserved_join_kib"] = reserved_join_kib
            if (
                sample["memory_kib"] + reserved_join_kib
                >= arguments.memory_budget_mib * 1024
            ):
                report["growth_stop_reason"] = (
                    "memory admission budget would be exceeded"
                )
                break
            node = LoadNode(f"leaf{len(nodes) + 1}", len(nodes) + 1)
            node.priority_forwarding = arguments.priority_forwarding
            node.create()
            keys[node.name], trust[node.name] = credential_record(node)
            node.configure(credential=keys[node.name], peers=[relay])
            nodes.append(node)
            publish_trust(nodes, trust)
            started = time.monotonic()
            node.start()
            connected = wait_until(lambda: node.ping(destination), 15)
            sample["join_convergence_seconds"] = time.monotonic() - started
            sample["join_reachable"] = connected
            if not connected:
                report["growth_stop_reason"] = (
                    "admitted join did not converge within 15 seconds"
                )
                break
        else:
            report["growth_stop_reason"] = "growth time budget reached"
        report["gates"].append(
            {
                "name": "admitted_joins_reachable",
                "status": (
                    "pass"
                    if all(
                        sample.get("join_reachable", True)
                        for sample in report["growth"]
                    )
                    else "fail"
                ),
            }
        )
        report["resources_after_growth"] = resources(nodes)
        report["direct_peers"] = {
            node.name: sum(row.get("cost") == "p2p" for row in node.command("peer"))
            for node in nodes
            if node.ready()
        }
        report["local_session_counts"] = {
            node.name: sum(
                len((row.get("peer") or {}).get("conns", []))
                for row in node.command("--verbose", "peer")
            )
            for node in nodes
            if node.ready()
        }
        if len(nodes) > 3:
            changing = nodes[-1]
            changing.stop()
            time.sleep(1)
            started = time.monotonic()
            changing.start()
            returned = wait_until(lambda: changing.ping(destination), 15)
            report["leave_rejoin"] = {
                "reachable": returned,
                "convergence_seconds": time.monotonic() - started,
            }
            report["gates"].append(
                {
                    "name": "cold_rejoin_reachable",
                    "status": "pass" if returned else "fail",
                }
            )
        # Shape one physical egress. All application classes share this authenticated relay connection.
        namespace_command(
            relay.name,
            [
                "tc",
                "qdisc",
                "replace",
                "dev",
                f"peer{relay.number}",
                "root",
                "tbf",
                "rate",
                str(arguments.bottleneck_bps) + "bit",
                "burst",
                "16384",
                "latency",
                "50ms",
            ],
        )
        report["bottleneck_bps"] = arguments.bottleneck_bps
        received_bytes_path = Path("/state/received-bytes.json")
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
        control_path = Path("/state/control-load.jsonl")
        started = time.monotonic()
        before = resources(nodes)
        control = worker(
            source,
            "control",
            destination.address,
            "--duration",
            str(arguments.load_seconds),
            "--output",
            str(control_path),
        )
        workers.append(control)
        load_generators = {}
        for mode in ["video", "bulk"]:
            path = Path("/state") / (mode + "-load.json")
            process = worker(
                source,
                mode,
                destination.address,
                "--duration",
                str(arguments.load_seconds),
                "--offered-bps",
                str(arguments.bottleneck_bps * 2),
                "--output",
                str(path),
            )
            workers.append(process)
            load_generators[mode] = (process, path)
        time.sleep(arguments.load_seconds / 2)
        midpoint_bytes = json.loads(received_bytes_path.read_text())
        control.wait(timeout=arguments.load_seconds + 5)
        ended = time.monotonic()
        result = analyze_control(control_path, started, ended)
        result["offered_video_bps"] = arguments.bottleneck_bps * 2
        result["offered_bulk_bps"] = arguments.bottleneck_bps * 2
        result["resource_before"] = before
        result["resource_after"] = resources(nodes)
        result["received_payload_bytes"] = json.loads(received_bytes_path.read_text())
        result["midpoint_received_payload_bytes"] = midpoint_bytes
        result["generators"] = {
            mode: flood_result(
                process, path, arguments.load_seconds, arguments.bottleneck_bps * 2
            )
            for mode, (process, path) in load_generators.items()
        }
        result["control_generator_completed"] = control_completed(
            control, control_path, arguments.load_seconds
        )
        report["congestion"] = result
        protected = (
            bool(result["received"])
            and result["loss_fraction"] <= 0.05
            and result["maximum_delivery_gap_seconds"] <= 2
            and result["maximum_round_trip_seconds"] <= 2
            and result["p95_round_trip_seconds"] <= 0.5
        )
        report["control_objective"] = {
            "minimum_delivery_fraction": 0.95,
            "maximum_gap_seconds": 2,
            "maximum_round_trip_seconds": 2,
            "maximum_p95_round_trip_seconds": 0.5,
        }
        other_classes_progress = all(
            result["received_payload_bytes"][port] > midpoint_bytes[port] > 0
            for port in ["8000", "19001"]
        )
        generators_completed = result["control_generator_completed"] and all(
            generator["generator_passed"] for generator in result["generators"].values()
        )
        report["gates"].append(
            {
                "name": "congestion_generators_sustained",
                "status": "pass" if generators_completed else "fail",
            }
        )
        report["gates"].append(
            {
                "name": "video_and_bulk_make_progress",
                "status": "pass" if other_classes_progress else "fail",
            }
        )
        report["gates"].append(
            {
                "name": "control_delivery_under_competing_load",
                "status": "pass" if protected else "fail",
            }
        )
        # Revocation enforcement under continuing offered traffic; signed delivery is tested separately.
        for process in workers[1:]:
            stop_worker(process)
        if len(nodes) < 4:
            raise RuntimeError(
                "Loaded revocation requires an admitted workload sink; increase the growth resource budget"
            )
        workload_sink = nodes[3]
        sink_bytes_path = Path("/state/revocation-sink-bytes.json")
        workers.append(
            worker(
                workload_sink,
                "serve",
                workload_sink.address,
                "--output",
                str(sink_bytes_path),
            )
        )
        time.sleep(0.2)
        revocation_path = Path("/state/control-revocation.jsonl")
        control = worker(
            source,
            "control",
            destination.address,
            "--duration",
            "6",
            "--output",
            str(revocation_path),
        )
        workers.append(control)
        revocation_generators = {}
        for mode in ["video", "bulk"]:
            path = Path("/state") / (mode + "-revocation.json")
            process = worker(
                source,
                mode,
                workload_sink.address,
                "--duration",
                "6",
                "--offered-bps",
                str(arguments.bottleneck_bps * 2),
                "--output",
                str(path),
            )
            workers.append(process)
            revocation_generators[mode] = (process, path)
        if not wait_until(
            lambda: revocation_path.exists()
            and len(revocation_path.read_text().splitlines()) >= 3,
            3,
        ):
            raise RuntimeError("control baseline unavailable before loaded revocation")
        trust.pop(destination.name)
        revoked_at = time.monotonic()
        publish_trust(nodes, trust)
        time.sleep(3)
        post_revocation_midpoint = json.loads(sink_bytes_path.read_text())
        control.wait(timeout=8)
        received_after_deadline = [
            json.loads(line)
            for line in revocation_path.read_text().splitlines()
            if "received" in json.loads(line)
            and json.loads(line)["received"] > revoked_at + 2
        ]
        completed = control_completed(control, revocation_path, 6)
        sink_received = json.loads(sink_bytes_path.read_text())
        sink_progress = all(
            sink_received[port] > post_revocation_midpoint[port] > 0
            for port in ["8000", "19001"]
        )
        generator_results = {
            mode: flood_result(process, path, 6, arguments.bottleneck_bps * 2)
            for mode, (process, path) in revocation_generators.items()
        }
        report["loaded_revocation"] = {
            "received_after_two_seconds": len(received_after_deadline),
            "control_generator_completed": completed,
            "generators": generator_results,
            "competing_load_destination": workload_sink.name,
            "authorized_sink_payload_bytes_at_three_seconds": post_revocation_midpoint,
            "authorized_sink_payload_bytes_after": sink_received,
        }
        report["gates"].append(
            {
                "name": "authorized_load_continues_after_revocation",
                "status": "pass" if sink_progress else "fail",
            }
        )
        report["gates"].append(
            {
                "name": "revocation_generators_sustained",
                "status": (
                    "pass"
                    if completed
                    and all(
                        generator["generator_passed"]
                        for generator in generator_results.values()
                    )
                    else "fail"
                ),
            }
        )
        report["gates"].append(
            {
                "name": "local_revocation_under_load",
                "status": "pass" if not received_after_deadline else "fail",
            }
        )
        report["status"] = (
            "pass"
            if all(gate["status"] == "pass" for gate in report["gates"])
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
