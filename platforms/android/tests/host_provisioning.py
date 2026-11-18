"""Check offline provisioning in a debug ARC APK on a disposable Android emulator."""

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile
import zipfile

DIRECTORY = Path(__file__).resolve().parent
HOST_PACKAGE = "ai.rownd.arc.debug"
CHECK_PACKAGE = "org.arc.qualification"


def execute(arguments, **options):
    result = subprocess.run(
        [str(argument) for argument in arguments],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=180,
        **options,
    )
    if result.returncode:
        raise RuntimeError((result.stdout + result.stderr).decode(errors="replace")[-8192:])
    return result.stdout.decode()


def build_checks(directory, sdk, keystore):
    tools = sdk / "build-tools/36.0.0"
    android_jar = sdk / "platforms/android-36/android.jar"
    classes = directory / "classes"
    dex = directory / "dex"
    classes.mkdir()
    dex.mkdir()
    javac = Path(os.environ["JAVA_HOME"]) / "bin/javac" if "JAVA_HOME" in os.environ else "javac"
    execute([
        javac, "-source", "8", "-target", "8", "-bootclasspath", android_jar,
        "-d", classes, DIRECTORY / "java/org/arc/qualification/ProvisioningCheck.java",
    ])
    execute([
        tools / "d8", "--lib", android_jar, "--min-api", "29", "--output", dex,
        *classes.rglob("*.class"),
    ])
    unsigned = directory / "unsigned.apk"
    execute([
        tools / "aapt2", "link", "-I", android_jar,
        "--manifest", DIRECTORY / "HostProvisioningManifest.xml", "-o", unsigned,
    ])
    with zipfile.ZipFile(unsigned, "a") as archive:
        archive.write(dex / "classes.dex", "classes.dex")
    aligned = directory / "aligned.apk"
    execute([tools / "zipalign", "-f", "4", unsigned, aligned])
    apk = directory / "checks.apk"
    execute([
        tools / "apksigner", "sign", "--ks", keystore, "--ks-pass", "pass:android",
        "--ks-key-alias", "androiddebugkey", "--out", apk, aligned,
    ])
    return apk


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host-apk", type=Path, required=True)
    parser.add_argument("--avianctl", type=Path, required=True)
    parser.add_argument("--serial", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--android-sdk", type=Path, default=Path.home() / "Library/Android/sdk")
    parser.add_argument("--keystore", type=Path, default=Path.home() / ".android/debug.keystore")
    parser.add_argument("--keep-host", action="store_true")
    parser.add_argument("--runtime", action="store_true", help="Also run the embedded mesh and native fleet bridge")
    arguments = parser.parse_args()
    if re.fullmatch(r"emulator-\d+", arguments.serial) is None:
        parser.error("Use a disposable emulator; physical devices are not accepted")
    adb = arguments.android_sdk / "platform-tools/adb"
    command = [adb, "-s", arguments.serial]
    if execute([*command, "shell", "getprop", "ro.kernel.qemu"]).strip() != "1":
        parser.error("The selected target is not an Android emulator")
    report = {
        "qualified_for_cutover": False,
        "checks": {},
        "android_api_level": int(execute([*command, "shell", "getprop", "ro.build.version.sdk"]).strip()),
        "emulator_serial": arguments.serial,
        "host_apk_sha256": hashlib.sha256(arguments.host_apk.read_bytes()).hexdigest(),
        "vpn_consent_fixture": "emulator ACTIVATE_VPN app-op" if arguments.runtime else "not requested",
        "runtime_permissions_fixture": "default platform prompts",
    }
    with zipfile.ZipFile(arguments.host_apk) as archive:
        libraries = ["libavian_android.so", "libeasytier_ffi.so", "libavian_storage_android.so"]
        if arguments.runtime:
            libraries.append("libdev_bridge.so")
        report["native_library_sha256"] = {
            library: hashlib.sha256(archive.read(f"lib/arm64-v8a/{library}")).hexdigest()
            for library in libraries
        }

    def instrument(action, *extra):
        output = execute([
            *command, "shell", "am", "instrument", "-w", "-e", "action", action,
            *extra, f"{CHECK_PACKAGE}/org.arc.qualification.ProvisioningCheck",
        ])
        if "INSTRUMENTATION_CODE: -1" not in output or "INSTRUMENTATION_RESULT: failure=" in output:
            raise RuntimeError(output)
        return {
            line.removeprefix("INSTRUMENTATION_RESULT: ").split("=", 1)[0]:
            line.split("=", 1)[1]
            for line in output.splitlines()
            if line.startswith("INSTRUMENTATION_RESULT: ") and "=" in line
        }

    try:
        with tempfile.TemporaryDirectory(prefix="avian-host-provisioning-") as temporary:
            directory = Path(temporary)
            checks_apk = build_checks(directory, arguments.android_sdk, arguments.keystore)
            execute([*command, "install", "--no-streaming", "-r", arguments.host_apk])
            # This runner only accepts a disposable emulator and clears its debug wrapper state.
            execute([*command, "shell", "pm", "clear", HOST_PACKAGE])
            execute([*command, "install", "--no-streaming", "-r", checks_apk])
            report["checks"].update(instrument("request"))
            request = directory / "request.json"
            request.write_text(execute([*command, "exec-out", "run-as", HOST_PACKAGE, "cat", "files/enrollment-request.json"]))
            issuer = directory / "issuer"
            initialized = json.loads(execute([arguments.avianctl, "init", "--state-dir", issuer, "--network", "host-provisioning"]))
            bundle = directory / "bundle.json"
            execute([
                arguments.avianctl, "--config", issuer / "avian.toml", "provision",
                "--request", request, "--address", "10.88.0.2", "--output", bundle,
            ])
            remote_bundle = "/data/local/tmp/avian-host-enrollment.json"
            try:
                execute([*command, "push", bundle, remote_bundle])
                execute([*command, "shell", "run-as", HOST_PACKAGE, "cp", remote_bundle, "files/enrollment-bundle.json"])
                report["checks"].update(instrument("import", "-e", "fingerprint", initialized["administrator_fingerprint"]))
                if arguments.runtime:
                    execute([*command, "shell", "appops", "set", HOST_PACKAGE, "ACTIVATE_VPN", "allow"])
                    report["checks"].update(instrument("runtime"))
            finally:
                execute([*command, "shell", "rm", "-f", remote_bundle])
    finally:
        subprocess.run([str(value) for value in [*command, "uninstall", CHECK_PACKAGE]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
        if not arguments.keep_host:
            subprocess.run([str(value) for value in [*command, "uninstall", HOST_PACKAGE]], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30)
        arguments.output.parent.mkdir(parents=True, exist_ok=True)
        arguments.output.write_text(json.dumps(report, indent=2) + "\n")
    print(arguments.output)


if __name__ == "__main__":
    main()
