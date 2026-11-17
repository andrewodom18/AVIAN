"""Test the Android component in a disposable headless emulator and isolated Linux peer."""

import argparse
import base64
import hashlib
import importlib.util
import json
from pathlib import Path

from build import PATCH_NAMES
import secrets
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import zipfile

DIRECTORY = Path(__file__).resolve().parent
REPOSITORY = DIRECTORY.parents[1]
PACKAGE = "org.bespin.avian.qualification"
SERIAL = "emulator-5582"


def execute(arguments, **options):
    return subprocess.run(
        [str(value) for value in arguments],
        check=True,
        capture_output=True,
        text=True,
        **options,
    ).stdout.strip()


def wait_until(predicate, seconds):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.2)
    return False


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def create_fixture(directory, cli, port):
    def command(*arguments):
        return json.loads(execute([cli, *arguments]))

    administrator = directory / "administrator"
    command("init", "--state-dir", administrator, "--network", "android-native-test")
    records = {}
    for index, name in enumerate(("android", "relay", "target"), 1):
        identity = command("keygen", "--output", directory / (name + ".identity"))
        secret = secrets.token_bytes(32)
        private_der = bytes.fromhex("302e020100300506032b656e04220420") + secret
        public = subprocess.run(
            ["openssl", "pkey", "-inform", "DER", "-pubout", "-outform", "DER"],
            input=private_der,
            check=True,
            capture_output=True,
        ).stdout[-32:]
        member = command(
            "--config",
            administrator / "avian.toml",
            "enroll",
            "--identity-public-key",
            identity["identity_public_key"],
            "--transport-public-key",
            public.hex(),
            "--address",
            "10.201.0." + str(index),
            "--relay",
            "--output",
            directory / (name + "-member.json"),
        )
        records[name] = {
            "node_id": member["node_id"],
            "private": base64.b64encode(secret).decode(),
            "public": base64.b64encode(public).decode(),
        }
    command(
        "--config",
        administrator / "avian.toml",
        "export-engine-trust",
        "--output",
        directory / "engine-trust.json",
    )
    assets = directory / "assets"
    assets.mkdir()
    for name in ("trust-root.json", "membership.json"):
        shutil.copyfile(administrator / name, assets / name)
    (assets / "transport.key").write_text(records["android"]["private"])
    shutil.copyfile(directory / "android.identity", assets / "identity.key")
    (assets / "runtime.json").write_text(
        json.dumps(
            {
                "version": 1,
                "exchange_port": 4191,
                "node_id": records["android"]["node_id"],
                "overlay_address": "10.201.0.1",
                "overlay_network": "10.201.0.0/24",
                "peers": [f"tcp://10.0.2.2:{port}"],
                "exchange_peers": [
                    {
                        "node_id": records["relay"]["node_id"],
                        "service_endpoint": "10.201.0.2:4192",
                        "revocation_endpoint": "10.201.0.2:9180",
                    }
                ],
            }
        )
    )
    fixture = directory / "peers"
    fixture.mkdir(mode=0o700)
    for index, name in enumerate(("relay", "target"), 2):
        state = fixture / name
        state.mkdir(mode=0o700)
        for filename in ("trust-root.json", "membership.json"):
            shutil.copyfile(administrator / filename, state / filename)
        shutil.copyfile(directory / "engine-trust.json", state / "engine-trust.json")
        shutil.copyfile(directory / (name + ".identity"), state / "identity.key")
        (state / "transport.key").write_text(records[name]["private"])
        (state / "api-token").write_text(secrets.token_hex(32))
        peer = records[name]
        listener = "0.0.0.0:11010" if name == "relay" else "172.28.0.2:11011"
        seed = "" if name == "relay" else '\n[[peer]]\nuri = "tcp://172.28.0.1:11010"\n'
        (state / "engine.toml").write_text(
            f"""instance_name = "{peer['node_id']}"
hostname = "{peer['node_id']}"
ipv4 = "10.201.0.{index}/24"
listeners = ["tcp://{listener}"]
credential_file = "/state/{name}/engine-trust.json"
stun_servers = ["127.0.0.1:9"]
stun_servers_v6 = ["[::1]:9"]
{seed}
[network_identity]
network_name = "android-native-test"
[secure_mode]
enabled = true
local_private_key = "{peer['private']}"
local_public_key = "{peer['public']}"
[flags]
dev_name = "avian-{name}"
mtu = 1380
disable_p2p = true
disable_tcp_hole_punching = true
disable_udp_hole_punching = true
disable_upnp = true
accept_dns = false
"""
        )
        peers = [("target", 3), ("android", 1)] if name == "relay" else [("relay", 2)]
        configuration = f"""listen = "127.0.0.1:9181"
token_file = "/state/{name}/api-token"
membership_file = "/state/{name}/membership.json"
trust_root_file = "/state/{name}/trust-root.json"
[services]
state_file = "/state/{name}/services.json"
identity_key_file = "/state/{name}/identity.key"
[service_exchange]
listen = "10.201.0.{index}:4193"
"""
        for other, address in peers:
            configuration += f"""[[service_exchange.peers]]
node_id = "{records[other]['node_id']}"
address = "10.201.0.{address}:{4191 if other == 'android' else 4193}"
"""
        (state / "avian.toml").write_text(configuration)
        for path in state.iterdir():
            path.chmod(0o600)
    revocations = fixture / "revocations"
    revocations.mkdir(mode=0o700)
    for filename in ("trust-root.json", "membership.json"):
        shutil.copyfile(administrator / filename, revocations / filename)
    (directory / "records.json").write_text(
        json.dumps({name: record["node_id"] for name, record in records.items()})
    )
    return assets


def build_apk(sdk, directory, native_directory, assets, storage_only=False):
    tools = sdk / "build-tools/36.0.0"
    android_jar = sdk / "platforms/android-36/android.jar"
    classes = directory / "classes"
    dex = directory / "dex"
    classes.mkdir()
    dex.mkdir()
    sources = sorted((DIRECTORY / "java").rglob("*.java")) + sorted(
        (DIRECTORY / "tests/java").rglob("*.java")
    )
    execute(
        [
            "javac",
            "-source",
            "8",
            "-target",
            "8",
            "-bootclasspath",
            android_jar,
            "-d",
            classes,
            *sources,
        ]
    )
    execute(
        [
            tools / "d8",
            "--lib",
            android_jar,
            "--min-api",
            "29",
            "--output",
            dex,
            *classes.rglob("*.class"),
        ]
    )
    unsigned = directory / "unsigned.apk"
    link = [
        tools / "aapt2",
        "link",
        "-I",
        android_jar,
        "--manifest",
        DIRECTORY / "tests/AndroidManifest.xml",
        "-o",
        unsigned,
    ]
    if assets:
        link.extend(["-A", assets])
    execute(link)
    with zipfile.ZipFile(unsigned, "a") as archive:
        archive.write(dex / "classes.dex", "classes.dex")
        if native_directory:
            for path in sorted((native_directory / "aar/jni/arm64-v8a").glob("*.so")):
                if storage_only and path.name not in {
                    "libavian_storage_android.so",
                    "libc++_shared.so",
                }:
                    continue
                archive.write(path, "lib/arm64-v8a/" + path.name)
    aligned = directory / "aligned.apk"
    execute([tools / "zipalign", "-P", "16", "-f", "4", unsigned, aligned])
    keystore = directory / "test.keystore"
    execute(
        [
            "keytool",
            "-genkeypair",
            "-keystore",
            keystore,
            "-storepass",
            "android",
            "-keypass",
            "android",
            "-alias",
            "test",
            "-dname",
            "CN=Isolated qualification",
            "-keyalg",
            "RSA",
            "-validity",
            "1",
        ]
    )
    apk = directory / "qualification.apk"
    execute(
        [
            tools / "apksigner",
            "sign",
            "--ks",
            keystore,
            "--ks-pass",
            "pass:android",
            "--out",
            apk,
            aligned,
        ]
    )
    return apk


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--avd", default="Medium_Phone")
    parser.add_argument(
        "--android-sdk", type=Path, default=Path.home() / "Library/Android/sdk"
    )
    parser.add_argument(
        "--native-directory", type=Path, default=DIRECTORY / ".local/build"
    )
    parser.add_argument("--engine-directory", type=Path)
    parser.add_argument("--linux-binary-directory", type=Path)
    parser.add_argument(
        "--host-binary-directory", type=Path, default=REPOSITORY / "target/debug"
    )
    parser.add_argument("--keystore-only", action="store_true")
    parser.add_argument("--allow-development", action="store_true")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--inspection-seconds",
        type=int,
        default=0,
        help="Keep failed disposable fixtures alive for at most 60 seconds for diagnostics",
    )
    arguments = parser.parse_args()
    if not 0 <= arguments.inspection_seconds <= 60:
        parser.error("--inspection-seconds must be within 0..60")
    if not arguments.keystore_only and (
        arguments.engine_directory is None or arguments.linux_binary_directory is None
    ):
        parser.error(
            "Native overlay tests require --engine-directory and --linux-binary-directory"
        )
    sdk = arguments.android_sdk.resolve()
    adb = sdk / "platform-tools/adb"
    report = {
        "scope": "disposable Android emulator; no physical multi-radio qualification",
        "gates": [],
        "qualified_for_cutover": False,
        "physical_multi_network": "not_run",
        "system_consent_withdrawal": "not_run",
        "harness_sha256": {
            str(path.relative_to(DIRECTORY)): sha256(path)
            for path in [
                Path(__file__).resolve(),
                DIRECTORY / "tests/AndroidManifest.xml",
                *sorted((DIRECTORY / "tests").glob("*.py")),
                *sorted((DIRECTORY / "tests/java").rglob("*.java")),
            ]
        },
    }

    def device(*values):
        return execute([adb, "-s", SERIAL, *values], timeout=15)

    def wake_device():
        device("shell", "input", "keyevent", "KEYCODE_WAKEUP")
        # A headless, non-secure emulator can defer new activity intents behind
        # its keyguard even while native foreground networking keeps running.
        device("shell", "wm", "dismiss-keyguard")

    def record(name, passed):
        report["gates"].append({"name": name, "status": "pass" if passed else "fail"})

    emulator = None
    sample_reader = None
    container = None
    network = None
    ingress = None
    temporary_fixture = None
    try:
        if SERIAL in execute([adb, "devices"]):
            raise RuntimeError("Dedicated emulator port5582 is in use")
        temporary_fixture = tempfile.TemporaryDirectory(prefix="avian-native-android-")
        directory = Path(temporary_fixture.name)
        assets = None
        native = arguments.native_directory.resolve()
        if arguments.keystore_only:
            storage_bridge = native / "aar/jni/arm64-v8a/libavian_storage_android.so"
            build = json.loads((native / "build.json").read_text())
            expected = build["binary_sha256"].get(storage_bridge.name)
            if expected is None or sha256(storage_bridge) != expected:
                raise RuntimeError("Android protected-storage bridge hash mismatch")
            report["storage_bridge_sha256"] = expected
            report["storage_bridge_sources"] = {
                name: expected
                for name, expected in build["native_sources"].items()
                if "storage" in name
            }
            if not report["storage_bridge_sources"] or any(
                sha256(DIRECTORY / name) != expected
                for name, expected in report["storage_bridge_sources"].items()
            ):
                raise RuntimeError("Android protected-storage bridge source mismatch")
        if not arguments.keystore_only:
            native = arguments.native_directory.resolve()
            build = json.loads((native / "build.json").read_text())
            if build.get("development_only") and not arguments.allow_development:
                raise RuntimeError(
                    "Development artifacts require --allow-development and do not establish release qualification"
                )
            report["development_only"] = bool(build.get("development_only"))
            if build.get("engine_build") != "pass":
                raise RuntimeError("Native engine artifact has not built successfully")
            if build["avian_workspace_sha256"] != sha256(REPOSITORY / "Cargo.toml"):
                raise RuntimeError(
                    "AVIAN workspace differs from native build provenance"
                )
            if build["manifest_sha256"] != sha256(DIRECTORY / "AndroidManifest.xml"):
                raise RuntimeError(
                    "Android manifest differs from native build provenance"
                )
            if sha256(native / "avian-android.aar") != build["aar_sha256"]:
                raise RuntimeError("Android library hash mismatch")
            current_java = {
                str(path.relative_to(DIRECTORY)): sha256(path)
                for path in sorted((DIRECTORY / "java").rglob("*.java"))
            }
            if current_java != build["java_sources"]:
                raise RuntimeError(
                    "Android Java sources differ from native build provenance"
                )
            for name, expected in build["native_sources"].items():
                if sha256(DIRECTORY / name) != expected:
                    raise RuntimeError("Native adapter differs from build provenance")
            for name, expected in build["verification_sources"].items():
                if sha256(REPOSITORY / name) != expected:
                    raise RuntimeError(
                        "Membership verifier differs from build provenance"
                    )
            current_app_sources = {
                str(path.relative_to(REPOSITORY)): sha256(path)
                for path in sorted((REPOSITORY / "apps/avian").rglob("*"))
                if path.is_file()
                and (path.suffix == ".rs" or path.name == "Cargo.toml")
            }
            if current_app_sources != build["app_library_sources"]:
                raise RuntimeError(
                    "Shared application library differs from native build provenance"
                )
            specification = importlib.util.spec_from_file_location(
                "engine_builder", REPOSITORY / "qualification/build.py"
            )
            builder = importlib.util.module_from_spec(specification)
            specification.loader.exec_module(builder)
            helper_hashes = builder.prepare_shared_network_helper(
                directory / "helper-provenance"
            )
            planner_hashes = builder.prepare_shared_routing(
                directory / "planner-provenance"
            )
            if helper_hashes != build["shared_network_helper_sha256"]:
                raise RuntimeError(
                    "Shared helper client differs from native build provenance"
                )
            if build["patches"][-1] != {
                "name": "android-sockets.patch",
                "sha256": sha256(DIRECTORY / "android-sockets.patch"),
            }:
                raise RuntimeError("Android socket patch differs from build provenance")
            for name, expected in build["binary_sha256"].items():
                if sha256(native / "aar/jni/arm64-v8a" / name) != expected:
                    raise RuntimeError("Native binary hash mismatch")
            expected_patches = [
                {
                    "name": name + ".patch",
                    "sha256": sha256(
                        REPOSITORY / "qualification/patches" / (name + ".patch")
                    ),
                }
                for name in PATCH_NAMES
            ]
            peer_build = json.loads(
                (arguments.engine_directory / "build.json").read_text()
            )
            if (
                build["patches"][:-1] != expected_patches
                or peer_build["patches"] != expected_patches
            ):
                raise RuntimeError(
                    "Android and Linux peer must use the same current canonical engine patches"
                )
            if peer_build.get("shared_routing_sha256") != planner_hashes:
                raise RuntimeError("Linux peer uses a different shared routing policy")
            report["shared_routing_sha256"] = planner_hashes
            if peer_build.get("shared_network_helper_sha256") != helper_hashes:
                raise RuntimeError("Linux peer uses a different shared helper client")
            for name, expected in peer_build["binary_sha256"].items():
                if sha256(arguments.engine_directory / name) != expected:
                    raise RuntimeError("Linux peer binary hash mismatch")
            report["native_build"] = build
            report["linux_peer_build"] = peer_build
            with socket.socket() as reservation:
                reservation.bind(("127.0.0.1", 0))
                port = reservation.getsockname()[1]
            assets = create_fixture(
                directory,
                arguments.host_binary_directory.resolve() / "avianctl",
                port,
            )
            report["linux_service_binary_sha256"] = {
                name: sha256(arguments.linux_binary_directory / name)
                for name in ("aviand", "avianctl", "avian-revocationd")
            }
            controls = directory / "control"
            controls.mkdir(mode=0o700)
            (controls / "control.json").write_text("{}")
            network = "avian-android-" + secrets.token_hex(4)
            execute(["docker", "network", "create", "--internal", network])
            results = directory / "results"
            results.mkdir()
            container = execute(
                [
                    "docker",
                    "run",
                    "-d",
                    "--rm",
                    "--privileged",
                    "--network",
                    network,
                    "--tmpfs",
                    "/state:mode=0700",
                    "--mount",
                    f"type=bind,source={DIRECTORY},target=/android,readonly",
                    "--mount",
                    f"type=bind,source={arguments.engine_directory.resolve()},target=/engine,readonly",
                    "--mount",
                    f"type=bind,source={directory / 'peers'},target=/fixture,readonly",
                    "--mount",
                    f"type=bind,source={controls},target=/control,readonly",
                    "--mount",
                    f"type=bind,source={arguments.linux_binary_directory.resolve()},target=/services,readonly",
                    "--mount",
                    f"type=bind,source={results},target=/results",
                    "--entrypoint",
                    "python3",
                    "avian-qualification:local",
                    "/android/tests/peer.py",
                ]
            )

            def peer_ready():
                path = results / "peer.json"
                target = results / "target.json"
                return (
                    path.exists()
                    and target.exists()
                    and json.loads(path.read_text()).get("ready") is True
                    and json.loads(target.read_text()).get("ready") is True
                )

            if not wait_until(peer_ready, 25):
                raise RuntimeError("Isolated Linux peer did not become ready")
            specification = importlib.util.spec_from_file_location(
                "avian_test_ingress", DIRECTORY / "tests/ingress.py"
            )
            ingress_module = importlib.util.module_from_spec(specification)
            sys.modules[specification.name] = ingress_module
            specification.loader.exec_module(ingress_module)
            ingress = ingress_module.TransparentIngressBroker(container).start()
            runtime_configuration = json.loads((assets / "runtime.json").read_text())
            runtime_configuration["peers"] = [f"tcp://10.0.2.2:{ingress.port}"]
            (assets / "runtime.json").write_text(json.dumps(runtime_configuration))
            with socket.create_connection(("127.0.0.1", ingress.port), timeout=3):
                pass
            report["underlay_transport"] = {
                "kind": ingress.transport_kind,
                "host_port": ingress.port,
                "public_container_port": False,
                "terminates_mesh_authentication": False,
                "physical_network_qualification": False,
            }

        apk = build_apk(
            sdk, directory, native, assets, storage_only=arguments.keystore_only
        )
        report["apk_sha256"] = sha256(apk)
        log = (directory / "emulator.log").open("w")
        emulator = subprocess.Popen(
            [
                str(sdk / "emulator/emulator"),
                "-avd",
                arguments.avd,
                "-port",
                "5582",
                "-read-only",
                "-no-window",
                "-no-audio",
                "-no-snapshot-load",
                "-no-snapshot-save",
                "-gpu",
                "swiftshader_indirect",
            ],
            stdout=log,
            stderr=subprocess.STDOUT,
        )

        def booted():
            if emulator.poll() is not None:
                raise RuntimeError("Headless emulator exited")
            try:
                return device("shell", "getprop", "sys.boot_completed") == "1"
            except subprocess.SubprocessError:
                return False

        if not wait_until(booted, 120):
            raise RuntimeError("Headless emulator boot timed out")
        report["original_screen_timeout_ms"] = device(
            "shell", "settings", "get", "system", "screen_off_timeout"
        )
        device("shell", "settings", "put", "system", "screen_off_timeout", "600000")
        wake_device()
        report["initial_power_state"] = device("shell", "dumpsys", "power")[:16384]
        report["initial_keyguard_state"] = device(
            "shell", "dumpsys", "window", "policy"
        )[:16384]
        device("install", "-r", apk)
        device("shell", "appops", "set", PACKAGE, "ACTIVATE_VPN", "allow")
        device("logcat", "-G", "16M")
        device("logcat", "-c")
        action = "keystore" if arguments.keystore_only else "start"
        device(
            "shell",
            "am",
            "start",
            "-n",
            PACKAGE + "/org.bespin.avian.QualificationActivity",
            "--es",
            "action",
            action,
        )

        def samples():
            records = []
            chunks = {}
            for line in device(
                "logcat", "-d", "-s", "AVIAN_NATIVE_TEST:I", "*:S"
            ).splitlines():
                offset = line.find("{")
                if offset < 0:
                    continue
                row = json.loads(line[offset:])
                if row.get("kind") != "sample_chunk":
                    records.append(row)
                    continue
                count, index = row["count"], row["index"]
                if (
                    not isinstance(count, int)
                    or not 1 <= count <= 64
                    or not isinstance(index, int)
                    or not 0 <= index < count
                ):
                    raise ValueError("Invalid Android sample chunk envelope")
                group = chunks.setdefault(row["id"], {"count": count, "parts": {}})
                if group["count"] != count or len(row["payload"]) > 2000:
                    raise ValueError("Inconsistent Android sample chunks")
                previous = group["parts"].setdefault(index, row["payload"])
                if previous != row["payload"]:
                    raise ValueError("Conflicting Android sample chunks")
                if len(group["parts"]) == count and not group.get("complete"):
                    encoded = "".join(group["parts"][part] for part in range(count))
                    records.append(json.loads(base64.b64decode(encoded, validate=True)))
                    group["complete"] = True
            return records

        sample_reader = samples
        if not wait_until(
            lambda: any(
                row.get("kind") in ("keystore", "failure") for row in samples()
            ),
            20,
        ):
            raise RuntimeError("Keystore qualification did not report")
        keystore = next((row for row in samples() if row["kind"] == "keystore"), None)
        if keystore is None:
            raise RuntimeError("Keystore initialization failed")
        for field in (
            "round_trip",
            "randomized_ciphertext",
            "tamper_rejected",
            "overwrite_rejected",
            "plaintext_rejected",
        ):
            record("keystore_" + field, keystore[field])
        if not arguments.keystore_only:

            def snapshots():
                return [row for row in samples() if row.get("kind") == "snapshot"]

            records = json.loads((directory / "records.json").read_text())

            def latest_owner(owner):
                pages = snapshots()
                return (
                    [
                        item
                        for item in pages[-1]["services"]["items"]
                        if item["announcement"]["claims"]["owner_node_id"]
                        == records[owner]
                    ]
                    if pages
                    else []
                )

            def action(name):
                before = sum(
                    row.get("kind") == "action_received" and row.get("action") == name
                    for row in samples()
                )
                device(
                    "shell",
                    "am",
                    "broadcast",
                    "-n",
                    PACKAGE + "/org.bespin.avian.QualificationReceiver",
                    "--es",
                    "action",
                    name,
                )
                acknowledged = wait_until(
                    lambda: sum(
                        row.get("kind") == "action_received"
                        and row.get("action") == name
                        for row in samples()
                    )
                    > before,
                    5,
                )
                if not acknowledged:
                    raise RuntimeError(
                        "Android test action was not acknowledged: " + name
                    )

            def restart_count():
                return sum(row.get("kind") == "runtime_restarted" for row in samples())

            def latest_peer_id():
                pages = snapshots()
                return pages[-1].get("engine", {}).get("peer_id") if pages else None

            def set_control(**values):
                temporary = controls / "control.next"
                temporary.write_text(json.dumps(values))
                temporary.replace(controls / "control.json")

            def event_seen(kind, after=0):
                return any(
                    item.get("kind") == kind
                    for row in snapshots()[after:]
                    for item in row["events"].get("items", [])
                )

            def echoes():
                return [
                    row
                    for row in samples()
                    if row.get("kind") == "overlay_echo" and row.get("received")
                ]

            if not wait_until(
                lambda: all(
                    sum(row.get("peer") == peer for row in echoes()) >= 3
                    for peer in ("10.201.0.2", "10.201.0.3")
                ),
                35,
            ):
                raise RuntimeError("Real native TUN overlay echo did not become ready")
            record("native_engine_tun_overlay_echo", True)
            record(
                "engine_underlay_socket_host_callback",
                all(row["prepared_sockets"] > 0 for row in echoes()),
            )

            def measured_mobile_capacity():
                for row in snapshots():
                    for event in row["events"].get("items", []):
                        if event.get("kind") != "capacity_probe_completed":
                            continue
                        evidence = event.get("detail", {})
                        eligibility = evidence.get("eligibility", {})
                        if (
                            evidence.get("method")
                            == "bounded_probe_delivered_rate_lower_bound"
                            and evidence.get("delivered_rate_lower_bound_bps", 0) > 0
                            and evidence.get("delivered_payload_bytes", 0) > 0
                            and eligibility.get("metered") is False
                            and eligibility.get("policy_valid") is True
                            and eligibility.get("quality_fresh") is True
                            and eligibility.get("queued_packets") in (False, 0)
                        ):
                            return True
                return False

            record(
                "automatic_capacity_uses_confirmed_unmetered_delivery",
                wait_until(measured_mobile_capacity, 15),
            )

            def relay_has_android_service():
                script = (
                    "import json,urllib.request; "
                    "p=json.load(urllib.request.urlopen('http://10.201.0.2:4193/v1/service-announcements?limit=100',timeout=2)); "
                    "print(json.dumps([r['claims']['owner_node_id'] for r in p['records']]))"
                )
                owners = json.loads(
                    execute(
                        ["docker", "exec", container, "python3", "-c", script],
                        timeout=5,
                    )
                )
                return records["android"] in owners

            visible = wait_until(relay_has_android_service, 12)
            record("android_owner_publication_reaches_relay", visible)
            action("pause_publication")
            expired_remote = wait_until(lambda: not relay_has_android_service(), 9)
            action("resume_publication")
            republished = wait_until(relay_has_android_service, 12)
            record(
                "application_claims_expire_without_renewal",
                visible and expired_remote and republished,
            )
            learned = wait_until(lambda: bool(latest_owner("target")), 15)
            record("owner_signed_services_cross_relay", learned)
            if not learned:
                raise RuntimeError(
                    "Android did not learn the target's signed services through its relay"
                )
            record(
                "advertisements_do_not_claim_reachability",
                all(
                    item["reachability"] == "unobserved"
                    for item in latest_owner("target")
                ),
            )
            marker = len(snapshots())
            set_control(service_fault="tamper")
            rejected = wait_until(
                lambda: event_seen("service_update_rejected", marker), 8
            )
            expired = wait_until(lambda: not latest_owner("target"), 12)
            record("tampered_owner_signature_rejected", rejected and expired)
            marker = len(snapshots())
            set_control(service_fault="wrong_network")
            record(
                "wrong_network_services_rejected",
                wait_until(lambda: event_seen("service_update_rejected", marker), 8)
                and not latest_owner("target"),
            )
            set_control()
            record(
                "valid_service_exchange_recovers",
                wait_until(lambda: bool(latest_owner("target")), 12),
            )
            publications = [
                row["revision"]
                for row in samples()
                if row.get("kind") == "local_publication"
            ]
            previous_restart = restart_count()
            previous_peer = latest_peer_id()
            action("restart_runtime")
            restarted = wait_until(
                lambda: restart_count() > previous_restart
                and latest_peer_id() is not None
                and latest_peer_id() != previous_peer,
                15,
            )
            resumed = wait_until(
                lambda: any(
                    row.get("kind") == "local_publication"
                    and row["revision"] > max(publications, default=0)
                    for row in samples()
                ),
                15,
            )
            record(
                "restart_preserves_local_publication_revision",
                restarted and resumed,
            )
            if not restarted:
                raise RuntimeError(
                    "Android runtime restart did not produce a ready replacement instance"
                )
            before = len(echoes())
            device("shell", "input", "keyevent", "KEYCODE_SLEEP")
            power = device("shell", "dumpsys", "power")
            asleep = any(
                marker in power
                for marker in ("mWakefulness=Asleep", "mWakefulness=Dozing")
            )
            record(
                "screen_off_native_overlay_echo",
                asleep and wait_until(lambda: len(echoes()) >= before + 3, 10),
            )
            wake_device()
            # Bound clock conversion with a host round-trip interval. Receipt
            # timestamps count stale replies as deliveries, even after a timeout.
            before_clock = time.monotonic()
            uptime = float(device("shell", "cat", "/proc/uptime").split()[0])
            after_clock = time.monotonic()
            execute(
                [
                    arguments.host_binary_directory.resolve() / "avianctl",
                    "--config",
                    directory / "administrator/avian.toml",
                    "revoke",
                    "--node",
                    records["target"],
                    "--output",
                    directory / "target-revocation.json",
                ]
            )
            update = controls / "membership.next"
            shutil.copyfile(directory / "administrator/membership.json", update)
            published_at = time.monotonic()
            update.replace(controls / "membership.json")
            # Use the earliest possible Android deadline: this cannot hide a
            # late delivery behind command latency or coarse uptime precision.
            android_deadline = uptime + published_at - after_clock + 2
            clock_uncertainty = after_clock - before_clock + 0.01
            report["revocation_clock_uncertainty_seconds"] = clock_uncertainty
            applied = wait_until(
                lambda: snapshots()
                and snapshots()[-1]["services"].get("revocation_revision", 0) > 0,
                8,
            )
            time.sleep(3)
            late = [
                row
                for row in echoes()
                if row.get("peer") == "10.201.0.3"
                and row.get("receipt_elapsed", 0) is not None
                and row["receipt_elapsed"] > android_deadline
            ]
            target_attempts = [
                row
                for row in samples()
                if row.get("kind") == "overlay_echo"
                and row.get("peer") == "10.201.0.3"
                and row.get("attempt_elapsed", 0) > android_deadline
            ]
            relay_alive = (
                sum(
                    row.get("peer") == "10.201.0.2"
                    and row.get("receipt_elapsed", 0) is not None
                    and row["receipt_elapsed"] > android_deadline
                    for row in echoes()
                )
                >= 3
            )
            record(
                "signed_revocation_stops_target_within_two_seconds",
                applied and not late and relay_alive and len(target_attempts) >= 3,
            )
            record(
                "revoked_service_owner_removed",
                applied and not latest_owner("target"),
            )
            before_restart = len(samples())
            previous_restart = restart_count()
            previous_peer = latest_peer_id()
            action("restart_runtime")

            def verified_restart_after_revocation():
                rows = samples()[before_restart:]
                relay_deliveries = [
                    row
                    for row in rows
                    if row.get("kind") == "overlay_echo"
                    and row.get("peer") == "10.201.0.2"
                    and row.get("received")
                ]
                target_attempts = [
                    row
                    for row in rows
                    if row.get("kind") == "overlay_echo"
                    and row.get("peer") == "10.201.0.3"
                ]
                return (
                    restart_count() > previous_restart
                    and latest_peer_id() is not None
                    and latest_peer_id() != previous_peer
                    and len(relay_deliveries) >= 3
                    and len(target_attempts) >= 3
                    and not any(row.get("received") for row in target_attempts)
                )

            record(
                "restart_retains_revocation_floor",
                wait_until(verified_restart_after_revocation, 15),
            )

            before_stop = len(samples())
            action("stop")
            cleaned = wait_until(
                lambda: any(
                    row.get("kind") == "cleanup" and row.get("tun_closed")
                    for row in samples()[before_stop:]
                ),
                12,
            )
            tunnel_gone = (
                subprocess.run(
                    [str(adb), "-s", SERIAL, "shell", "ip", "link", "show", "tun0"],
                    capture_output=True,
                ).returncode
                != 0
            )
            record("native_stop_closes_tun", cleaned and tunnel_gone)
            late_after_all_checks = [
                row
                for row in echoes()
                if row.get("peer") == "10.201.0.3"
                and row.get("receipt_elapsed") is not None
                and row["receipt_elapsed"] > android_deadline
            ]
            report["late_target_receipts"] = len(late_after_all_checks)
            if late_after_all_checks:
                for gate in report["gates"]:
                    if (
                        gate["name"]
                        == "signed_revocation_stops_target_within_two_seconds"
                    ):
                        gate["status"] = "fail"
            report["peer"] = json.loads((results / "peer.json").read_text())
            record(
                "linux_peer_observed_packets",
                report["peer"]["received_echo_packets"] >= 3,
            )
        report["samples"] = samples()
        device("shell", "am", "force-stop", PACKAGE)
        device("emu", "kill")
        emulator.wait(timeout=15)
        emulator = None
        log.close()
    except Exception as error:
        if sample_reader is not None:
            try:
                report["samples"] = sample_reader()
            except (OSError, subprocess.SubprocessError):
                pass
        if sample_reader is not None:
            report["android_network_diagnostics"] = {}
            for name, command in {
                "tun_counters": ("ip", "-s", "link", "show", "tun0"),
                "tun_addresses": ("ip", "address", "show", "tun0"),
                "routes": ("ip", "route", "show", "table", "all"),
                "rules": ("ip", "rule", "show"),
                "vpn_management": ("dumpsys", "vpn_management"),
            }.items():
                try:
                    report["android_network_diagnostics"][name] = device(
                        "shell", *command
                    )[:32768]
                except (OSError, subprocess.SubprocessError) as diagnostic_error:
                    report["android_network_diagnostics"][name] = {
                        "error_type": type(diagnostic_error).__name__
                    }
        if container:
            diagnostic_script = r"""
import json,os,subprocess
from pathlib import Path
result={}
for name,prefix in [('relay',[]),('target',['ip','netns','exec','target'])]:
    environment=os.environ.copy()
    environment['AVIAN_RPC_TOKEN_FILE']='/state/'+name+'/rpc-token'
    for operation in ('peer','route'):
        try:
            process=subprocess.run(prefix+['/engine/easytier-cli','--rpc-portal','127.0.0.1:15888','-o','json',operation],
                env=environment,capture_output=True,text=True,timeout=3)
            result[name+'_'+operation]={'status':process.returncode,'output':process.stdout[:32768]}
        except Exception as failure:
            result[name+'_'+operation]={'error_type':type(failure).__name__}
    process=subprocess.run(prefix+['ss','-tnp'],capture_output=True,text=True,timeout=3)
    result[name+'_tcp']=process.stdout[:8192]
for path in Path('/state').glob('*.log'):
    lines=[]
    for line in path.read_text(errors='replace').splitlines()[-100:]:
        if not any(word in line.lower() for word in ('private','credential','token','secret')):
            lines.append(line)
    result[path.name]='\n'.join(lines)[-8192:]
print(json.dumps(result))
"""
            try:
                report["linux_diagnostics"] = json.loads(
                    execute(
                        [
                            "docker",
                            "exec",
                            container,
                            "python3",
                            "-c",
                            diagnostic_script,
                        ],
                        timeout=20,
                    )
                )
            except (
                OSError,
                ValueError,
                subprocess.SubprocessError,
            ) as diagnostic_error:
                report["linux_diagnostics"] = {
                    "error_type": type(diagnostic_error).__name__
                }
        report["error"] = {"type": type(error).__name__, "description": str(error)}
        if isinstance(error, subprocess.CalledProcessError):
            standard_error = error.stderr or ""
            if isinstance(standard_error, bytes):
                standard_error = standard_error.decode(errors="replace")
            report["error"]["stderr"] = standard_error[-4096:]
        if emulator is not None:
            report["emulator_exit_code_before_cleanup"] = emulator.poll()
        if temporary_fixture is not None:
            emulator_log = Path(temporary_fixture.name) / "emulator.log"
            if emulator_log.exists():
                report["emulator_log_tail"] = [
                    line
                    for line in emulator_log.read_text(errors="replace").splitlines()[
                        -50:
                    ]
                    if not any(
                        word in line.lower()
                        for word in ("token", "credential", "private", "secret")
                    )
                ]

        if arguments.inspection_seconds and emulator is not None:
            report["inspection"] = {
                "serial": SERIAL,
                "container": container,
                "duration_seconds": arguments.inspection_seconds,
            }
            arguments.output.parent.mkdir(parents=True, exist_ok=True)
            arguments.output.write_text(json.dumps(report, indent=2) + "\n")
            deadline = time.monotonic() + arguments.inspection_seconds
            while time.monotonic() < deadline:
                time.sleep(min(0.2, max(0, deadline - time.monotonic())))

    finally:
        if emulator is not None:
            try:
                device("emu", "kill")
            except Exception:
                emulator.terminate()
            try:
                emulator.wait(timeout=10)
            except subprocess.TimeoutExpired:
                emulator.kill()
                emulator.wait()
        if ingress is not None:
            ingress.close()
        if container:
            subprocess.run(["docker", "rm", "-f", container], capture_output=True)
        if network:
            subprocess.run(["docker", "network", "rm", network], capture_output=True)
        if temporary_fixture is not None:
            temporary_fixture.cleanup()
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(json.dumps(report, indent=2) + "\n")
    expected = 5 if arguments.keystore_only else 22
    complete = (
        "error" not in report
        and len(report["gates"]) == expected
        and all(gate["status"] == "pass" for gate in report["gates"])
    )
    print(json.dumps({"passed": complete, "report": str(arguments.output)}))
    return 0 if complete else 1


if __name__ == "__main__":
    raise SystemExit(main())
