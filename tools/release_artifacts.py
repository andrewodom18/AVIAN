"""Build deterministic payload archives and verify native release evidence before publication."""

import argparse
import contextlib
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import re
import stat
import tarfile
import unicodedata

SCHEMA_VERSION = 1
PLATFORMS = {"linux", "macos", "windows", "android"}
TARGETS = {
    "aarch64-unknown-linux-gnu": "linux",
    "x86_64-unknown-linux-gnu": "linux",
    "aarch64-apple-darwin": "macos",
    "x86_64-apple-darwin": "macos",
    "x86_64-pc-windows-msvc": "windows",
    "aarch64-pc-windows-msvc": "windows",
    "aarch64-linux-android": "android",
}
REQUIRED_CORE_GATES = {
    "native_install_upgrade_remove", "protected_storage", "native_runtime_forwarding",
    "authenticated_relay", "traffic_class_selection", "metered_policy", "failover",
    "revocation", "owned_interface_cleanup", "unprivileged_manager",
}
REQUIRED_RELEASE_GATES = {"chud_coordination", "arc_tank_connectivity"}
PLATFORM_GATES = {
    "linux": {"scoped_network_helper"},
    "macos": {"signed_native_runtime", "sleep_wake_recovery"},
    "windows": {"native_service_lifecycle", "owner_acl_and_key_protection"},
    "android": {"vpn_consent_and_revoke", "background_forwarding", "protected_network_sockets"},
}
REQUIRED_GATES = {
    platform: REQUIRED_CORE_GATES | REQUIRED_RELEASE_GATES | gates |
    ({"physical_wifi_cellular_binding"} if platform == "android" else set())
    for platform, gates in PLATFORM_GATES.items()
}
COMMON_ROLES = {"engine", "engine_manifest", "license", "installer", "service_definition"}
DESKTOP_ROLES = {"manager", "cli", "revocations", "engine_cli", "network_helper"}
MANIFEST_FIELDS = {
    "schema_version", "target", "platform", "source_commit", "version", "roles", "files",
    "engine", "protocol_sha256", "shared_planner_sha256", "privileged_manager",
}
HASH = re.compile(r"[0-9a-f]{64}")
COMMIT = re.compile(r"[0-9a-f]{40}")
VERSION = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?")


def _require(condition, reason):
    if not condition:
        raise ValueError(reason)


def _relative(value):
    _require(isinstance(value, str) and value and "\\" not in value and ":" not in value,
             "Artifact paths must be relative POSIX paths")
    path = PurePosixPath(value)
    _require(not path.is_absolute() and str(path) == value and
             all(part not in {"", ".", ".."} for part in value.split("/")) and
             not any(ord(character) < 32 or ord(character) == 127 or character in '<>"|?*' for character in value), "Unsafe artifact path")
    reserved = {"CON", "PRN", "AUX", "NUL"} | {"COM" + str(number) for number in range(1, 10)} | {"LPT" + str(number) for number in range(1, 10)}
    _require(all(part == part.rstrip(" .") and unicodedata.normalize("NFKC", part.split(".")[0].rstrip(" ")).upper() not in reserved
                 for part in path.parts), "Artifact path is unsafe on supported platforms")
    return value


def _regular(root, relative):
    relative = _relative(relative)
    root = Path(root)
    _require(not root.is_symlink() and root.is_dir(), "Artifact root must be a real directory")
    path = root
    for part in PurePosixPath(relative).parts:
        path = path / part
        _require(not path.is_symlink(), "Symbolic links are forbidden in release artifacts")
    _require(path.is_file() and stat.S_ISREG(path.stat().st_mode), "Missing regular artifact file: " + relative)
    _require(path.stat().st_nlink == 1, "Hard-linked artifact inputs are forbidden")
    return path


def _digest_stream(stream):
    digest = hashlib.sha256()
    for block in iter(lambda: stream.read(1024 * 1024), b""):
        digest.update(block)
    return digest.hexdigest()


def digest_file(path):
    with Path(path).open("rb") as stream:
        return _digest_stream(stream)


def _json(path):
    _require(Path(path).stat().st_size <= 16 * 1024 * 1024, "Artifact metadata exceeds the local verification budget")
    with Path(path).open(encoding="utf-8") as stream:
        return json.load(stream, object_pairs_hook=_unique_object)


def _unique_object(pairs):
    result = {}
    for name, value in pairs:
        _require(name not in result, "Duplicate JSON metadata key")
        result[name] = value
    return result


def _encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2, ensure_ascii=True) + "\n").encode()


def source_hashes(repository):
    repository = Path(repository)
    protocols = [repository / "qualification/engine.lock.json"] + sorted((repository / "qualification/patches").glob("*.patch"))
    android_patch = repository / "platforms/android/android-sockets.patch"
    if android_patch.is_file():
        protocols.append(android_patch)
    planners = sorted((repository / "crates/avian-routing").rglob("*.rs"))
    planners += sorted((repository / "crates/avian-connectivity").rglob("*.rs"))
    planners += sorted((repository / "crates/avian-network-helper").rglob("*.rs"))
    planners += [path for path in [repository / "Cargo.toml", repository / "Cargo.lock",
                                  repository / "crates/avian-routing/Cargo.toml",
                                  repository / "crates/avian-connectivity/Cargo.toml",
                                  repository / "crates/avian-network-helper/Cargo.toml",
                                  repository / "qualification/build.py"] if path.is_file()]
    _require(protocols and planners, "Protocol and shared planner source inputs are missing")
    return {
        "protocol_sha256": {path.relative_to(repository).as_posix(): digest_file(path) for path in protocols},
        "shared_planner_sha256": {path.relative_to(repository).as_posix(): digest_file(path) for path in planners},
    }


def _validate_manifest(manifest):
    _require(isinstance(manifest, dict) and set(manifest) == MANIFEST_FIELDS, "Unsupported artifact manifest fields")
    _require(manifest["schema_version"] == SCHEMA_VERSION, "Unsupported artifact manifest version")
    _require(TARGETS.get(manifest["target"]) == manifest["platform"], "Unsupported native artifact target")
    _require(bool(COMMIT.fullmatch(manifest["source_commit"])), "Invalid source commit")
    _require(bool(VERSION.fullmatch(manifest["version"])), "Invalid release version")
    _require(manifest["privileged_manager"] is False, "A privileged application manager cannot qualify as a release runtime")
    files = manifest["files"]
    _require(isinstance(files, dict) and files, "Artifact file inventory is missing")
    normalized = {unicodedata.normalize("NFC", name).casefold() for name in files}
    _require(len(normalized) == len(files), "Artifact paths collide on supported filesystems")
    for name, entry in files.items():
        _relative(name)
        _require(name != "artifact.json", "The manifest cannot hash itself")
        _require(isinstance(entry, dict) and set(entry) == {"sha256", "size", "mode"}, "Invalid artifact file inventory")
        _require(isinstance(entry["sha256"], str) and HASH.fullmatch(entry["sha256"]), "Invalid artifact file digest")
        _require(type(entry["size"]) is int and entry["size"] >= 0 and entry["mode"] in {0o644, 0o755}, "Invalid artifact file metadata")
    roles = manifest["roles"]
    _require(isinstance(roles, dict), "Artifact roles are missing")
    required = COMMON_ROLES | ({"application", "native_bridge"} if manifest["platform"] == "android" else DESKTOP_ROLES)
    if manifest["platform"] == "linux":
        required.update({"revocation_service", "network_helper_service", "network_helper_socket", "service_account", "release_validator"})
    missing = sorted(required - set(roles))
    _require(not missing, "Native packaging is unavailable; missing roles: " + ", ".join(missing))
    _require(all(isinstance(path, str) and path in files for path in roles.values()), "Artifact role points to a missing file")
    distinct_roles = dict(roles)
    if manifest["platform"] == "android" and roles.get("installer") == roles.get("application"):
        distinct_roles.pop("installer")
    _require(len(set(distinct_roles.values())) == len(distinct_roles), "Different native roles cannot substitute for one another")
    engine = manifest["engine"]
    _require(isinstance(engine, dict) and set(engine) == {"version", "source_commit", "patch_sha256"}, "Engine provenance is missing")
    _require(bool(COMMIT.fullmatch(engine["source_commit"])) and isinstance(engine["patch_sha256"], dict) and engine["patch_sha256"], "Invalid engine provenance")
    for group in [engine["patch_sha256"], manifest["protocol_sha256"], manifest["shared_planner_sha256"]]:
        _require(isinstance(group, dict) and group and all(isinstance(value, str) and HASH.fullmatch(value) for value in group.values()), "Missing or invalid source provenance hashes")


def _verify_source(manifest, repository):
    expected = source_hashes(repository)
    for name, hashes in expected.items():
        _require(manifest[name] == hashes, "Artifact differs from the checked-out " + name)
    _require(manifest["files"][manifest["roles"]["license"]]["sha256"] == digest_file(Path(repository) / "LICENSE.md"), "Packaged license differs from the repository license")
    lock = _json(Path(repository) / "qualification/engine.lock.json")
    _require(manifest["engine"]["source_commit"] == lock["source_commit"] and manifest["engine"]["version"] == lock["version"], "Engine differs from the pinned source")
    android_patch = "platforms/android/android-sockets.patch"
    if manifest["platform"] == "android":
        _require(android_patch in expected["protocol_sha256"], "Android native socket adapter patch is unavailable")
    patches = {PurePosixPath(path).name: digest for path, digest in expected["protocol_sha256"].items()
               if path.endswith(".patch") and (path != android_patch or manifest["platform"] == "android")}
    _require(manifest["engine"]["patch_sha256"] == patches, "Engine does not contain the complete current target-specific reviewed patch set")


def build_artifact(stage, metadata, output):
    """Package supplied native outputs; never compile, download, install, or invent them."""
    stage = Path(stage)
    output = Path(output)
    _require(not output.resolve().is_relative_to(stage.resolve()), "Archive output must be outside its input tree")
    manifest = dict(metadata)
    files = {}
    for path in sorted(stage.rglob("*")):
        relative = path.relative_to(stage).as_posix()
        _require(not path.is_symlink(), "Symbolic links are forbidden in release artifacts")
        if path.is_dir():
            continue
        regular = _regular(stage, relative)
        mode = 0o755 if regular.stat().st_mode & 0o111 else 0o644
        files[relative] = {"sha256": digest_file(regular), "size": regular.stat().st_size, "mode": mode}
    manifest["schema_version"] = SCHEMA_VERSION
    manifest["files"] = files
    _validate_manifest(manifest)
    output.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(output, "w", format=tarfile.USTAR_FORMAT) as archive:
        for name, entry in sorted(files.items()):
            content = _regular(stage, name).read_bytes()
            _require(len(content) == entry["size"] and hashlib.sha256(content).hexdigest() == entry["sha256"], "Artifact input changed during packaging")
            _add_member(archive, name, content, entry["mode"])
        _add_member(archive, "artifact.json", _encoded(manifest), 0o644)
    verify_archive(output, manifest)
    return manifest


def _add_member(archive, name, content, mode):
    member = tarfile.TarInfo(name)
    member.size = len(content)
    member.mode = mode
    member.mtime = 0
    member.uid = member.gid = 0
    member.uname = member.gname = ""
    archive.addfile(member, io.BytesIO(content))


def verify_archive(archive_path, manifest):
    """Inspect a payload without extraction, including its exact embedded manifest."""
    _validate_manifest(manifest)
    expected = manifest["files"]
    seen = set()
    member_order = []
    captured = {}
    with contextlib.ExitStack() as stack:
        stream = stack.enter_context(Path(archive_path).open("rb")) if isinstance(archive_path, (str, Path)) else archive_path
        stream.seek(0)
        archive_digest = _digest_stream(stream)
        stream.seek(0)
        archive = stack.enter_context(tarfile.open(fileobj=stream, mode="r:"))
        for member in archive:
            _relative(member.name)
            _require(member.name not in seen, "Duplicate archive member")
            seen.add(member.name)
            member_order.append(member.name)
            _require(member.isfile() and not member.issym() and not member.islnk(), "Only regular archive files are permitted")
            _require(member.uid == 0 and member.gid == 0 and member.mtime == 0 and not member.pax_headers,
                     "Archive metadata is not deterministic")
            stream = archive.extractfile(member)
            _require(stream is not None, "Archive file is unreadable")
            if member.name == "artifact.json":
                _require(member.size <= 16 * 1024 * 1024 and member.mode == 0o644, "Invalid embedded manifest")
                _require(stream.read() == _encoded(manifest), "Embedded artifact manifest mismatch")
                continue
            _require(member.name in expected, "Unexpected archive file")
            entry = expected[member.name]
            _require(member.size == entry["size"] and member.mode == entry["mode"], "Archive file size or permissions mismatch")
            # Engine provenance is public metadata and has a bounded parser budget.
            if member.name == manifest["roles"]["engine_manifest"]:
                _require(member.size <= 16 * 1024 * 1024, "Engine metadata exceeds verification budget")
                content = stream.read()
                captured["engine"] = json.loads(content, object_pairs_hook=_unique_object)
                digest = hashlib.sha256(content).hexdigest()
            else:
                digest = _digest_stream(stream)
            _require(digest == entry["sha256"], "Archive content hash mismatch: " + member.name)
    _require(seen == set(expected) | {"artifact.json"}, "Archive is missing required payload files")
    _require(member_order == sorted(expected) + ["artifact.json"], "Archive member order is not canonical")
    engine = captured.get("engine", {})
    _require(engine.get("source_commit") == manifest["engine"]["source_commit"], "Packaged engine provenance differs from manifest")
    patches = engine.get("patches", [])
    _require(isinstance(patches, list) and len(patches) == len(manifest["engine"]["patch_sha256"]) and
             {entry["name"]: entry["sha256"] for entry in patches} == manifest["engine"]["patch_sha256"], "Packaged engine patch provenance mismatch")
    planner_digest = manifest["shared_planner_sha256"].get("crates/avian-routing/src/lib.rs")
    _require(planner_digest is not None and engine.get("shared_routing_sha256", {}).get("src/lib.rs") == planner_digest,
             "Packaged engine uses a different shared planner")
    feature_patches = {
        "measured-routing.patch": "measured_routing", "underlay-binding.patch": "underlay_binding",
        "private-rpc.patch": "private_rpc", "managed-network.patch": "managed_network",
    }
    for patch, feature in feature_patches.items():
        if patch in manifest["engine"]["patch_sha256"]:
            _require(engine.get(feature) is True, "Engine lacks required native feature: " + feature)
    if "managed-network.patch" in manifest["engine"]["patch_sha256"]:
        helper = engine.get("shared_network_helper_sha256")
        _require(isinstance(helper, dict) and set(helper) == {"Cargo.toml", "src/lib.rs", "src/client.rs", "src/transport.rs", "LICENSE"},
                 "Engine helper client provenance is missing")
        for name in ["src/lib.rs", "src/client.rs", "src/transport.rs"]:
            _require(helper[name] == manifest["shared_planner_sha256"].get("crates/avian-network-helper/" + name),
                     "Engine helper client differs from the approved source")
        _require(helper["LICENSE"] == expected[manifest["roles"]["license"]]["sha256"] and
                 isinstance(helper["Cargo.toml"], str) and HASH.fullmatch(helper["Cargo.toml"]),
                 "Engine helper manifest or license provenance is invalid")
        if manifest["platform"] == "linux":
            helper_binary = manifest["roles"]["network_helper"]
            _require(engine.get("binary_sha256", {}).get("avian-network-helper") == expected[helper_binary]["sha256"],
                     "Packaged helper differs from the compiled managed runtime")
    for role in ["engine", "engine_cli"]:
        if role in manifest["roles"]:
            name = manifest["roles"][role]
            _require(engine.get("binary_sha256", {}).get(PurePosixPath(name).name) == expected[name]["sha256"], "Packaged engine binary hash mismatch")
    return {"archive_sha256": archive_digest, "manifest_sha256": hashlib.sha256(_encoded(manifest)).hexdigest()}


def verify_native_evidence(evidence, manifest, archive_sha256, *, stage="release"):
    """Core gates allow isolated native testing before hardware integration is permitted."""
    _require(stage in {"native_core", "release"}, "Unknown qualification stage")
    _require(evidence.get("schema_version") == SCHEMA_VERSION and evidence.get("status") == "pass", "Native qualification evidence has not passed")
    for name in ["target", "platform", "source_commit", "version", "engine", "protocol_sha256", "shared_planner_sha256"]:
        _require(evidence.get(name) == manifest[name], "Native evidence does not match artifact " + name)
    _require(evidence.get("archive_sha256") == archive_sha256, "Native evidence identifies a different installed archive")
    installed = {name: entry["sha256"] for name, entry in manifest["files"].items()}
    _require(evidence.get("installed_file_sha256") == installed, "Installed files differ from packaged artifact")
    execution = evidence.get("execution", {})
    _require(execution.get("kind") == "native_installed_runtime" and execution.get("engine_forwarding") is True and
             execution.get("compiler_only") is False and execution.get("platform_api_probe") is False,
             "Compiler checks and platform API probes cannot qualify native forwarding")
    gates = evidence.get("gates", [])
    _require(isinstance(gates, list) and all(isinstance(gate, dict) for gate in gates), "Invalid native qualification gates")
    names = [gate.get("name") for gate in gates]
    _require(len(names) == len(set(names)), "Duplicate native qualification gate")
    required = REQUIRED_CORE_GATES | PLATFORM_GATES[manifest["platform"]]
    if stage == "release":
        required = REQUIRED_GATES[manifest["platform"]]
        _require(execution.get("physical_hardware") is True, "Hardware interoperability requires physical native execution")
    _require(required <= set(names), "Native qualification gates are missing: " + ", ".join(sorted(required - set(names))))
    for gate in gates:
        _require(gate.get("status") == "pass" and gate.get("archive_sha256") == archive_sha256,
                 "Native gate failed or belongs to another artifact")
    return True


def verify_release_set(directory, *, repository, source_commit, version):
    """Require every platform's exact installed artifact before creating a release tag."""
    directory = Path(directory)
    release = _json(_regular(directory, "release-set.json"))
    _require(set(release) == {"schema_version", "source_commit", "version", "artifacts"} and
             release["schema_version"] == SCHEMA_VERSION, "Invalid release artifact index")
    _require(release["source_commit"] == source_commit and release["version"] == version, "Release artifact index identifies another source version")
    _require(isinstance(release["artifacts"], list), "Release artifact entries are missing")
    platforms = set()
    targets = set()
    verified = []
    for entry in release["artifacts"]:
        _require(set(entry) == {"target", "archive", "manifest", "evidence"}, "Invalid release artifact entry")
        _require(entry["target"] not in targets, "Duplicate release target")
        targets.add(entry["target"])
        manifest = _json(_regular(directory, entry["manifest"]))
        _validate_manifest(manifest)
        _require(manifest["target"] == entry["target"] and manifest["source_commit"] == source_commit and manifest["version"] == version,
                 "Artifact target or source version mismatch")
        _verify_source(manifest, repository)
        archive = _regular(directory, entry["archive"])
        digests = verify_archive(archive, manifest)
        evidence = _json(_regular(directory, entry["evidence"]))
        verify_native_evidence(evidence, manifest, digests["archive_sha256"])
        platforms.add(manifest["platform"])
        verified.append({"target": entry["target"], "archive": entry["archive"], **digests})
    _require(platforms == PLATFORMS, "Release requires native Linux, macOS, Windows, and Android artifacts and evidence")
    return {"source_commit": source_commit, "version": version, "artifacts": verified}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("build")
    build.add_argument("--stage", type=Path, required=True)
    build.add_argument("--metadata", type=Path, required=True)
    build.add_argument("--output", type=Path, required=True)
    build.add_argument("--manifest", type=Path, required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("--directory", type=Path, required=True)
    verify.add_argument("--repository", type=Path, required=True)
    verify.add_argument("--source-commit", required=True)
    verify.add_argument("--version", required=True)
    arguments = parser.parse_args()
    try:
        if arguments.command == "build":
            result = build_artifact(arguments.stage, _json(arguments.metadata), arguments.output)
            arguments.manifest.write_bytes(_encoded(result))
        else:
            result = verify_release_set(arguments.directory, repository=arguments.repository,
                                        source_commit=arguments.source_commit, version=arguments.version)
        print(json.dumps(result, sort_keys=True, indent=2))
    except (ValueError, OSError, KeyError, TypeError, tarfile.TarError) as error:
        parser.exit(1, str(error) + "\n")


if __name__ == "__main__":
    main()
