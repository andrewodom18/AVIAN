"""Exercise authenticated lifecycle control inside disposable network namespaces."""

import argparse
import base64
import os
import socket
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
PORT_NAMESPACES = {}
HELPERS = []


def unprivileged(namespace, arguments):
    return [
        "ip",
        "netns",
        "exec",
        namespace,
        "setpriv",
        "--reuid=1000",
        "--regid=1000",
        "--clear-groups",
        "--bounding-set=-all",
        *map(str, arguments),
    ]


def run_cli(*arguments):
    command = [str(SERVICES / "avianctl"), *map(str, arguments)]
    if "--config" in arguments:
        config = Path(arguments[arguments.index("--config") + 1])
        if config.parent.name.startswith("runtime-node-"):
            command = unprivileged(config.parent.name, command)
    result = subprocess.run(
        command, check=True, capture_output=True, text=True, timeout=20
    )
    return json.loads(result.stdout)


def request_api(port, token, route_path, action=None, json_body=None):
    if json_body is None and action is not None:
        json_body = {"action": action}
    # The token travels through stdin, never process arguments or environment.
    script = """import json,sys,urllib.request,urllib.error
values=json.load(sys.stdin)
request=urllib.request.Request('http://127.0.0.1:'+str(values['port'])+values['path'],data=None if values['body'] is None else json.dumps(values['body']).encode(),headers={'Authorization':'Bearer '+values['token'],'Content-Type':'application/json'})
try:
 with urllib.request.urlopen(request,timeout=20) as response: print(json.dumps([response.status,json.load(response)]))
except urllib.error.HTTPError as error: print(json.dumps([error.code,json.load(error)]))
"""
    completed = subprocess.run(
        unprivileged(PORT_NAMESPACES[port], ["python3", "-c", script]),
        input=json.dumps(
            {"port": port, "token": token, "path": route_path, "body": json_body}
        ),
        text=True,
        capture_output=True,
        timeout=25,
    )
    if completed.returncode:
        raise OSError("local manager API unavailable")
    return json.loads(completed.stdout)


def start_helper(node, node_id):
    directory = Path("/run/avian") / node.name
    directory.mkdir(parents=True, mode=0o711, exist_ok=True)
    socket_path = directory / "network.sock"
    configuration = STATE / (node.name + "-helper.json")
    configuration.write_text(
        json.dumps(
            {
                "allowed_uid": 1000,
                "runtime_directory": str(directory),
                "node_id": node_id,
                "tun_name": "avian0",
                "overlay_address": node.address,
                "overlay_network": "10.201.0.0/24",
                "mtu": 1320,
                "listen_port": 11010,
                "allowed_underlay_interfaces": ["peer" + str(node.number)],
            }
        )
    )
    configuration.chmod(0o600)
    socket_path.unlink(missing_ok=True)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    listener.bind(str(socket_path))
    os.chown(socket_path, 1000, 1000)
    socket_path.chmod(0o600)
    listener.listen(16)
    log = (STATE / (node.name + "-helper.log")).open("w")
    process = subprocess.Popen(
        [
            "ip",
            "netns",
            "exec",
            node.name,
            "setpriv",
            "--bounding-set=-all,+net_admin,+net_raw",
            "/engine/avian-network-helper",
            "--config",
            str(configuration),
        ],
        stdin=listener,
        stdout=log,
        stderr=subprocess.STDOUT,
    )
    listener.close()
    HELPERS.append((process, log))
    time.sleep(0.15)
    if process.poll() is not None:
        raise RuntimeError("scoped helper did not start")
    return socket_path


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
            os.chown(temporary_path, 1000, 1000)
            temporary_path.replace(destination)
        finally:
            temporary_path.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--priority-forwarding", action="store_true")
    parser.add_argument("--services", action="store_true")
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
        "live_routes_observe_per_class",
        "automatic_capacity_has_confirmed_delivery",
        "explicit_stop_reaped",
        "restart_after_stop",
        "runtime_cli_status",
        "delivery_after_restart",
        "runtime_revocation_cache_publication",
        "runtime_revocation_blocks_overlay",
        "manager_shutdown_reaps_engine",
    }
    expected_gates.update(
        {
            "capability_free_manager_and_core",
            "helper_has_only_network_capabilities",
            "stop_removes_owned_interface",
            "manager_crash_removes_owned_interface",
            "restart_after_manager_crash",
            "helper_outage_preserves_overlay_packets",
            "helper_restart_retains_exact_tun_identity",
            "helper_restart_keeps_manager_and_core_running",
            "failed_release_retains_owned_scope",
            "failed_release_blocks_replacement_start",
            "release_retry_after_foreign_route_removed",
        }
    )
    if arguments.services:
        expected_gates.update(
            {
                "unauthorized_service_publication_rejected",
                "signed_services_cross_overlay",
                "services_remain_unobserved",
                "service_owner_revocation_removes_advertisement",
            }
        )
    report = {
        "scope": "native unprivileged Linux runtime inside disposable container namespaces",
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
        STATE.chmod(0o711)
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
            helper_socket = start_helper(node, node_id)
            PORT_NAMESPACES[port] = node.name
            configuration = f"""listen = "127.0.0.1:{port}"
token_file = "{node_directory}/api-token"
membership_file = "{node_directory}/membership.json"
trust_root_file = "{node_directory}/trust-root.json"
[runtime]
execution = "native"
isolation_required = false
network_helper_socket = "{helper_socket}"
interface_name = "avian0"
overlay_prefix_length = 24
measured_routing = true
private_rpc = true
underlay_binding = true
priority_forwarding = true
control_tcp_ports = [9183]
engine_directory = "/engine"
runtime_directory = "{node_directory}/runtime"
membership_file = "{node_directory}/membership.json"
trust_root_file = "{node_directory}/trust-root.json"
credential_file = "{node_directory}/transport.key"
node_id = "{node_id}"
overlay_address = "{node.address}"
listen = "{node.physical_address}:11010"
rpc_listen = "127.0.0.1:15888"
peers = []
[runtime.bootstrap]
local_discovery = false
owned_endpoints = ["tcp://{peer.physical_address}:11010", "udp://{peer.physical_address}:11010"]
[runtime.bootstrap.metering]
"peer{node.number}" = "unmetered"
"""
            if arguments.services:
                peer_identity = next(
                    identity
                    for candidate, _, identity in node_records
                    if candidate.name == peer.name
                )
                configuration += f"""
[services]
state_file = "{node_directory}/services.json"
identity_key_file = "{node_directory}/identity.key"
[service_exchange]
listen = "{node.address}:9183"
[[service_exchange.peers]]
node_id = "{peer_identity}"
address = "{peer.address}:9183"
"""
            configuration_path = node_directory / "avian.toml"
            configuration_path.write_text(configuration)
            for owned_path in [node_directory, *node_directory.rglob("*")]:
                os.chown(owned_path, 1000, 1000)
            log_file = (node_directory / "manager.log").open("w")
            log_files.append(log_file)
            process = subprocess.Popen(
                unprivileged(
                    node.name,
                    [str(SERVICES / "aviand"), "--config", str(configuration_path)],
                ),
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
        cap_free = True
        for manager, port, token in managers:
            _, state = request_api(port, token, "/v1/runtime")
            for process_id in [manager.pid, state["runtime"]["process_id"]]:
                cap_free &= (
                    "CapEff:\t0000000000000000"
                    in Path(f"/proc/{process_id}/status").read_text()
                )
        record("capability_free_manager_and_core", cap_free)
        record(
            "helper_has_only_network_capabilities",
            all(
                int(
                    Path(f"/proc/{helper.pid}/status")
                    .read_text()
                    .split("CapEff:\t")[1]
                    .splitlines()[0],
                    16,
                )
                == (1 << 12) | (1 << 13)
                for helper, _ in HELPERS
            ),
        )
        connected = network_helpers.wait_until(lambda: nodes[0].ping(nodes[1]), 15)
        record("supervised_overlay_packet_delivery", connected)
        record(
            "supervised_overlay_tcp_delivery",
            connected and tcp_echo(nodes[0], nodes[1]),
        )
        process, port, token = managers[0]
        _, before_restart = request_api(port, token, "/v1/runtime")
        original_core = before_restart["runtime"]["process_id"]
        original_tun = network_helpers.namespace_command(
            nodes[0].name, ["ip", "-json", "link", "show", "avian0"]
        ).stdout
        helper, _ = HELPERS[0]
        helper.kill()
        helper.wait()
        record(
            "helper_outage_preserves_overlay_packets",
            nodes[0].ping(nodes[1]) and tcp_echo(nodes[0], nodes[1]),
        )
        start_helper(nodes[0], node_records[0][2])
        retained_tun = network_helpers.namespace_command(
            nodes[0].name, ["ip", "-json", "link", "show", "avian0"]
        ).stdout
        record(
            "helper_restart_retains_exact_tun_identity", retained_tun == original_tun
        )
        _, after_restart = request_api(port, token, "/v1/runtime")
        record(
            "helper_restart_keeps_manager_and_core_running",
            process.poll() is None
            and after_restart["runtime"]["process_id"] == original_core
            and after_restart["runtime"]["engine_ready"]
            and nodes[0].ping(nodes[1])
            and tcp_echo(nodes[0], nodes[1]),
        )
        route_observation = {}

        def measured_routes_available():
            nonlocal route_observation
            status_code, route_observation = request_api(port, token, "/v1/paths")
            decisions = route_observation.get("items", [])
            return (
                status_code == 200
                and route_observation.get("availability") == "observed"
                and route_observation.get("kind") == "per_class_routes"
                and bool(decisions)
                and all(
                    item.get("traffic_class")
                    in {"control", "telemetry", "video", "bulk"}
                    and isinstance(item.get("connection_id"), str)
                    and item["connection_id"]
                    and isinstance(item.get("next_hop_peer_id"), int)
                    and item["next_hop_peer_id"] > 0
                    and item.get("route", {}).get("hop_count", 0) > 0
                    for item in decisions
                )
            )

        record(
            "live_routes_observe_per_class",
            network_helpers.wait_until(measured_routes_available, 10),
        )
        report["observed_class_routes"] = route_observation
        capacity_observation = {}

        def confirmed_automatic_capacity():
            nonlocal capacity_observation
            status_code, capacity_observation = request_api(port, token, "/v1/links")
            return (
                status_code == 200
                and capacity_observation.get("availability") == "observed"
                and any(
                    link.get("metered") is False
                    and link.get("policy_valid") is True
                    and link.get("quality_fresh") is True
                    and isinstance(
                        link.get("capacity_delivered_rate_lower_bound_bps"), int
                    )
                    and link["capacity_delivered_rate_lower_bound_bps"] > 0
                    and isinstance(link.get("capacity_age_ms"), int)
                    and 0 <= link["capacity_age_ms"] <= 10_000
                    for link in capacity_observation.get("items", [])
                )
            )

        # No explicit probe API is called: delivery evidence must come from the
        # manager's bounded automatic measurement loop on this unmetered link.
        record(
            "automatic_capacity_has_confirmed_delivery",
            network_helpers.wait_until(confirmed_automatic_capacity, 10),
        )
        report["observed_automatic_capacity"] = capacity_observation
        network_helpers.namespace_command(
            nodes[0].name, ["ip", "route", "add", "10.202.0.0/24", "dev", "avian0"]
        )
        code, _ = request_api(port, token, "/v1/network-control", "stop")
        _, state = request_api(port, token, "/v1/runtime")
        record(
            "failed_release_retains_owned_scope",
            code != 200
            and state["runtime"]["state"] == "failed"
            and network_helpers.namespace_command(
                nodes[0].name, ["ip", "link", "show", "avian0"], check=False
            ).returncode
            == 0,
        )
        code, _ = request_api(port, token, "/v1/network-control", "start")
        _, state = request_api(port, token, "/v1/runtime")
        record(
            "failed_release_blocks_replacement_start",
            code != 200 and state["runtime"]["process_id"] is None,
        )
        network_helpers.namespace_command(
            nodes[0].name, ["ip", "route", "delete", "10.202.0.0/24", "dev", "avian0"]
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
        record(
            "stop_removes_owned_interface",
            network_helpers.namespace_command(
                nodes[0].name, ["ip", "link", "show", "avian0"], check=False
            ).returncode
            != 0,
        )
        record("release_retry_after_foreign_route_removed", status_code == 200)
        configuration_path = node_records[0][1] / "avian.toml"
        response_body = run_cli("--config", configuration_path, "runtime", "start")
        record("restart_after_stop", response_body["runtime"]["engine_ready"])
        response_body = run_cli("--config", configuration_path, "runtime", "status")
        record("runtime_cli_status", response_body["runtime"]["engine_ready"])
        record(
            "delivery_after_restart",
            network_helpers.wait_until(lambda: nodes[0].ping(nodes[1]), 15),
        )
        # A killed standalone manager must release its leased network. The test
        # explicitly reaps the orphan engine; installed service cgroup behavior is
        # a separate qualification gate and is not claimed by this fixture.
        _, state = request_api(managers[0][1], managers[0][2], "/v1/runtime")
        orphan_engine = state["runtime"]["process_id"]
        managers[0][0].kill()
        managers[0][0].wait()
        removed = network_helpers.wait_until(
            lambda: network_helpers.namespace_command(
                nodes[0].name, ["ip", "link", "show", "avian0"], check=False
            ).returncode
            != 0,
            5,
        )
        record("manager_crash_removes_owned_interface", removed)
        try:
            os.kill(orphan_engine, 9)
        except ProcessLookupError:
            pass
        try:
            os.waitpid(orphan_engine, 0)
        except ChildProcessError:
            pass
        log_file = (node_records[0][1] / "manager-restarted.log").open("w")
        log_files.append(log_file)
        restarted = subprocess.Popen(
            unprivileged(
                nodes[0].name,
                [str(SERVICES / "aviand"), "--config", str(configuration_path)],
            ),
            stdout=log_file,
            stderr=subprocess.STDOUT,
        )
        managers[0] = (restarted, managers[0][1], managers[0][2])

        def restart_ready():
            try:
                return (
                    request_api(managers[0][1], managers[0][2], "/v1/status")[0] == 200
                )
            except OSError:
                return False

        if not network_helpers.wait_until(restart_ready, 10):
            raise RuntimeError("manager restart API unavailable")
        code, started = request_api(
            managers[0][1], managers[0][2], "/v1/network-control", "start"
        )
        record(
            "restart_after_manager_crash",
            code == 200
            and started.get("runtime", {}).get("engine_ready") is True
            and network_helpers.wait_until(lambda: nodes[0].ping(nodes[1]), 15),
        )
        if arguments.services:
            publication = {
                "ttl_seconds": 120,
                "endpoints": [
                    {
                        "kind": "arc_telemetry",
                        "address": nodes[1].address,
                        "protocol": "udp",
                        "port": 18001,
                    }
                ],
            }
            record(
                "unauthorized_service_publication_rejected",
                request_api(
                    managers[1][1], "wrong-token", "/v1/services", json_body=publication
                )[0]
                == 401,
            )
            status, _ = request_api(
                managers[1][1], managers[1][2], "/v1/services", json_body=publication
            )
            if status != 200:
                raise RuntimeError("service publication failed")

            def learned_service():
                code, page = request_api(managers[0][1], managers[0][2], "/v1/services")
                return code == 200 and any(
                    item["announcement"]["claims"]["owner_node_id"]
                    == node_records[1][2]
                    for item in page["items"]
                )

            record(
                "signed_services_cross_overlay",
                network_helpers.wait_until(learned_service, 12),
            )
            if not learned_service():
                raise RuntimeError("service announcement did not cross overlay")
            _, page = request_api(managers[0][1], managers[0][2], "/v1/services")
            record(
                "services_remain_unobserved",
                all(item["reachability"] == "unobserved" for item in page["items"]),
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
        if arguments.services:

            def removed_service():
                code, page = request_api(managers[0][1], managers[0][2], "/v1/services")
                return code == 200 and not page["items"]

            record(
                "service_owner_revocation_removes_advertisement",
                network_helpers.wait_until(removed_service, 5),
            )
        process, port, token = managers[0]
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
        for helper, log in HELPERS:
            helper.terminate()
            try:
                helper.wait(timeout=8)
            except subprocess.TimeoutExpired:
                helper.kill()
                helper.wait()
            log.close()
        Path("/results/native-runtime.json").write_text(
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
