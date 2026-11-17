#!/usr/bin/env python3
"""Container-only qualification of the scoped network helper's privilege boundary."""

import array
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import socket
import struct
import subprocess
import sys
import time
import threading

IP = "/usr/sbin/ip"
STATE = Path("/state")
SOCKET = "/state/helper/network.sock"
CONFIG = STATE / "helper.json"
BINARY = "/build/debug/avian-network-helper"
GATES = {}


def command(*arguments, **options):
    return subprocess.run(arguments, check=True, capture_output=True, **options).stdout


def wait_for(predicate, timeout=5):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError("bounded wait expired")


def request(operation, descriptor=False):
    with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as client:
        client.settimeout(4)
        client.connect(SOCKET)
        assert (
            struct.unpack(
                "3i", client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)
            )[1]
            == 0
        )
        client.send(json.dumps({"version": 1, "request": operation}).encode())
        payload, ancillary, flags, _ = client.recvmsg(4096, socket.CMSG_SPACE(8))
        assert not flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC)
        files = array.array("i")
        for level, kind, data in ancillary:
            assert (level, kind) == (socket.SOL_SOCKET, socket.SCM_RIGHTS)
            files.frombytes(data)
        response = json.loads(payload)
        if not response["allowed"]:
            for file in files:
                os.close(file)
            raise PermissionError(response.get("error"))
        assert len(files) == int(descriptor)
        return response, socket.socket(fileno=files[0]) if descriptor else None


def denied(operation):
    try:
        request(operation)
    except (PermissionError, ConnectionResetError):
        return
    raise AssertionError("invalid request was accepted")


def checksum(packet):
    if len(packet) % 2:
        packet += b"\0"
    total = sum(struct.unpack("!%dH" % (len(packet) // 2), packet))
    while total >> 16:
        total = (total & 65535) + (total >> 16)
    return (~total) & 65535


def manager():
    # This long-lived peer owns the helper lease. Its engine children can borrow
    # sockets without acquiring the privilege needed to create or reconfigure them.
    assert os.getuid() == 1000
    assert "CapEff:\t0000000000000000" in Path("/proc/self/status").read_text()
    held_sockets = []
    for line in sys.stdin:
        action = json.loads(line)
        try:
            name = action["action"]
            if name == "prepare":
                response, _ = request({"action": "prepare_network"})
                assert response["status"]["active"]
            elif name == "release":
                response, _ = request({"action": "release_network"})
                assert not response["status"]["active"]
            elif name == "reject":
                for operation in [
                    {
                        "action": "open_socket",
                        "transport": "tcp",
                        "interface_name": "lo",
                        "source_address": "127.0.0.1",
                        "destination": "172.29.0.2:11010",
                    },
                    {
                        "action": "open_socket",
                        "transport": "tcp",
                        "interface_name": "phys0",
                        "source_address": "172.29.0.99",
                        "destination": "172.29.0.2:11010",
                    },
                    {
                        "action": "open_socket",
                        "transport": "udp",
                        "interface_name": "phys0",
                        "source_address": "172.29.0.1",
                        "destination": "172.29.0.2:22",
                    },
                    {"action": "prepare_network", "command": "anything"},
                    {
                        "action": "open_listener",
                        "transport": "raw",
                        "interface_name": "phys0",
                        "source_address": "172.29.0.1",
                    },
                ]:
                    denied(operation)
            elif name == "outbound":
                for protocol in ["tcp", "udp"]:
                    _, endpoint = request(
                        {
                            "action": "open_socket",
                            "transport": protocol,
                            "interface_name": "phys0",
                            "source_address": "172.29.0.1",
                            "destination": "172.29.0.2:11010",
                        },
                        True,
                    )
                    with endpoint:
                        assert (
                            endpoint.getsockopt(
                                socket.SOL_SOCKET, socket.SO_BINDTODEVICE, 32
                            ).rstrip(b"\0")
                            == b"phys0"
                        )
                        assert endpoint.getsockname()[0] == "172.29.0.1"
                        endpoint.setblocking(True)
                        endpoint.settimeout(3)
                        endpoint.connect(("172.29.0.2", 11010))
                        endpoint.sendall(b"approved-underlay")
                        assert endpoint.recv(128) == b"approved-underlay"
            elif name == "hold_sockets":
                for protocol in ["tcp", "udp"]:
                    _, endpoint = request(
                        {
                            "action": "open_socket",
                            "transport": protocol,
                            "interface_name": "phys0",
                            "source_address": "172.29.0.1",
                            "destination": "172.29.0.2:11010",
                        },
                        True,
                    )
                    endpoint.setblocking(True)
                    endpoint.settimeout(3)
                    endpoint.connect(("172.29.0.2", 11010))
                    endpoint.sendall(b"before-helper-restart")
                    assert endpoint.recv(128) == b"before-helper-restart"
                    held_sockets.append(endpoint)
            elif name == "resume_sockets":
                for endpoint in held_sockets:
                    endpoint.sendall(b"after-helper-restart")
                    assert endpoint.recv(128) == b"after-helper-restart"
                    endpoint.close()
                held_sockets.clear()
            elif name == "listener":
                protocol = action["protocol"]
                _, endpoint = request(
                    {
                        "action": "open_listener",
                        "transport": protocol,
                        "interface_name": "phys0",
                        "source_address": "172.29.0.1",
                    },
                    True,
                )
                with endpoint:
                    assert endpoint.getsockname() == ("172.29.0.1", 11011)
                    assert (
                        endpoint.getsockopt(
                            socket.SOL_SOCKET, socket.SO_BINDTODEVICE, 32
                        ).rstrip(b"\0")
                        == b"phys0"
                    )
                    endpoint.setblocking(True)
                    endpoint.settimeout(4)
                    print(json.dumps({"ready": True}), flush=True)
                    if protocol == "tcp":
                        connected, _ = endpoint.accept()
                        with connected:
                            connected.sendall(connected.recv(128))
                    else:
                        payload, peer = endpoint.recvfrom(128)
                        endpoint.sendto(payload, peer)
            elif name == "engine_start":
                environment = dict(
                    os.environ,
                    AVIAN_NETWORK_HELPER_SOCKET=SOCKET,
                    AVIAN_RPC_TOKEN_FILE="/state/manager/rpc-token",
                    ET_REQUIRE_VERIFIED_MEMBERSHIP="1",
                )
                engine_log = open("/state/manager/core.log", "w")
                engine = subprocess.Popen(
                    [
                        "/engine/easytier-core",
                        "--config-file",
                        "/state/manager/core.toml",
                        "--rpc-portal",
                        "127.0.0.1:15888",
                        "--console-log-level",
                        "warn",
                    ],
                    env=environment,
                    stdout=engine_log,
                    stderr=subprocess.STDOUT,
                )

                def cli(*arguments):
                    return subprocess.run(
                        [
                            "/engine/easytier-cli",
                            "--rpc-portal",
                            "127.0.0.1:15888",
                            "-o",
                            "json",
                            *arguments,
                        ],
                        env=environment,
                        capture_output=True,
                        text=True,
                        timeout=6,
                    )

                def ready():
                    if engine.poll() is not None:
                        raise AssertionError(
                            Path("/state/manager/core.log").read_text()
                        )
                    return cli("node").returncode == 0

                wait_for(ready, 15)
                assert (
                    "CapEff:\t0000000000000000"
                    in Path(f"/proc/{engine.pid}/status").read_text()
                )
            elif name == "engine_listener":
                for protocol in ["tcp", "udp"]:
                    endpoint = f"{protocol}://172.29.0.1:11011?avian_bind=172.29.0.1&avian_interface=phys0"
                    added = cli("listener", "local-add", endpoint)
                    assert added.returncode == 0, added.stderr + added.stdout
                    listed = cli("listener", "local-list")
                    assert endpoint in listed.stdout, listed.stderr + listed.stdout
                    removed = cli("listener", "local-remove", endpoint)
                    assert removed.returncode == 0, removed.stderr + removed.stdout
                    assert endpoint not in cli("listener", "local-list").stdout
                    assert engine.poll() is None
                assert cli("listener", "local-add", endpoint).returncode == 0
            elif name == "engine_connect":
                for protocol in ["tcp", "udp"]:
                    endpoint = f"{protocol}://172.29.0.2:11012?avian_bind=172.29.0.1&avian_interface=phys0"
                    connected = cli("connector", "add", endpoint)
                    assert connected.returncode == 0, connected.stderr

                def both_transports():
                    observed = cli("peer").stdout
                    return any(
                        {"tcp", "udp"} <= set(peer.get("tunnel_proto", "").split(","))
                        for peer in json.loads(observed)
                    )

                try:
                    wait_for(both_transports, 15)
                except AssertionError:
                    raise AssertionError(cli("peer").stdout + cli("peer").stderr)
            elif name == "engine_stop":
                engine.terminate()
                engine.wait(timeout=5)
                engine_log.close()
            elif name == "tun":
                with open("/dev/net/tun", "r+b", buffering=0) as device:
                    fcntl.ioctl(
                        device, 0x400454CA, struct.pack("16sH", b"avian0", 0x1001)
                    )
                    print(json.dumps({"ready": True}), flush=True)
                    assert select.select([device], [], [], 4)[0]
                    packet = bytearray(device.read(65535))
                    header_length = (packet[0] & 15) * 4
                    assert packet[9] == 1 and packet[header_length] == 8
                    packet[12:16], packet[16:20] = packet[16:20], packet[12:16]
                    packet[10:12] = b"\0\0"
                    packet[10:12] = struct.pack(
                        "!H", checksum(bytes(packet[:header_length]))
                    )
                    packet[header_length] = 0
                    packet[header_length + 2 : header_length + 4] = b"\0\0"
                    packet[header_length + 2 : header_length + 4] = struct.pack(
                        "!H", checksum(bytes(packet[header_length:]))
                    )
                    device.write(packet)
            else:
                raise AssertionError("unknown test action")
            print(json.dumps({"passed": name}), flush=True)
        except Exception as error:
            print(json.dumps({"error": str(error)}), flush=True)


def remote():
    import threading

    def echo_udp():
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as endpoint:
            endpoint.bind(("172.29.0.2", 11010))
            while True:
                payload, peer = endpoint.recvfrom(4096)
                endpoint.sendto(payload, peer)

    threading.Thread(target=echo_udp, daemon=True).start()
    with socket.socket() as endpoint:
        endpoint.bind(("172.29.0.2", 11010))
        endpoint.listen()
        while True:
            connected, _ = endpoint.accept()
            with connected:
                while True:
                    payload = connected.recv(4096)
                    if not payload:
                        break
                    connected.sendall(payload)


def start_helper():
    # Match socket activation: helper has NET_ADMIN/NET_RAW, and no CAP_CHOWN.
    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    listener.bind(SOCKET)
    os.chown(SOCKET, 1000, 1000)
    os.chmod(SOCKET, 0o600)
    listener.listen(16)
    process = subprocess.Popen(
        [
            "setpriv",
            "--bounding-set=-all,+net_admin,+net_raw",
            BINARY,
            "--config",
            str(CONFIG),
        ],
        stdin=listener,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    listener.close()
    time.sleep(0.15)
    assert process.poll() is None, process.stderr.read().decode()
    capabilities = (
        Path(f"/proc/{process.pid}/status")
        .read_text()
        .split("CapEff:\t")[1]
        .splitlines()[0]
    )
    assert int(capabilities, 16) == (1 << 12) | (1 << 13)
    return process


def start_manager():
    return subprocess.Popen(
        [
            "setpriv",
            "--reuid=1000",
            "--regid=1000",
            "--clear-groups",
            "--bounding-set=-all",
            sys.executable,
            __file__,
            "manager",
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def send(process, action, **values):
    process.stdin.write(json.dumps({"action": action, **values}) + "\n")
    process.stdin.flush()


def result(process):
    assert select.select([process.stdout], [], [], 25)[0], process.poll()
    response = json.loads(process.stdout.readline())
    assert "error" not in response, response
    return response


def check(process, action, **values):
    send(process, action, **values)
    assert result(process) == {"passed": action}


def configure_engine():
    directory = STATE / "manager"
    directory.mkdir(mode=0o700)
    private_key = os.urandom(32)
    public_der = command(
        "openssl",
        "pkey",
        "-inform",
        "DER",
        "-pubout",
        "-outform",
        "DER",
        input=bytes.fromhex("302e020100300506032b656e04220420") + private_key,
    )
    public_key = base64.b64encode(public_der[-32:]).decode()
    configuration = f"""instance_name = "{'a'*64}"
hostname = "managed-fixture"
ipv4 = "10.88.0.1/24"
listeners = []
rpc_portal = "127.0.0.1:15888"
credential_file = "/state/manager/credentials.json"
stun_servers = ["127.0.0.1:9"]
stun_servers_v6 = ["[::1]:9"]
[network_identity]
network_name = "managed-fixture"
[secure_mode]
enabled = true
local_private_key = "{base64.b64encode(private_key).decode()}"
local_public_key = "{public_key}"
[flags]
dev_name = "avian0"
mtu = 1340
disable_p2p = true
disable_udp_hole_punching = true
disable_tcp_hole_punching = true
disable_upnp = true
accept_dns = false
"""
    (directory / "core.toml").write_text(configuration)
    (directory / "rpc-token").write_text(os.urandom(32).hex())
    (directory / "credentials.json").write_text(
        json.dumps(
            {
                "format": "avian-verified-membership-v1",
                "credentials": {
                    "a"
                    * 64: {
                        "pubkey": public_key,
                        "groups": [],
                        "allow_relay": True,
                        "allowed_proxy_cidrs": [],
                        "overlay_addresses": ["10.88.0.1"],
                        "reusable": False,
                        "expiry_unix": int(time.time()) + 600,
                        "created_at_unix": int(time.time()),
                    }
                },
            }
        )
    )
    remote_private = os.urandom(32)
    remote_public = base64.b64encode(
        command(
            "openssl",
            "pkey",
            "-inform",
            "DER",
            "-pubout",
            "-outform",
            "DER",
            input=bytes.fromhex("302e020100300506032b656e04220420") + remote_private,
        )[-32:]
    ).decode()
    trust = json.loads((directory / "credentials.json").read_text())
    trust["credentials"]["b" * 64] = dict(
        trust["credentials"]["a" * 64],
        pubkey=remote_public,
        overlay_addresses=["10.88.0.2"],
    )
    (directory / "credentials.json").write_text(json.dumps(trust))
    remote_config = (
        configuration.replace("a" * 64, "b" * 64)
        .replace("10.88.0.1/24", "10.88.0.2/24")
        .replace(
            "listeners = []",
            'listeners = ["tcp://172.29.0.2:11012", "udp://172.29.0.2:11012"]',
        )
        .replace(
            'local_private_key = "' + base64.b64encode(private_key).decode() + '"',
            'local_private_key = "' + base64.b64encode(remote_private).decode() + '"',
        )
        .replace(public_key, remote_public)
        .replace('dev_name = "avian0"', 'dev_name = "remoteavian"')
    )
    (directory / "remote.toml").write_text(remote_config)
    for path in directory.iterdir():
        path.chmod(0o600)
        os.chown(path, 1000, 1000)
    os.chown(directory, 1000, 1000)


class DnsCapture:
    """Observe DNS transport attempts on all interfaces in this disposable namespace."""

    def __init__(self):
        self.stop = threading.Event()
        self.packets = []
        self.socket = socket.socket(socket.AF_PACKET, socket.SOCK_RAW, socket.htons(3))
        self.socket.settimeout(0.1)
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()

    def read(self):
        while not self.stop.is_set():
            try:
                packet = self.socket.recv(65535)
            except TimeoutError:
                continue
            if len(packet) < 38 or packet[12:14] != b"\x08\x00":
                continue
            header_length = (packet[14] & 15) * 4
            offset = 14 + header_length
            if packet[23] in [6, 17] and len(packet) >= offset + 4:
                source, destination = struct.unpack("!HH", packet[offset : offset + 4])
                if 53 in [source, destination] and len(self.packets) < 100:
                    self.packets.append(packet)

    def close(self):
        self.stop.set()
        self.thread.join(timeout=2)
        self.socket.close()
        assert not self.packets, "owned connectivity attempted DNS transport"


def main():
    assert os.getuid() == 0 and os.environ.get("AVIAN_HELPER_CONTAINER") == "1"
    assert (
        os.stat("/proc/self/ns/net").st_ino != os.stat("/proc/1/ns/net").st_ino
        or Path("/.dockerenv").exists()
    )
    STATE.mkdir(exist_ok=True)
    os.chmod(STATE, 0o755)
    (STATE / "helper").mkdir(exist_ok=True)
    os.chmod(STATE / "helper", 0o711)
    CONFIG.write_text(
        json.dumps(
            {
                "allowed_uid": 1000,
                "runtime_directory": "/state/helper",
                "node_id": "a" * 64,
                "tun_name": "avian0",
                "overlay_address": "10.88.0.1",
                "overlay_network": "10.88.0.0/24",
                "mtu": 1280,
                "listen_port": 11011,
                "allowed_underlay_interfaces": ["phys0"],
            }
        )
    )
    CONFIG.chmod(0o600)
    command(IP, "link", "set", "lo", "up")
    command(IP, "netns", "add", "peer")
    command(IP, "netns", "exec", "peer", IP, "link", "set", "lo", "up")
    command(IP, "link", "add", "phys0", "type", "veth", "peer", "name", "remote0")
    command(IP, "link", "set", "remote0", "netns", "peer")
    command(IP, "addr", "add", "172.29.0.1/24", "dev", "phys0")
    command(IP, "link", "set", "phys0", "up")
    command(
        IP,
        "netns",
        "exec",
        "peer",
        IP,
        "addr",
        "add",
        "172.29.0.2/24",
        "dev",
        "remote0",
    )
    command(IP, "netns", "exec", "peer", IP, "link", "set", "remote0", "up")
    before_routes = command(IP, "-json", "-4", "route", "show", "table", "all")
    helper = start_helper()
    peer = subprocess.Popen(
        [IP, "netns", "exec", "peer", sys.executable, __file__, "remote"]
    )
    manager = start_manager()
    try:
        check(manager, "prepare")
        GATES["unprivileged_prepare"] = True
        check(manager, "reject")
        GATES["typed_scope_rejections"] = True
        check(manager, "outbound")
        GATES["pinned_tcp_udp_return"] = True
        for protocol in ["tcp", "udp"]:
            send(manager, "listener", protocol=protocol)
            assert result(manager) == {"ready": True}
            script = f'import socket; s=socket.socket(socket.AF_INET,socket.SOCK_{"STREAM" if protocol=="tcp" else "DGRAM"}); s.settimeout(3); s.connect(("172.29.0.1",11011)); s.sendall(b"listener-return"); assert s.recv(128)==b"listener-return"'
            command(IP, "netns", "exec", "peer", sys.executable, "-c", script)
            assert result(manager) == {"passed": "listener"}
        GATES["pinned_listeners_tcp_udp_return"] = True
        send(manager, "tun")
        assert result(manager) == {"ready": True}
        command("ping", "-c", "1", "-W", "3", "10.88.0.2")
        assert result(manager) == {"passed": "tun"}
        GATES["capability_free_tun_packets"] = True
        # Root can bypass socket DAC but must still fail kernel peer-UID admission.
        denied({"action": "status"})
        GATES["wrong_peer_uid_rejected"] = True
        manager.kill()
        manager.wait()
        wait_for(lambda: b"avian0" not in command(IP, "-json", "link", "show"))
        assert (
            command(IP, "-json", "-4", "route", "show", "table", "all") == before_routes
        )
        GATES["manager_crash_cleanup_preserves_routes"] = True
        manager = start_manager()
        check(manager, "prepare")
        check(manager, "hold_sockets")
        owned_link = command(IP, "-json", "link", "show", "avian0")
        helper.kill()
        helper.wait()
        helper = start_helper()
        assert command(IP, "-json", "link", "show", "avian0") == owned_link
        GATES["helper_restart_retains_live_owner_network"] = True
        check(manager, "resume_sockets")
        GATES["helper_restart_preserves_tcp_udp_descriptors"] = True
        for mutation, gate in [
            ("start", "reused_process_identity_rejected"),
            ("boot", "mismatched_boot_identity_cleaned"),
            ("legacy", "legacy_ownerless_journal_cleaned"),
        ]:
            helper.kill()
            helper.wait()
            journal_path = Path("/state/helper/network.json")
            journal = json.loads(journal_path.read_text())
            if mutation == "start":
                journal["lease_owner"]["started_at_ticks"] += 1
            elif mutation == "boot":
                journal["lease_owner"][
                    "boot_id"
                ] = "00000000-0000-0000-0000-000000000000"
            else:
                journal["version"] = 1
                for field in [
                    "lease_owner",
                    "listen_port",
                    "allowed_underlay_interfaces",
                ]:
                    journal.pop(field)
            journal_path.write_text(json.dumps(journal))
            journal_path.chmod(0o600)
            helper = start_helper()
            assert b"avian0" not in command(IP, "-json", "link", "show")
            GATES[gate] = True
            check(manager, "prepare")
        helper.kill()
        helper.wait()
        manager.kill()
        manager.wait()
        helper = start_helper()
        assert b"avian0" not in command(IP, "-json", "link", "show")
        GATES["dead_owner_journal_cleaned"] = True
        manager = start_manager()
        command(IP, "link", "add", "avian0", "type", "dummy")
        send(manager, "prepare")
        assert "error" in json.loads(manager.stdout.readline())
        assert b"avian0" in command(IP, "-json", "link", "show")
        command(IP, "link", "delete", "avian0")
        command(IP, "route", "add", "10.88.0.0/25", "dev", "phys0")
        send(manager, "prepare")
        assert "error" in json.loads(manager.stdout.readline())
        assert b"10.88.0.0/25" in command(IP, "-json", "-4", "route", "show")
        GATES["conflicting_interface_and_route_preserved"] = True
        command(IP, "route", "delete", "10.88.0.0/25", "dev", "phys0")
        check(manager, "prepare")
        if Path("/engine/easytier-core").exists():
            configure_engine()
            dns_capture = DnsCapture()
            check(manager, "engine_start")
            GATES["capability_free_managed_engine_startup"] = True
            check(manager, "engine_listener")
            GATES["live_authenticated_listener_add_remove"] = True
            with open("/state/remote.log", "w") as remote_log:
                remote_engine = subprocess.Popen(
                    [
                        IP,
                        "netns",
                        "exec",
                        "peer",
                        "/engine/easytier-core",
                        "--config-file",
                        "/state/manager/remote.toml",
                        "--rpc-portal",
                        "127.0.0.1:15889",
                        "--console-log-level",
                        "warn",
                    ],
                    env=dict(
                        os.environ,
                        ET_REQUIRE_VERIFIED_MEMBERSHIP="1",
                        AVIAN_OWNED_CONNECTIVITY_ONLY="1",
                    ),
                    stdout=remote_log,
                    stderr=subprocess.STDOUT,
                )
                try:
                    time.sleep(0.3)
                    check(manager, "engine_connect")
                    GATES["capability_free_engine_pinned_tcp_udp_connectors"] = True

                    def bidirectional_overlay():
                        assert remote_engine.poll() is None, Path(
                            "/state/remote.log"
                        ).read_text()
                        outgoing = subprocess.run(
                            ["ping", "-c", "1", "-W", "1", "10.88.0.2"],
                            capture_output=True,
                        )
                        incoming = subprocess.run(
                            [
                                IP,
                                "netns",
                                "exec",
                                "peer",
                                "ping",
                                "-c",
                                "1",
                                "-W",
                                "1",
                                "10.88.0.1",
                            ],
                            capture_output=True,
                        )
                        return outgoing.returncode == incoming.returncode == 0

                    wait_for(bidirectional_overlay, 20)
                    GATES["capability_free_engine_verified_overlay_packets"] = True
                finally:
                    remote_engine.terminate()
                    remote_engine.wait(timeout=5)
            check(manager, "engine_stop")
            dns_capture.close()
            GATES["owned_connectivity_emits_no_dns_packets"] = True
        check(manager, "release")
        assert (
            command(IP, "-json", "-4", "route", "show", "table", "all") == before_routes
        )
        GATES["explicit_release_preserves_routes"] = True
        binaries = {"helper": BINARY}
        if Path("/engine/easytier-core").exists():
            binaries.update(core="/engine/easytier-core", cli="/engine/easytier-cli")
        hashes = {
            name: hashlib.sha256(Path(path).read_bytes()).hexdigest()
            for name, path in binaries.items()
        }
        print(
            json.dumps(
                {
                    "passed": all(GATES.values()),
                    "gates": GATES,
                    "binary_sha256": hashes,
                },
                sort_keys=True,
            )
        )
    except Exception:
        print(
            command(IP, "-json", "-4", "route", "show", "table", "all").decode(),
            file=sys.stderr,
        )
        print("helper exit", helper.poll(), file=sys.stderr)
        if helper.poll() is not None:
            print(helper.stderr.read().decode(), file=sys.stderr)
        raise
    finally:
        manager.kill()
        manager.wait()
        helper.terminate()
        try:
            helper.wait(timeout=5)
        except subprocess.TimeoutExpired:
            helper.kill()
            helper.wait()
        peer.kill()
        peer.wait()


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "manager":
        manager()
    elif len(sys.argv) > 1 and sys.argv[1] == "remote":
        remote()
    else:
        main()
