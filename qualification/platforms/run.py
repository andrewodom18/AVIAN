"""Run native loopback checks and optional isolated Android API probes."""

import argparse
import base64
import hashlib
import json
from pathlib import Path
import platform
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import zipfile

DIRECTORY = Path(__file__).resolve().parent
REPOSITORY = DIRECTORY.parents[1]
PACKAGE = "org.bespin.avian.probe"


def execute(arguments, **options):
    return subprocess.run(
        [str(value) for value in arguments],
        check=True,
        capture_output=True,
        text=True,
        **options,
    ).stdout.strip()


def wait_until(predicate, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.2)
    return False


def binary_hash(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def loopback_checks(binary_directory):
    cli = binary_directory / "avianctl"
    daemon = binary_directory / "aviand"
    report = {
        "scope": "native CLI and authenticated loopback API; no host interface mutation",
        "gates": [],
    }
    report["binary_sha256"] = {
        name: binary_hash(binary_directory / name) for name in ["avianctl", "aviand"]
    }
    report["version"] = execute([cli, "--version"])
    with tempfile.TemporaryDirectory(prefix="avian-platform-") as temporary:
        state = Path(temporary) / "state"
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        execute(
            [
                cli,
                "init",
                "--state-dir",
                state,
                "--network",
                "platform-test",
                "--listen",
                f"127.0.0.1:{port}",
            ]
        )
        process = subprocess.Popen(
            [str(daemon), "--config", str(state / "avian.toml")],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        token = (state / "api-token").read_text().strip()
        address = f"http://127.0.0.1:{port}"
        headers = {"Authorization": "Bearer " + token}

        def ready():
            try:
                with urllib.request.urlopen(
                    urllib.request.Request(address + "/v1/status", headers=headers),
                    timeout=1,
                ) as response:
                    return json.load(response).get("network_control_enabled") is False
            except OSError:
                return False

        try:
            report["gates"].append(
                {
                    "name": "native_daemon_readiness",
                    "status": "pass" if wait_until(ready, 10) else "fail",
                }
            )
            for name, request, expected in [
                (
                    "api_authentication",
                    urllib.request.Request(address + "/v1/status"),
                    401,
                ),
                (
                    "browser_origin_rejected",
                    urllib.request.Request(
                        address + "/v1/status",
                        headers={**headers, "Origin": "https://example.invalid"},
                    ),
                    403,
                ),
                (
                    "default_network_control_interlock",
                    urllib.request.Request(
                        address + "/v1/network-control",
                        headers={**headers, "Content-Type": "application/json"},
                        data=b'{"action":"start"}',
                        method="POST",
                    ),
                    409,
                ),
            ]:
                try:
                    with urllib.request.urlopen(request, timeout=2) as response:
                        code = response.status
                except urllib.error.HTTPError as error:
                    code = error.code
                report["gates"].append(
                    {
                        "name": name,
                        "status": "pass" if code == expected else "fail",
                        "http_status": code,
                    }
                )
            permissions = (state / "api-token").stat().st_mode & 0o777
            report["gates"].append(
                {
                    "name": "private_credential_permissions",
                    "status": "pass" if permissions == 0o600 else "fail",
                }
            )
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
    report["native_tun_and_radio"] = {
        "status": "not_run",
        "reason": "Host network mutation is excluded; full engine tests require an isolated platform VM or device.",
    }
    return report


def windows_inventory(host):
    command = "$ProgressPreference='SilentlyContinue'; $result=@{architecture=$env:PROCESSOR_ARCHITECTURE; rust_available=[bool](Get-Command cargo -ErrorAction SilentlyContinue); git_available=[bool](Get-Command git -ErrorAction SilentlyContinue); docker_available=[bool](Get-Command docker -ErrorAction SilentlyContinue)}; $result | ConvertTo-Json -Compress"
    encoded = base64.b64encode(command.encode("utf-16-le")).decode()
    result = execute(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            host,
            "powershell",
            "-NoProfile",
            "-EncodedCommand",
            encoded,
        ],
        timeout=20,
    )
    return {
        "status": "not_run",
        "inventory": json.loads(result),
        "reason": "Read-only inventory; native protected storage and isolated Windows engine execution are not qualified.",
    }


def build_android(sdk, directory):
    tools = sdk / "build-tools" / "36.0.0"
    platform_jar = sdk / "platforms" / "android-36" / "android.jar"
    classes = directory / "classes"
    dex = directory / "dex"
    classes.mkdir()
    dex.mkdir()
    sources = sorted((DIRECTORY / "android" / "src").rglob("*.java"))
    execute(
        [
            "javac",
            "-source",
            "8",
            "-target",
            "8",
            "-bootclasspath",
            platform_jar,
            "-d",
            classes,
            *sources,
        ]
    )
    execute(
        [
            tools / "d8",
            "--lib",
            platform_jar,
            "--min-api",
            "29",
            "--output",
            dex,
            *classes.rglob("*.class"),
        ]
    )
    unsigned = directory / "unsigned.apk"
    execute(
        [
            tools / "aapt2",
            "link",
            "-I",
            platform_jar,
            "--manifest",
            DIRECTORY / "android" / "AndroidManifest.xml",
            "-o",
            unsigned,
        ]
    )
    with zipfile.ZipFile(unsigned, "a") as archive:
        archive.write(dex / "classes.dex", "classes.dex")
    aligned = directory / "aligned.apk"
    execute([tools / "zipalign", "-f", "4", unsigned, aligned])
    keystore = directory / "probe.keystore"
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
            "probe",
            "-dname",
            "CN=Isolated test",
            "-keyalg",
            "RSA",
            "-validity",
            "1",
        ]
    )
    signed = directory / "probe.apk"
    execute(
        [
            tools / "apksigner",
            "sign",
            "--ks",
            keystore,
            "--ks-pass",
            "pass:android",
            "--out",
            signed,
            aligned,
        ]
    )
    return signed


def android_checks(sdk, avd):
    report = {
        "scope": "headless emulator VPN and network socket APIs; not engine forwarding or physical multi-radio testing",
        "gates": [],
    }
    serial = "emulator-5580"
    adb = sdk / "platform-tools" / "adb"

    def device(*arguments):
        return execute([adb, "-s", serial, *arguments], timeout=10)

    if serial in execute([adb, "devices"]):
        raise RuntimeError("Dedicated emulator port is already in use")
    with tempfile.TemporaryDirectory(prefix="avian-android-") as temporary:
        directory = Path(temporary)
        apk = build_android(sdk, directory)
        report["apk_sha256"] = binary_hash(apk)
        emulator_log = (directory / "emulator.log").open("w")
        emulator = subprocess.Popen(
            [
                str(sdk / "emulator" / "emulator"),
                "-avd",
                avd,
                "-port",
                "5580",
                "-read-only",
                "-no-window",
                "-no-audio",
                "-no-snapshot-load",
                "-no-snapshot-save",
                "-gpu",
                "swiftshader_indirect",
            ],
            stdout=emulator_log,
            stderr=subprocess.STDOUT,
        )
        echo = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        echo.bind(("127.0.0.1", 0))
        echo.settimeout(0.2)
        stop = threading.Event()

        def serve_echo():
            while not stop.is_set():
                try:
                    payload, address = echo.recvfrom(2048)
                    echo.sendto(payload, address)
                except socket.timeout:
                    continue
                except OSError:
                    return

        server = threading.Thread(target=serve_echo, daemon=True)
        server.start()

        def booted():
            if emulator.poll() is not None:
                raise RuntimeError("Headless emulator exited before readiness")
            try:
                return device("shell", "getprop", "sys.boot_completed") == "1"
            except subprocess.SubprocessError:
                return False

        try:
            if not wait_until(booted, 120):
                raise RuntimeError("Emulator boot timed out")
            device("install", "-r", str(apk))
            device("shell", "appops", "set", PACKAGE, "ACTIVATE_VPN", "allow")
            device("logcat", "-c")
            device(
                "shell",
                "am",
                "start",
                "-n",
                PACKAGE + "/.ProbeActivity",
                "--ei",
                "port",
                str(echo.getsockname()[1]),
            )

            def samples():
                result = []
                for line in device(
                    "logcat", "-d", "-s", "AVIAN_PLATFORM:I", "*:S"
                ).splitlines():
                    start = line.find("{")
                    if start >= 0:
                        result.append(json.loads(line[start:]))
                return result

            if not wait_until(lambda: len(samples()) >= 2, 20):
                report["probe_log"] = device(
                    "logcat", "-d", "-s", "AVIAN_PLATFORM:I", "*:S"
                )
                raise RuntimeError("Android probe did not produce samples")
            before = len(samples())
            device("shell", "input", "keyevent", "KEYCODE_SLEEP")
            power_state = device("shell", "dumpsys", "power")
            if not any(
                marker in power_state
                for marker in ["mWakefulness=Asleep", "mWakefulness=Dozing"]
            ):
                raise RuntimeError("Emulator did not enter screen-off power state")
            screen_off_started = time.monotonic()
            if not wait_until(lambda: len(samples()) >= before + 3, 12):
                raise RuntimeError("Background Android probe stopped producing samples")
            observed = samples()
            report["samples"] = observed
            for field in [
                "tun_established",
                "protected_socket",
                "network_bound",
                "echo_received",
            ]:
                report["gates"].append(
                    {
                        "name": field,
                        "status": (
                            "pass"
                            if all(sample[field] for sample in observed)
                            else "fail"
                        ),
                    }
                )
            report["gates"].append(
                {
                    "name": "screen_off_foreground_service",
                    "status": "pass",
                    "continued_samples": len(observed) - before,
                    "observed_seconds": time.monotonic() - screen_off_started,
                }
            )
            device("shell", "am", "force-stop", PACKAGE)
            stopped_count = len(samples())
            time.sleep(2)
            tunnel_gone = (
                subprocess.run(
                    [str(adb), "-s", serial, "shell", "ip", "link", "show", "tun0"],
                    capture_output=True,
                ).returncode
                != 0
            )
            report["gates"].append(
                {
                    "name": "service_cleanup",
                    "status": (
                        "pass"
                        if len(samples()) == stopped_count and tunnel_gone
                        else "fail"
                    ),
                }
            )
            device("shell", "input", "keyevent", "KEYCODE_WAKEUP")
            device(
                "shell",
                "am",
                "start",
                "-n",
                PACKAGE + "/.ProbeActivity",
                "--ei",
                "port",
                str(echo.getsockname()[1]),
            )
            restarted = wait_until(lambda: len(samples()) > stopped_count, 10)
            report["gates"].append(
                {"name": "service_restart", "status": "pass" if restarted else "fail"}
            )
            device("shell", "am", "force-stop", PACKAGE)
        except Exception as error:
            report["runner_error"] = {
                "type": type(error).__name__,
                "description": str(error),
            }
            try:
                report["probe_log"] = device(
                    "logcat", "-d", "-s", "AVIAN_PLATFORM:I", "*:S"
                )
            except (OSError, subprocess.SubprocessError):
                pass
        finally:
            stop.set()
            echo.close()
            try:
                device("emu", "kill")
            except (OSError, subprocess.SubprocessError):
                emulator.terminate()
            try:
                emulator.wait(timeout=10)
            except subprocess.TimeoutExpired:
                emulator.kill()
                emulator.wait()
            emulator_log.close()
    report["physical_wifi_cellular_binding"] = {
        "status": "not_run",
        "reason": "No physical Android device is attached; emulator networks do not qualify simultaneous physical Wi-Fi/cellular operation.",
    }
    report["engine_forwarding"] = {
        "status": "not_run",
        "reason": "The probe exercises platform APIs without embedding the AVIAN engine.",
    }
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary-directory", type=Path, default=REPOSITORY / "target" / "debug"
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--windows-host")
    parser.add_argument("--android-avd")
    parser.add_argument("--cross-check-windows", action="store_true")
    parser.add_argument(
        "--android-sdk", type=Path, default=Path.home() / "Library" / "Android" / "sdk"
    )
    arguments = parser.parse_args()
    report = {
        "host": {"system": platform.system(), "architecture": platform.machine()},
        "qualified_for_cutover": False,
    }
    try:
        report["native"] = loopback_checks(arguments.binary_directory.resolve())
        if arguments.cross_check_windows:
            command = [
                "cargo",
                "check",
                "-p",
                "avian-membership",
                "-p",
                "avian-engine",
                "-p",
                "avian-routing",
                "--target",
                "x86_64-pc-windows-gnu",
                "--locked",
            ]
            checked = subprocess.run(
                command, cwd=REPOSITORY, capture_output=True, text=True
            )
            report["windows_cross_check"] = {
                "status": "pass" if checked.returncode == 0 else "fail",
                "target": "x86_64-pc-windows-gnu",
                "scope": "compiler check only; no native execution or credential-store qualification",
            }
        if arguments.windows_host:
            report["windows"] = windows_inventory(arguments.windows_host)
        if arguments.android_avd:
            report["android"] = android_checks(
                arguments.android_sdk.resolve(), arguments.android_avd
            )
    except Exception as error:
        report["runner_error"] = {
            "type": type(error).__name__,
            "description": str(error),
        }
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    groups = [report.get("native", {}), report.get("android", {})]
    cross_check_failed = report.get("windows_cross_check", {}).get("status") == "fail"
    return (
        1
        if "runner_error" in report
        or cross_check_failed
        or any("runner_error" in group for group in groups)
        or any(
            gate["status"] != "pass"
            for group in groups
            for gate in group.get("gates", [])
        )
        else 0
    )


if __name__ == "__main__":
    raise SystemExit(main())
