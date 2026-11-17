"""Generate and measure competing traffic for signed-distribution checks."""

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time

sys.path.insert(0, "/qualification")
from runtime import Node, STATE, namespace_command, wait_until


class DistributionNode(Node):
    priority_forwarding = False

    def start(self):
        if not self.priority_forwarding:
            return super().start()
        self.log_handle = (STATE / (self.name + ".log")).open("w")
        environment = {
            name: value
            for name, value in os.environ.items()
            if not name.startswith("ET_")
        }
        environment["ET_CREDENTIAL"] = self.credential
        environment["ET_REQUIRE_VERIFIED_MEMBERSHIP"] = "1"
        environment["ET_PRIORITY_FORWARDING"] = "1"
        self.process = subprocess.Popen(
            [
                "ip",
                "netns",
                "exec",
                self.name,
                "/engine/easytier-core",
                "--config-file",
                str(STATE / (self.name + ".toml")),
                "--console-log-level",
                "warn",
            ],
            stdout=self.log_handle,
            stderr=subprocess.STDOUT,
            env=environment,
        )


def observations(path):
    if not path.exists():
        return []
    lines = path.read_text().splitlines()
    records = []
    for line in lines:
        try:
            records.append(json.loads(line))
        except json.JSONDecodeError:
            # A reader may observe the final line while its writer is appending.
            continue
    return records


def serve(address, output):
    counters = {"video": 0, "bulk": 0}

    def receive(name, port):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as listener:
            listener.bind((address, port))
            while True:
                payload = listener.recv(65535)
                counters[name] += len(payload)

    for name, port in (("video", 8000), ("bulk", 19001)):
        threading.Thread(target=receive, args=(name, port), daemon=True).start()
    with Path(output).open("w", buffering=1) as log:
        while True:
            log.write(json.dumps({"at": time.monotonic(), **counters}) + "\n")
            time.sleep(0.25)


def flood(address, name, output):
    port = 8000 if name == "video" else 19001
    payload = b"x" * 1100
    interval = len(payload) * 8 / 4_000_000
    offered_bytes = 0
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender, Path(output).open(
        "w", buffering=1
    ) as log:
        sender.connect((address, port))
        next_send = time.monotonic()
        next_report = next_send
        while True:
            sender.send(payload)
            offered_bytes += len(payload)
            now = time.monotonic()
            if now >= next_report:
                log.write(
                    json.dumps({"at": now, "offered_bytes": offered_bytes}) + "\n"
                )
                next_report = now + 0.25
            next_send += interval
            time.sleep(max(0, next_send - time.monotonic()))


class CompetingLoad:
    def __init__(self, source, relay, destination, processes):
        self.source = source
        self.relay = relay
        self.destination = destination
        self.processes = processes
        self.workers = []
        self.receiver_path = STATE / "competing-received.jsonl"
        self.sender_paths = {
            name: STATE / ("competing-" + name + ".jsonl") for name in ("video", "bulk")
        }

    def launch(self, node, *arguments):
        process = subprocess.Popen(
            [
                "ip",
                "netns",
                "exec",
                node.name,
                "python3",
                "/qualification/revocation-distribution/competing.py",
                *arguments,
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.workers.append(process)
        self.processes.append(process)

    def start(self):
        namespace_command(
            self.relay.name,
            [
                "tc",
                "qdisc",
                "replace",
                "dev",
                f"peer{self.relay.number}",
                "root",
                "tbf",
                "rate",
                "2000000bit",
                "burst",
                "16384",
                "latency",
                "50ms",
            ],
        )
        self.launch(
            self.destination, "serve", self.destination.address, str(self.receiver_path)
        )
        time.sleep(0.3)
        for name, path in self.sender_paths.items():
            self.launch(self.source, name, self.destination.address, str(path))
        if not wait_until(
            lambda: bool(observations(self.receiver_path))
            and all(
                observations(self.receiver_path)[-1][name] > 10000
                for name in self.sender_paths
            ),
            10,
        ):
            raise RuntimeError("Competing video and bulk delivery did not become ready")
        self.started = time.monotonic()
        time.sleep(1.5)

    def measure(self, issued_at, received_at):
        ended = time.monotonic()
        receivers = observations(self.receiver_path)
        windows = []
        for label, start, finish in (
            ("before_revocation", self.started, issued_at),
            ("revocation_and_enforcement", issued_at, ended),
        ):
            received = [row for row in receivers if start <= row["at"] <= finish]
            details = {"name": label, "duration_seconds": finish - start}
            details["received_bytes"] = {
                name: (
                    received[-1][name] - received[0][name] if len(received) >= 2 else 0
                )
                for name in self.sender_paths
            }
            offered = {}
            offered_rates = {}
            sender_observation_seconds = {}
            maximum_gaps = {}
            for name, path in self.sender_paths.items():
                rows = [
                    row for row in observations(path) if start <= row["at"] <= finish
                ]
                offered[name] = (
                    rows[-1]["offered_bytes"] - rows[0]["offered_bytes"]
                    if len(rows) >= 2
                    else 0
                )
                observed_seconds = (
                    rows[-1]["at"] - rows[0]["at"] if len(rows) >= 2 else 0
                )
                sender_observation_seconds[name] = observed_seconds
                offered_rates[name] = (
                    offered[name] * 8 / observed_seconds if observed_seconds else 0
                )
                points = [start] + [row["at"] for row in rows] + [finish]
                maximum_gaps[name] = max(
                    right - left for left, right in zip(points, points[1:])
                )
            details["offered_bytes"] = offered
            details["observed_offered_bps"] = offered_rates
            details["sender_observation_seconds"] = sender_observation_seconds
            details["maximum_sender_observation_gap_seconds"] = maximum_gaps
            details["active"] = (
                all(value > 1000 for value in details["received_bytes"].values())
                and all(value <= 1 for value in maximum_gaps.values())
                and all(rate >= 4_000_000 * 0.95 for rate in offered_rates.values())
            )
            windows.append(details)
        qdisc = json.loads(
            namespace_command(
                self.relay.name,
                [
                    "tc",
                    "-s",
                    "-j",
                    "qdisc",
                    "show",
                    "dev",
                    f"peer{self.relay.number}",
                ],
            ).stdout
        )
        return {
            "bottleneck_bps": 2_000_000,
            "offered_bps_per_class": 4_000_000,
            "target": "unrevoked_administrator",
            "signed_revision_received_seconds": (
                None if received_at is None else received_at - issued_at
            ),
            "windows": windows,
            "qdisc": qdisc,
            "workers_alive": all(process.poll() is None for process in self.workers),
            "active_at_signed_receipt": received_at is not None
            and all(
                any(abs(row["at"] - received_at) <= 0.75 for row in observations(path))
                for path in self.sender_paths.values()
            ),
        }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("serve", "video", "bulk"))
    parser.add_argument("address")
    parser.add_argument("output")
    arguments = parser.parse_args()
    if arguments.mode == "serve":
        serve(arguments.address, arguments.output)
    else:
        flood(arguments.address, arguments.mode, arguments.output)
