"""Build the AVIAN Android library from the pinned engine and platform adapters."""

import argparse
import importlib.util
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

DIRECTORY = Path(__file__).resolve().parent
REPOSITORY = DIRECTORY.parents[1]
PATCH_NAMES = [
    "verified-membership",
    "authenticated-forwarding",
    "traffic-policy",
    "live-revocation",
    "failover",
    "topology-policy",
    "congestion",
    "measured-routing",
    "underlay-binding",
    "private-rpc",
    "managed-network",
]


def execute(arguments, **options):
    subprocess.run([str(value) for value in arguments], check=True, **options)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def copy_planner(source):
    planner = source / "avian-routing"
    planner.mkdir()
    shutil.copytree(REPOSITORY / "crates/avian-routing/src", planner / "src")
    shutil.copyfile(REPOSITORY / "LICENSE.md", planner / "LICENSE")
    version = tomllib.loads((REPOSITORY / "Cargo.toml").read_text())["workspace"][
        "package"
    ]["version"]
    (planner / "Cargo.toml").write_text(f"""[package]
name = "avian-routing"
version = "{version}"
edition = "2024"
license-file = "LICENSE"
[dependencies]
serde = {{ version = "1", features = ["derive"] }}
thiserror = "2"
""")


def prepare_source(checkout, source, lock):
    actual = subprocess.check_output(
        ["git", "-C", str(checkout), "rev-parse", lock["source_commit"]], text=True
    ).strip()
    if actual != lock["source_commit"]:
        raise RuntimeError("Engine source revision does not match the pin")
    source.mkdir()
    with tempfile.TemporaryFile() as archive:
        execute(["git", "-C", checkout, "archive", actual], stdout=archive)
        archive.seek(0)
        with tarfile.open(fileobj=archive) as contents:
            contents.extractall(source, filter="data")
    # Anchor git apply here; otherwise a checkout's parent repository can silently skip paths.
    execute(["git", "init", "-q", source])
    patches = [
        REPOSITORY / "qualification/patches" / (name + ".patch") for name in PATCH_NAMES
    ]
    patches.append(DIRECTORY / "android-sockets.patch")
    for patch in patches:
        execute(["git", "apply", "--check", patch], cwd=source)
        execute(["git", "apply", patch], cwd=source)
    copy_planner(source)
    specification = importlib.util.spec_from_file_location(
        "engine_builder", REPOSITORY / "qualification/build.py"
    )
    builder = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(builder)
    builder.prepare_shared_network_helper(source / "avian-network-helper")
    # The app library already depends on this workspace planner. Use one package identity.
    engine_manifest = source / "easytier/Cargo.toml"
    engine_manifest.write_text(
        engine_manifest.read_text().replace(
            'avian-routing = { path = "../avian-routing" }',
            "avian-routing = { path = "
            + json.dumps(str(REPOSITORY / "crates/avian-routing"))
            + " }",
        )
    )
    engine_manifest.write_text(
        engine_manifest.read_text().replace(
            'avian-network-helper = { path = "../avian-network-helper", default-features = false }',
            "avian-network-helper = { path = "
            + json.dumps(str(REPOSITORY / "crates/avian-network-helper"))
            + ", default-features = false }",
        )
    )
    ffi = source / "easytier-contrib/easytier-ffi"
    shutil.copyfile(
        DIRECTORY / "native/verified_host.rs", ffi / "src/avian_verified_host.rs"
    )
    (ffi / "src/avian_verified_host").mkdir()
    shutil.copyfile(
        DIRECTORY / "native/overlay.rs", ffi / "src/avian_verified_host/overlay.rs"
    )
    shutil.copyfile(
        DIRECTORY / "native/underlay.rs", ffi / "src/avian_verified_host/underlay.rs"
    )
    shutil.copyfile(
        DIRECTORY / "native/probes.rs", ffi / "src/avian_verified_host/probes.rs"
    )
    manifest = ffi / "Cargo.toml"
    text = manifest.read_text().replace(
        'easytier = { path = "../../easytier" }',
        'easytier = { path = "../../easytier", default-features = false, features = ["tun", "aes-gcm"] }',
    )
    for name in (
        "avian-membership",
        "avian-engine",
        "avian-storage",
        "avian-services",
        "avian-revocations",
        "avian-runtime",
    ):
        text += (
            f'\n{name} = {{ path = {json.dumps(str(REPOSITORY / "crates" / name))} }}\n'
        )
    text += "\navian = { path = " + json.dumps(str(REPOSITORY / "apps/avian")) + " }\n"
    text += '\naxum = { version = "0.8", features = ["json"] }\ntokio = { version = "1", features = ["full"] }\nreqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }\ned25519-dalek = "2.2"\n'
    text += '\nanyhow = "1"\nbase64 = "0.22"\nhex = "0.4"\nipnet = "2"\ntoml = "0.8"\nurl = "2"\nx25519-dalek = { version = "2", features = ["static_secrets"] }\nzeroize = "1"\n'
    manifest.write_text(text)
    return [{"name": patch.name, "sha256": digest(patch)} for patch in patches]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, default=DIRECTORY / ".local/build")
    parser.add_argument(
        "--android-sdk", type=Path, default=Path.home() / "Library/Android/sdk"
    )
    parser.add_argument("--ndk-version", default="28.2.13676358")
    parser.add_argument("--protoc", type=Path, required=True)
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--development", action="store_true")
    arguments = parser.parse_args()
    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    (output / "build.json").write_text(
        json.dumps({"engine_build": "not_run", "qualified_for_cutover": False}) + "\n"
    )
    source = output / "source"
    if source.exists():
        shutil.rmtree(source)
    lock = json.loads((REPOSITORY / "qualification/engine.lock.json").read_text())
    patches = prepare_source(arguments.source.resolve(), source, lock)
    ndk = arguments.android_sdk / "ndk" / arguments.ndk_version
    toolchain = ndk / "toolchains/llvm/prebuilt/darwin-x86_64"
    compiler = toolchain / "bin/aarch64-linux-android29-clang++"
    provenance = {
        "source_commit": lock["source_commit"],
        "patches": patches,
        "shared_network_helper_sha256": {
            name: digest(source / "avian-network-helper" / name)
            for name in [
                "Cargo.toml",
                "src/lib.rs",
                "src/client.rs",
                "src/transport.rs",
                "LICENSE",
            ]
        },
        "target": "aarch64-linux-android",
        "android_api": 29,
        "ndk_version": arguments.ndk_version,
        "ndk_notice_sha256": digest(ndk / "NOTICE.toolchain"),
        "avian_workspace_sha256": digest(REPOSITORY / "Cargo.toml"),
        "java_sources": {
            str(path.relative_to(DIRECTORY)): digest(path)
            for path in sorted((DIRECTORY / "java").rglob("*.java"))
        },
        "manifest_sha256": digest(DIRECTORY / "AndroidManifest.xml"),
        "native_sources": {
            str(path.relative_to(DIRECTORY)): digest(path)
            for path in sorted((DIRECTORY / "native").glob("*"))
        },
        "verification_sources": {
            str(path.relative_to(REPOSITORY)): digest(path)
            for name in (
                "avian-storage",
                "avian-membership",
                "avian-engine",
                "avian-routing",
                "avian-services",
                "avian-revocations",
                "avian-runtime",
                "avian-connectivity",
            )
            for path in sorted((REPOSITORY / "crates" / name).rglob("*"))
            if path.is_file() and (path.suffix == ".rs" or path.name == "Cargo.toml")
        },
        "app_library_sources": {
            str(path.relative_to(REPOSITORY)): digest(path)
            for path in sorted((REPOSITORY / "apps/avian").rglob("*"))
            if path.is_file() and (path.suffix == ".rs" or path.name == "Cargo.toml")
        },
        "development_only": arguments.development,
        "qualified_for_cutover": False,
        "engine_build": "not_run",
        "physical_multi_network": "not_run",
    }
    provenance["verification_sources"].update(
        {
            str(path.relative_to(REPOSITORY)): digest(path)
            for name in [
                "Cargo.toml",
                "src/lib.rs",
                "src/client.rs",
                "src/transport.rs",
            ]
            for path in [REPOSITORY / "crates/avian-network-helper" / name]
        }
    )
    (output / "build.json").write_text(json.dumps(provenance, indent=2) + "\n")
    if arguments.prepare_only:
        return
    environment = os.environ.copy()
    environment.update(
        {
            "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER": str(
                toolchain / "bin/aarch64-linux-android29-clang"
            ),
            "CC_aarch64_linux_android": str(
                toolchain / "bin/aarch64-linux-android29-clang"
            ),
            "CXX_aarch64_linux_android": str(compiler),
            "AR_aarch64_linux_android": str(toolchain / "bin/llvm-ar"),
            "CARGO_TARGET_DIR": str(output / "cargo"),
            "CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS": "-C link-arg=-Wl,-z,max-page-size=16384",
            "CARGO_PROFILE_DEV_DEBUG": "0",
            "CARGO_BUILD_JOBS": "4",
            "PROTOC": str(arguments.protoc.resolve()),
        }
    )
    execute(
        ["cargo", "build", "--target", "aarch64-linux-android", "-p", "easytier-ffi"],
        cwd=source,
        env=environment,
    )
    jni = output / "aar/jni/arm64-v8a"
    jni.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(
        output / "cargo/aarch64-linux-android/debug/libeasytier_ffi.so",
        jni / "libeasytier_ffi.so",
    )
    execute(
        [
            compiler,
            "-std=c++17",
            "-shared",
            "-fPIC",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wl,--no-undefined",
            "-Wl,-z,max-page-size=16384",
            "-Wl,-z,relro,-z,now",
            "-Wl,-soname,libavian_android.so",
            DIRECTORY / "native/bridge.cpp",
            "-L" + str(jni),
            "-leasytier_ffi",
            "-o",
            jni / "libavian_android.so",
        ]
    )
    execute(
        [
            compiler,
            "-std=c++17",
            "-shared",
            "-fPIC",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-nostdlib++",
            "-fno-exceptions",
            "-fno-rtti",
            "-Wl,--no-undefined",
            "-Wl,-z,max-page-size=16384",
            "-Wl,-z,relro,-z,now",
            "-Wl,-soname,libavian_storage_android.so",
            DIRECTORY / "native/storage.cpp",
            "-o",
            jni / "libavian_storage_android.so",
        ]
    )
    shutil.copyfile(
        toolchain / "sysroot/usr/lib/aarch64-linux-android/libc++_shared.so",
        jni / "libc++_shared.so",
    )
    classes = output / "classes"
    classes.mkdir(exist_ok=True)
    execute(
        [
            "javac",
            "-source",
            "8",
            "-target",
            "8",
            "-bootclasspath",
            arguments.android_sdk / "platforms/android-36/android.jar",
            "-d",
            classes,
            *sorted((DIRECTORY / "java").rglob("*.java")),
        ]
    )
    execute(["jar", "cf", output / "aar/classes.jar", "-C", classes, "."])
    shutil.copyfile(
        DIRECTORY / "AndroidManifest.xml", output / "aar/AndroidManifest.xml"
    )
    shutil.copyfile(REPOSITORY / "LICENSE.md", output / "aar/AVIAN-LICENSE.md")
    shutil.copyfile(source / "LICENSE", output / "aar/EasyTier-LICENSE")
    shutil.copyfile(ndk / "NOTICE.toolchain", output / "aar/NDK-NOTICE")
    archive = output / "avian-android.aar"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as aar:
        for path in sorted((output / "aar").rglob("*")):
            if path.is_file():
                aar.write(path, str(path.relative_to(output / "aar")))
    provenance["engine_build"] = "pass"
    provenance["rustc"] = subprocess.check_output(
        ["rustc", "--version"], cwd=source, text=True
    ).strip()
    provenance["binary_sha256"] = {
        path.name: digest(path) for path in sorted(jni.glob("*.so"))
    }
    provenance["aar_sha256"] = digest(archive)
    provenance["cargo_lock_sha256"] = digest(source / "Cargo.lock")
    shutil.copyfile(source / "Cargo.lock", output / "Cargo.lock")
    (output / "build.json").write_text(json.dumps(provenance, indent=2) + "\n")
    print(archive)


if __name__ == "__main__":
    main()
