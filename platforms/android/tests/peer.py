"""Run an isolated relay and target; issuer private keys are never mounted here."""

import argparse
import json
import os
from pathlib import Path
import secrets
import shutil
import signal
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE = Path("/state")
RESULTS = Path("/results")
FIXTURE = Path("/fixture")
STOPPED = threading.Event()


def publish(name, report):
    temporary = RESULTS / (name + ".tmp")
    temporary.write_text(json.dumps(report))
    temporary.replace(RESULTS / (name + ".json"))


def control():
    try:
        return json.loads(Path("/control/control.json").read_text())
    except (OSError, ValueError):
        return {}


def spawn(arguments, name, environment=None):
    log = (STATE / (name + ".log")).open("w")
    try:
        return subprocess.Popen(
            arguments, env=environment, stdout=log, stderr=subprocess.STDOUT
        )
    finally:
        log.close()


def stop(children):
    for child in reversed(children):
        child.terminate()
    for child in reversed(children):
        try:
            child.wait(timeout=5)
        except subprocess.TimeoutExpired:
            child.kill()
            child.wait()


def request_local(name, method, path, body=None):
    token = (STATE / name / "api-token").read_text()
    request = urllib.request.Request(
        "http://127.0.0.1:9181" + path,
        method=method,
        headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
        },
        data=None if body is None else json.dumps(body).encode(),
    )
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=2) as response:
        return json.load(response)


def echo(name, address):
    report = {"received_echo_packets": 0, "ready": False}
    next_publication = 0
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as endpoint:
        endpoint.setsockopt(socket.IPPROTO_IP, socket.IP_TOS, 0xA0)
        endpoint.bind((address, 19000))
        endpoint.settimeout(0.2)
        report["ready"] = True
        publish(name, report)
        while not STOPPED.is_set():
            if name == "target" and time.monotonic() >= next_publication:
                next_publication = time.monotonic() + 2
                if not control().get("pause_target_publication", False):
                    try:
                        publication = request_local(
                            name,
                            "POST",
                            "/v1/services",
                            {
                                "ttl_seconds": 8,
                                "endpoints": [
                                    {
                                        "kind": "arc_telemetry",
                                        "address": address,
                                        "protocol": "udp",
                                        "port": 19000,
                                    }
                                ],
                            },
                        )
                        report["publication"] = publication
                    except (OSError, ValueError, urllib.error.URLError):
                        pass
            try:
                payload, source = endpoint.recvfrom(2048)
                endpoint.sendto(payload, source)
                report["received_echo_packets"] += 1
                publish(name, report)
            except socket.timeout:
                pass


def target_agent():
    manager = spawn(
        ["/services/aviand", "--config", str(STATE / "target/avian.toml")],
        "target-manager",
    )
    try:
        echo("target", "10.201.0.3")
    finally:
        stop([manager])


class ExchangeProxy(BaseHTTPRequestHandler):
    def do_GET(self):
        if not self.path.startswith("/v1/service-announcements"):
            self.send_error(404)
            return
        try:
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
            with opener.open(
                "http://10.201.0.2:4193" + self.path, timeout=2
            ) as response:
                payload = json.load(response)
            fault = control().get("service_fault")
            if fault in {"tamper", "wrong_network"}:
                # Preserve the original signature while modifying its authenticated claims.
                for announcement in payload.get("records", []):
                    record = announcement.get("announcement", announcement)
                    if "claims" in record:
                        if fault == "tamper":
                            record["claims"]["revision"] += 100000
                        else:
                            record["claims"]["network_id"] = "another-network"
            body = json.dumps(payload).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except (OSError, ValueError, urllib.error.URLError):
            self.send_error(502)

    def log_message(self, *arguments):
        pass


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target-agent", action="store_true")
    arguments = parser.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: STOPPED.set())
    if arguments.target_agent:
        target_agent()
        return 0
    children = []
    proxy = None
    try:
        for name in ("relay", "target", "revocations"):
            shutil.copytree(FIXTURE / name, STATE / name)
            (STATE / name).chmod(0o700)
            for path in (STATE / name).iterdir():
                if path.is_file():
                    path.chmod(0o600)
        subprocess.run(["ip", "netns", "add", "target"], check=True)
        subprocess.run(
            [
                "ip",
                "link",
                "add",
                "relay-link",
                "type",
                "veth",
                "peer",
                "name",
                "target-link",
            ],
            check=True,
        )
        subprocess.run(
            ["ip", "link", "set", "target-link", "netns", "target"], check=True
        )
        for prefix, interface, address in [
            ([], "relay-link", "172.28.0.1/24"),
            (["ip", "netns", "exec", "target"], "target-link", "172.28.0.2/24"),
        ]:
            subprocess.run(
                prefix + ["ip", "address", "add", address, "dev", interface], check=True
            )
            subprocess.run(prefix + ["ip", "link", "set", interface, "up"], check=True)
        subprocess.run(
            ["ip", "netns", "exec", "target", "ip", "link", "set", "lo", "up"],
            check=True,
        )
        for name in ("relay", "target"):
            token = STATE / name / "rpc-token"
            token.write_text(secrets.token_hex(32))
            token.chmod(0o600)
            policy = STATE / name / "traffic-policy.json"
            policy.write_text(
                json.dumps(
                    {
                        "metered_addresses": [],
                        "control_tcp_ports": [4191, 4192, 4193, 9180],
                        "allow_metered_video": False,
                        "allow_metered_bulk": False,
                    }
                )
            )
            policy.chmod(0o600)
            environment = os.environ.copy()
            environment.update(
                {
                    "ET_CREDENTIAL": (STATE / name / "transport.key")
                    .read_text()
                    .strip(),
                    "ET_REQUIRE_VERIFIED_MEMBERSHIP": "1",
                    "AVIAN_OWNED_CONNECTIVITY_ONLY": "1",
                    "AVIAN_TRAFFIC_POLICY_FILE": str(policy),
                    "ET_PRIORITY_FORWARDING": "1",
                    "ET_MEASURED_ROUTING": "1",
                    "ET_FAST_FAILOVER": "1",
                    "AVIAN_RPC_TOKEN_FILE": str(token),
                }
            )
            prefix = [] if name == "relay" else ["ip", "netns", "exec", "target"]
            children.append(
                spawn(
                    prefix
                    + [
                        "/engine/easytier-core",
                        "--config-file",
                        str(STATE / name / "engine.toml"),
                        "--rpc-portal",
                        "127.0.0.1:15888",
                        "--console-log-level",
                        "warn",
                    ],
                    name + "-engine",
                    environment,
                )
            )
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if any(child.poll() is not None for child in children):
                raise RuntimeError("Isolated engine exited before readiness")
            ready = (
                subprocess.run(
                    ["ip", "address", "show", "avian-relay"], capture_output=True
                ).returncode
                == 0
            )
            ready &= (
                subprocess.run(
                    [
                        "ip",
                        "netns",
                        "exec",
                        "target",
                        "ip",
                        "address",
                        "show",
                        "avian-target",
                    ],
                    capture_output=True,
                ).returncode
                == 0
            )
            if ready:
                break
            time.sleep(0.1)
        else:
            raise RuntimeError("Isolated relay and target tunnels did not become ready")
        children.append(
            spawn(
                ["/services/aviand", "--config", str(STATE / "relay/avian.toml")],
                "relay-manager",
            )
        )
        children.append(
            spawn(
                [
                    "ip",
                    "netns",
                    "exec",
                    "target",
                    "python3",
                    "/android/tests/peer.py",
                    "--target-agent",
                ],
                "target-agent",
            )
        )
        children.append(
            spawn(
                [
                    "/services/avian-revocationd",
                    "--trust-root",
                    str(STATE / "revocations/trust-root.json"),
                    "--membership",
                    str(STATE / "revocations/membership.json"),
                    "--engine-trust",
                    str(STATE / "revocations/unused-cache.json"),
                    "--listen",
                    "10.201.0.2:9180",
                ],
                "revocation-service",
            )
        )
        proxy = ThreadingHTTPServer(("10.201.0.2", 4192), ExchangeProxy)
        threading.Thread(target=proxy.serve_forever, daemon=True).start()

        def updates():
            previous = None
            while not STOPPED.wait(0.2):
                source = Path("/control/membership.json")
                if not source.exists():
                    continue
                payload = source.read_bytes()
                if payload != previous:
                    target = STATE / "revocations/membership.next"
                    target.write_bytes(payload)
                    target.chmod(0o600)
                    target.replace(STATE / "revocations/membership.json")
                    previous = payload

        threading.Thread(target=updates, daemon=True).start()

        def renew_baseline_leases():
            while not STOPPED.wait(30):
                for name in ("relay", "target"):
                    temporary = STATE / name / "engine-trust.next"
                    temporary.unlink(missing_ok=True)
                    result = subprocess.run(
                        [
                            "/services/avianctl",
                            "--config",
                            str(STATE / name / "avian.toml"),
                            "export-engine-trust",
                            "--output",
                            str(temporary),
                        ],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                    )
                    if result.returncode == 0:
                        temporary.replace(STATE / name / "engine-trust.json")

        threading.Thread(target=renew_baseline_leases, daemon=True).start()
        echo("peer", "10.201.0.2")
    except Exception as error:
        publish(
            "peer",
            {
                "ready": False,
                "error_type": type(error).__name__,
                "description": str(error),
            },
        )
        return 1
    finally:
        STOPPED.set()
        if proxy is not None:
            proxy.shutdown()
            proxy.server_close()
        stop(children)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
