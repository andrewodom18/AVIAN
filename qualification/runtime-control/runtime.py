"""Exercise authenticated lifecycle control inside disposable network namespaces."""

import argparse
import base64
import importlib.util
import json
from pathlib import Path
import secrets
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

module_specification = importlib.util.spec_from_file_location(
    "network_runtime", "/qualification/runtime.py"
)
network_helpers = importlib.util.module_from_spec(module_specification)
sys.modules[module_specification.name] = network_helpers
module_specification.loader.exec_module(network_helpers)
SERVICES = Path("/services")
STATE = Path("/state")


def run_cli(*arguments):
    result = subprocess.run(
        [str(SERVICES / "avianctl"), *map(str, arguments)],
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def request_api(port, token, route_path, action=None):
    response_body = None if action is None else json.dumps({"action": action}).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{route_path}",
        data=response_body,
        headers={
            "Authorization": "Bearer " + token,
            "Content-Type": "application/json",
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=20) as response:
            return response.status, json.load(response)
    except urllib.error.HTTPError as problem:
        return problem.code, json.load(problem)


def tcp_echo(source, destination):
    server_code = """import socket
server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(('0.0.0.0', 19001))
server.listen(1)
server.settimeout(5)
connection, client_address = server.accept()
connection.sendall(connection.recv(128))
connection.close()
server.close()
"""
    process = subprocess.Popen(
        ["ip", "netns", "exec", destination.name, "python3", "-c", server_code],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        time.sleep(0.2)
        client_code = f"""import socket
with socket.create_connection(({destination.address!r}, 19001), timeout=3) as connection:
    connection.sendall(b'runtime-control')
    assert connection.recv(128) == b'runtime-control'
"""
        return (
            network_helpers.namespace_command(
                source.name, ["python3", "-c", client_code], check=False
            ).returncode
            == 0
        )
    finally:
        process.terminate()
        process.wait(timeout=3)


def publish_membership(source, destination):
    """Replace a peer's public membership file without exposing a partial document."""
    with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as temporary:
        temporary_path = Path(temporary.name)
        try:
            temporary.write(source.read_bytes())
            temporary.flush()
            temporary_path.chmod(0o600)
            temporary_path.replace(destination)
        finally:
            temporary_path.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--priority-forwarding", action="store_true")
    arguments = parser.parse_args()
    expected_gates = {
        f"{name}_{port}"
        for port in (4191, 4192)
        for name in (
            "unauthorized_start_rejected",
            "engine_start_ready",
            "repeated_start_safe",
        )
    } | {
        "supervised_overlay_packet_delivery",
        "supervised_overlay_tcp_delivery",
        "live_routes_explicitly_generic",
        "explicit_stop_reaped",
        "restart_after_stop",
        "runtime_cli_status",
        "delivery_after_restart",
        "runtime_revocation_cache_publication",
        "runtime_revocation_blocks_overlay",
        "manager_shutdown_reaps_engine",
    }
    report = {
        "scope": "isolated namespace runtime control; no host connection ownership",
        "gates": [],
        "qualified_for_cutover": False,
        "priority_forwarding": arguments.priority_forwarding,
        "expected_gates": sorted(expected_gates),
    }
    managers = []
    nodes = []
    log_files = []

    def record(name, passed):
        report["gates"].append({"name": name, "status": "pass" if passed else "fail"})
        print(name, passed, flush=True)

    try:
        STATE.mkdir(exist_ok=True)
        administrator_directory = STATE / "administrator"
        run_cli(
            "init", "--state-dir", administrator_directory, "--network", "runtime-test"
        )
        network_helpers.run(["ip", "link", "add", "underlay", "type", "bridge"])
        network_helpers.run(["ip", "link", "set", "underlay", "up"])
        node_records = []
        for number in [1, 2]:
            node = network_helpers.Node("runtime-node-" + str(number), number)
            node.create()
            nodes.append(node)
            node_directory = STATE / node.name
            node_directory.mkdir(mode=0o700)
            identity = run_cli("keygen", "--output", node_directory / "identity.key")
            transport_private_key = secrets.token_bytes(32)
            private_key_der = (
                bytes.fromhex("302e020100300506032b656e04220420")
                + transport_private_key
            )
            public_key_der = subprocess.run(
                ["openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
                input=private_key_der,
                check=True,
                capture_output=True,
            ).stdout
            membership_export = node_directory / "member.json"
            member = run_cli(
                "--config",
                administrator_directory / "avian.toml",
                "enroll",
                "--identity-public-key",
                identity["identity_public_key"],
                "--transport-public-key",
                public_key_der[-32:].hex(),
                "--address",
                node.address,
                "--relay",
                "--output",
                membership_export,
            )
            (node_directory / "transport.key").write_text(
                base64.b64encode(transport_private_key).decode()
            )
            (node_directory / "transport.key").chmod(0o600)
            node_records.append((node, node_directory, member["node_id"]))
        for node, node_directory, node_id in node_records:
            shutil.copyfile(
                administrator_directory / "trust-root.json",
                node_directory / "trust-root.json",
            )
            shutil.copyfile(
                administrator_directory / "membership.json",
                node_directory / "membership.json",
            )
            (node_directory / "membership.json").chmod(0o600)
            (node_directory / "trust-root.json").chmod(0o600)
            token = secrets.token_hex(32)
            (node_directory / "api-token").write_text(token)
            (node_directory / "api-token").chmod(0o600)
            port = 4190 + node.number
            peer = nodes[1 if node.number == 1 else 0]
            configuration = f"""listen = "127.0.0.1:{port}"
token_file = "{node_directory}/api-token"
membership_file = "{node_directory}/membership.json"
trust_root_file = "{node_directory}/trust-root.json"
[runtime]
isolation_required = true
priority_forwarding = {str(arguments.priority_forwarding).lower()}
namespace_name = "{node.name}"
engine_directory = "/engine"
runtime_directory = "{node_directory}/runtime"
membership_file = "{node_directory}/membership.json"
trust_root_file = "{node_directory}/trust-root.json"
credential_file = "{node_directory}/transport.key"
node_id = "{node_id}"
overlay_address = "{node.address}"
listen = "{node.physical_address}:11010"
rpc_listen = "127.0.0.1:15888"
peers = ["{peer.physical_address}:11010"]
"""
            configuration_path = node_directory / "avian.toml"
            configuration_path.write_text(configuration)
            log_file = (node_directory / "manager.log").open("w")
            log_files.append(log_file)
            process = subprocess.Popen(
                [str(SERVICES / "aviand"), "--config", str(configuration_path)],
                stdout=log_file,
                stderr=subprocess.STDOUT,
            )
            managers.append((process, port, token))
        for process, port, token in managers:

            def ready():
                try:
                    return request_api(port, token, "/v1/status")[0] == 200
                except OSError:
                    return False

            if not network_helpers.wait_until(ready, 10):
                raise RuntimeError("manager API did not become ready")
            record(
                "unauthorized_start_rejected_" + str(port),
                request_api(port, "invalid", "/v1/network-control", "start")[0] == 401,
            )
            status_code, response_body = request_api(
                port, token, "/v1/network-control", "start"
            )
            record(
                "engine_start_ready_" + str(port),
                status_code == 200
                and response_body.get("runtime", {}).get("engine_ready") is True,
            )
            if status_code != 200:
                report["start_error"] = response_body
                report["engine_diagnostics"] = {}
                for (
                    diagnostic_node,
                    diagnostic_directory,
                    expected_node_id,
                ) in node_records:
                    details = {}
                    try:
                        info = diagnostic_node.command("node")
                        details["node_type"] = type(info).__name__
                        details["node_keys"] = (
                            list(info) if isinstance(info, dict) else []
                        )
                        if isinstance(info, dict):
                            details["hostname_matches"] = (
                                info.get("hostname") == expected_node_id
                            )
                            details["ipv4"] = info.get("ipv4_addr")
                    except Exception as diagnostic_error:
                        details["cli_error_type"] = type(diagnostic_error).__name__
                    log_path = diagnostic_directory / "runtime" / "engine.log"
                    if log_path.exists():
                        text = log_path.read_text(errors="replace")[-5000:]
                        text = re.sub(r"[A-Za-z0-9+/]{42,}={0,2}", "<redacted>", text)
                        details["engine_log_tail"] = text
                    report["engine_diagnostics"][diagnostic_node.name] = details
                raise RuntimeError("isolated engine start rejected")
            status_code, response_body = request_api(
                port, token, "/v1/network-control", "start"
            )
            record(
                "repeated_start_safe_" + str(port),
                status_code == 200 and response_body["runtime"]["engine_ready"],
            )
        connected = network_helpers.wait_until(lambda: nodes[0].ping(nodes[1]), 15)
        record("supervised_overlay_packet_delivery", connected)
        record(
            "supervised_overlay_tcp_delivery",
            connected and tcp_echo(nodes[0], nodes[1]),
        )
        process, port, token = managers[0]
        status_code, response_body = request_api(port, token, "/v1/paths")
        record(
            "live_routes_explicitly_generic",
            status_code == 200 and response_body.get("kind") == "engine_generic_routes",
        )
        status_code, response_body = request_api(
            port, token, "/v1/network-control", "stop"
        )
        record(
            "explicit_stop_reaped",
            status_code == 200
            and response_body["runtime"]["process_id"] is None
            and not response_body["runtime"]["engine_ready"],
        )
        configuration_path = node_records[0][1] / "avian.toml"
        response_body = run_cli("--config", configuration_path, "runtime", "start")
        record("restart_after_stop", response_body["runtime"]["engine_ready"])
        response_body = run_cli("--config", configuration_path, "runtime", "status")
        record("runtime_cli_status", response_body["runtime"]["engine_ready"])
        record(
            "delivery_after_restart",
            network_helpers.wait_until(lambda: nodes[0].ping(nodes[1]), 15),
        )
        revoked_node_id = node_records[1][2]
        for _, node_directory, _ in node_records:
            trust = json.loads(
                (node_directory / "runtime" / "engine-trust.json").read_text()
            )
            if revoked_node_id not in trust["credentials"]:
                raise RuntimeError("Revocation baseline cache lacks the active member")
        revocation = run_cli(
            "--config",
            administrator_directory / "avian.toml",
            "revoke",
            "--node",
            revoked_node_id,
            "--output",
            STATE / "revocation.json",
        )
        report["revocation_revision"] = revocation["revocation_revision"]
        for _, node_directory, _ in node_records:
            publish_membership(
                administrator_directory / "membership.json",
                node_directory / "membership.json",
            )

        def revocation_published():
            for _, node_directory, _ in node_records:
                trust_path = node_directory / "runtime" / "engine-trust.json"
                trust = json.loads(trust_path.read_text())
                if set(trust["credentials"]) != {node_records[0][2]}:
                    return False
                membership_path = node_directory / "membership.json"
                membership = json.loads(membership_path.read_text())
                if (
                    membership["revocations"][-1]["claims"]["revision"]
                    != revocation["revocation_revision"]
                ):
                    return False
                if membership_path.stat().st_mode & 0o777 != 0o600:
                    return False
                if trust_path.stat().st_mode & 0o777 != 0o600:
                    return False
            return True

        publication_ready = network_helpers.wait_until(revocation_published, 10)
        record("runtime_revocation_cache_publication", publication_ready)
        blocked = publication_ready and network_helpers.wait_until(
            lambda: not nodes[0].ping(nodes[1]) and not nodes[1].ping(nodes[0]), 10
        )
        if blocked:
            blocked = all(
                not nodes[0].ping(nodes[1]) and not nodes[1].ping(nodes[0])
                for _ in range(3)
            )
        record("runtime_revocation_blocks_overlay", blocked)
        _, runtime = request_api(port, token, "/v1/runtime")
        engine_process_id = runtime["runtime"]["process_id"]
        engine_was_running = (
            isinstance(engine_process_id, int)
            and Path("/proc", str(engine_process_id)).exists()
        )
        process.terminate()
        process.wait(timeout=10)
        record(
            "manager_shutdown_reaps_engine",
            engine_was_running and not Path("/proc", str(engine_process_id)).exists(),
        )
    except Exception as problem:
        report["error"] = str(problem)
    finally:
        for process, _, _ in managers:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
        for log_file in log_files:
            log_file.close()
        Path("/results/runtime-control.json").write_text(
            json.dumps(report, indent=2) + "\n"
        )
    observed_names = [gate["name"] for gate in report["gates"]]
    complete = set(observed_names) == expected_gates and len(observed_names) == len(
        expected_gates
    )
    return (
        0
        if complete
        and "error" not in report
        and all(gate["status"] == "pass" for gate in report["gates"])
        else 1
    )


if __name__ == "__main__":
    raise SystemExit(main())
