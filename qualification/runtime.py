#!/usr/bin/env python3
"""Measure engine behavior inside disposable network namespaces."""

import argparse
import base64
import hashlib
from dataclasses import dataclass
import json
import os
from pathlib import Path
import secrets
import re
import subprocess
import time

STATE = Path("/state")
ENGINE = Path("/engine")


def run(arguments, *, check=True, timeout=8):
    return subprocess.run(arguments, check=check, text=True, capture_output=True, timeout=timeout)


def namespace_command(name, arguments, **options):
    return run(["ip", "netns", "exec", name, *arguments], **options)


@dataclass
class Node:
    name: str
    number: int
    process: subprocess.Popen | None = None
    log_handle: object = None
    credential: str | None = None
    require_verified_membership: bool = False

    @property
    def address(self):
        return f"10.201.0.{self.number}"

    @property
    def physical_address(self):
        return f"172.30.0.{self.number}"

    def create(self):
        run(["ip", "netns", "add", self.name])
        run(["ip", "link", "add", f"host{self.number}", "type", "veth", "peer", "name", f"peer{self.number}"])
        run(["ip", "link", "set", f"peer{self.number}", "netns", self.name])
        run(["ip", "link", "set", f"host{self.number}", "master", "underlay"])
        run(["ip", "link", "set", f"host{self.number}", "up"])
        namespace_command(self.name, ["ip", "link", "set", "lo", "up"])
        namespace_command(self.name, ["ip", "address", "add", self.physical_address + "/24", "dev", f"peer{self.number}"])
        namespace_command(self.name, ["ip", "link", "set", f"peer{self.number}", "up"])

    def configure(self, *, administrator_secret=None, credential=None, peers=()):
        # Private configuration and logs exist only in the container's temporary filesystem.
        self.credential = credential
        configuration = [
            f'instance_name = "{self.name}"',
            f'hostname = "{self.name}"',
            f'ipv4 = "{self.address}"',
            'listeners = ["tcp://0.0.0.0:11010"]',
            'rpc_portal = "127.0.0.1:15888"',
            'stun_servers = ["127.0.0.1:9"]',
            'stun_servers_v6 = ["[::1]:9"]',
        ]
        configuration.append(f'credential_file = "/state/{self.name}-credentials.json"')
        configuration.extend(['[network_identity]', 'network_name = "avian-qualification"'])
        if administrator_secret is not None:
            configuration.append(f'network_secret = "{administrator_secret}"')
        configuration.extend(['[secure_mode]', 'enabled = true'])
        private_key = credential or base64.b64encode(secrets.token_bytes(32)).decode()
        configuration.append(f'local_private_key = "{private_key}"')
        private_der = bytes.fromhex("302e020100300506032b656e04220420") + base64.b64decode(private_key)
        public_der = subprocess.run(
            ["openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
            input=private_der, check=True, capture_output=True,
        ).stdout
        public_key = base64.b64encode(public_der[-32:]).decode()
        configuration.append(f'local_public_key = "{public_key}"')
        for peer in peers:
            configuration.extend(['[[peer]]', f'uri = "tcp://{peer.physical_address}:11010"'])
        configuration.extend([
            '[flags]', 'disable_p2p = true', 'disable_udp_hole_punching = true',
            'disable_tcp_hole_punching = true', 'disable_upnp = true',
            'accept_dns = false',
        ])
        path = STATE / (self.name + ".toml")
        path.write_text("\n".join(configuration) + "\n")
        path.chmod(0o600)

    def start(self):
        self.log_handle = (STATE / (self.name + ".log")).open("w")
        environment = {key: value for key, value in os.environ.items() if not key.startswith("ET_")}
        if self.credential is not None:
            environment["ET_CREDENTIAL"] = self.credential
        if self.require_verified_membership:
            environment["ET_REQUIRE_VERIFIED_MEMBERSHIP"] = "1"
        self.process = subprocess.Popen(
            ["ip", "netns", "exec", self.name, str(ENGINE / "easytier-core"),
             "--config-file", str(STATE / (self.name + ".toml")),
             "--console-log-level", "debug"],
            stdout=self.log_handle, stderr=subprocess.STDOUT, env=environment,
        )

    def stop(self):
        if self.process is not None:
            self.process.terminate()
            try:
                self.process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()
            self.process = None
        if self.log_handle is not None:
            self.log_handle.close()
            self.log_handle = None

    def command(self, *arguments):
        result = namespace_command(self.name, [str(ENGINE / "easytier-cli"), "-o", "json", *arguments])
        return json.loads(result.stdout)

    def ready(self):
        try:
            self.command("node")
            return True
        except (subprocess.SubprocessError, json.JSONDecodeError):
            return False

    def ping(self, other):
        return namespace_command(self.name, ["ping", "-n", "-c", "1", "-W", "1", other.address], check=False).returncode == 0

    def sees_route(self, other):
        try:
            return other.address in json.dumps(self.command("route"))
        except (subprocess.SubprocessError, json.JSONDecodeError):
            return False

    def memory_kib(self):
        if self.process is None:
            return None
        status = Path(f"/proc/{self.process.pid}/status").read_text()
        for line in status.splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
        return None


def wait_until(predicate, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.2)
    return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timeout", type=float, default=15)
    arguments = parser.parse_args()
    lock = json.loads(Path("/qualification/engine.lock.json").read_text())
    report = {
        "schema_version": 1,
        "engine": lock["version"],
        "source_commit": lock["source_commit"],
        "environment": "isolated Linux container; no external network",
        "gates": [],
        "qualified_for_cutover": False,
    }
    nodes = []

    def record(name, passed, evidence):
        report["gates"].append({"name": name, "status": "pass" if passed else "fail", "evidence": evidence})
        print(f"{name}: {'pass' if passed else 'fail'}", flush=True)

    try:
        actual_version = run([str(ENGINE / "easytier-core"), "--version"]).stdout.strip()
        expected_version = f"easytier-core {lock['version']}-{lock['source_commit'][:8]}"
        if not actual_version.startswith(expected_version):
            raise RuntimeError("Engine binary does not match the pinned source revision")
        report["binary_version"] = actual_version
        report["binary_sha256"] = hashlib.sha256((ENGINE / "easytier-core").read_bytes()).hexdigest()
        STATE.mkdir(mode=0o700, exist_ok=True)
        run(["ip", "link", "add", "underlay", "type", "bridge"])
        run(["ip", "link", "set", "underlay", "up"])
        administrator = Node("administrator", 1)
        relay = Node("relay", 2)
        source = Node("source", 3)
        destination = Node("destination", 4)
        unknown = Node("unknown", 5)
        nodes = [administrator, relay, source, destination, unknown]
        for node in nodes:
            node.create()
        administrator.configure(administrator_secret=secrets.token_hex(32))
        administrator.start()
        if not wait_until(administrator.ready, arguments.timeout):
            raise RuntimeError("Administrator management API did not start")
        credentials = {}
        for node in [relay, source, destination]:
            credentials[node.name] = administrator.command("credential", "generate", "--ttl", "3600", "--allow-relay", "--reusable", "false")
        relay.configure(credential=credentials[relay.name]["credential_secret"], peers=[administrator])
        relay.start()
        if not wait_until(relay.ready, arguments.timeout):
            raise RuntimeError("Relay management API did not start")
        for node in [source, destination]:
            node.configure(credential=credentials[node.name]["credential_secret"], peers=[administrator, relay])
            node.start()
        baseline = wait_until(lambda: source.ping(administrator) and destination.ping(administrator), arguments.timeout)
        record("credential_direct_connectivity", baseline, "Real ICMP traffic from independently credentialed endpoints to the administrator")
        if not baseline:
            report["baseline_diagnostics"] = {}
            for node in [administrator, source, destination, relay]:
                details = {"addresses": namespace_command(node.name, ["ip", "-br", "address"]).stdout}
                for command in ["route", "peer"]:
                    try:
                        details[command] = node.command(command)
                    except Exception:
                        details[command] = "unavailable"
                log = (STATE / (node.name + ".log")).read_text()
                details["errors"] = [re.sub(r"[A-Za-z0-9+/]{40,}={0,2}", "[redacted]", line) for line in log.splitlines() if any(level in line for level in ["ERROR", "WARN"])][-8:]
                report["baseline_diagnostics"][node.name] = details
            raise RuntimeError("Direct credential baseline failed; dependent checks cannot be interpreted")
        ready_routes = wait_until(lambda: source.sees_route(destination), arguments.timeout)
        record("credential_route_discovery", ready_routes, "Destination overlay address appears in source routing information")
        unknown.configure(credential=base64.b64encode(secrets.token_bytes(32)).decode(), peers=[administrator])
        unknown.start()
        if not wait_until(unknown.ready, arguments.timeout):
            raise RuntimeError("Unknown-peer management API did not start")
        unauthorized_connected = wait_until(lambda: unknown.ping(administrator), 3)
        record("unknown_identity_rejected", not unauthorized_connected, "Random unissued credential cannot exchange overlay ICMP with administrator")
        report["baseline_memory_kib"] = {node.name: node.memory_kib() for node in nodes}
        # No source-to-destination application traffic occurred before removing the administrator.
        administrator.stop()
        fresh_session = wait_until(lambda: source.ping(destination), arguments.timeout)
        report["relay_routes"] = source.command("route")
        report["relay_peers"] = source.command("peer")
        record("fresh_session_through_ordinary_relay", fresh_session, "Administrator stopped before first endpoint-to-endpoint ICMP; ordinary relay has no network secret")
        for node in [source, destination, relay]:
            node.stop()
        for node in [relay, source, destination]:
            node.start()
        offline_reconnect = wait_until(lambda: source.ping(destination), arguments.timeout)
        record("offline_restart_without_administrator", offline_reconnect, "All ordinary nodes restarted with the same private keys and configuration; administrator remains stopped")
        for node in [source, destination, relay]:
            node.stop()
        public_trust = json.loads((STATE / "administrator-credentials.json").read_text())
        for entry in public_trust.values():
            entry.pop("secret", None)
            entry["expiry_unix"] = min(entry["expiry_unix"], int(time.time()) + 300)
        for node in [relay, source, destination]:
            public_trust[credentials[node.name]["credential_id"]]["overlay_addresses"] = [node.address]
        for node in [relay, source, destination]:
            trust_path = STATE / (node.name + "-credentials.json")
            trust_path.write_text(json.dumps({"format": "avian-verified-membership-v1", "credentials": public_trust}))
            trust_path.chmod(0o600)
            node.require_verified_membership = True
            node.start()
        restored_public_trust = wait_until(lambda: source.ping(destination), arguments.timeout)
        record("public_trust_file_cold_restart", restored_public_trust,
               "Public-only cache restored on each member with five-minute leases; no administrator online or private issuer/device keys copied; signed membership verification is tested separately")
        report["restored_routes"] = source.command("route")
        report["restored_peers"] = source.command("peer")
        report["restored_errors"] = {
            node.name: [re.sub(r"[A-Za-z0-9+/]{40,}={0,2}", "[redacted]", line) for line in (STATE / (node.name + ".log")).read_text().splitlines()
                        if "WARN" in line or "ERROR" in line][-5:]
            for node in [relay, source, destination]
        }
        if restored_public_trust:
            # Keep the known-good path as a control for both rejection scenarios.
            destination.stop()
            destination_configuration = STATE / "destination.toml"
            original_configuration = destination_configuration.read_text()
            destination_configuration.write_text(original_configuration.replace(
                f'ipv4 = "{destination.address}"', 'ipv4 = "10.201.0.99"'))
            destination.start()
            spoofed_destination = Node("unassigned", 99)
            spoofed_address_reachable = wait_until(lambda: source.ping(spoofed_destination), 5)
            direct_relay_healthy = source.ping(relay)
            record("unassigned_overlay_address_rejected", direct_relay_healthy and not spoofed_address_reachable,
                   "A member advertises an address absent from its verified key binding; source-to-relay control remains reachable")
            destination.stop()
            destination_configuration.write_text(original_configuration)
            destination.start()
            baseline_restored = wait_until(lambda: source.ping(destination), arguments.timeout)
            record("authorized_address_restored", baseline_restored,
                   "Restoring the assigned address restores endpoint traffic")
            for node in [source, destination, relay]:
                node.stop()
            public_trust[credentials[relay.name]["credential_id"]]["allow_relay"] = False
            for node in [relay, source, destination]:
                trust_path = STATE / (node.name + "-credentials.json")
                trust_path.write_text(json.dumps({"format": "avian-verified-membership-v1", "credentials": public_trust}))
                node.start()
            direct_links_ready = wait_until(lambda: source.ping(relay) and destination.ping(relay), arguments.timeout)
            forbidden_relay_reachable = wait_until(lambda: source.ping(destination), 5)
            record("relay_permission_enforced", baseline_restored and direct_links_ready and not forbidden_relay_reachable,
                   "After cold restart with relay permission removed, both direct links work but endpoint traffic cannot traverse the relay")
            source.stop()
            (STATE / "source-credentials.json").unlink()
            source.start()
            rejected_start = wait_until(lambda: source.process.poll() is not None, 3)
            required_cache_error = "verified membership cache is required" in (STATE / "source.log").read_text()
            record("missing_required_cache_stops_startup", rejected_start and required_cache_error,
                   "Explicit verified-membership mode refuses startup when its authoritative public cache is missing")
        for name in [
            "revocation_propagation", "traffic_class_path_selection", "cellular_exclusion_each_hop",
            "two_second_tcp_udp_recovery", "congestion_control_priority", "progressive_scale",
            "relay_payload_confidentiality", "android_physical_network_binding",
        ]:
            report["gates"].append({"name": name, "status": "not_run", "evidence": "Requires AVIAN engine extensions or a separate platform/load test; upstream baseline does not establish this requirement"})
    except Exception as error:
        # Configuration and CLI responses contain temporary private keys; do not copy them into reports.
        report["runner_error"] = {"type": type(error).__name__, "description": str(error) if isinstance(error, RuntimeError) else "Runtime check could not complete; inspect the disposable environment"}
    finally:
        for node in nodes:
            node.stop()
        output = Path("/results/runtime.json")
        output.write_text(json.dumps(report, indent=2) + "\n")
        print(f"Wrote {output}", flush=True)
    return 0 if report["qualified_for_cutover"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
