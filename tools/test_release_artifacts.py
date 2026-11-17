"""Artifact integrity and qualification gates are checked without installing system services."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest

import release_artifacts as artifacts


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repository = self.root / "repository"
        self.repository.mkdir()
        self.commit = "a" * 40
        self.version = "0.1.0"
        for name, content in {
            "LICENSE.md": b"test license fixture\n",
            "qualification/engine.lock.json": json.dumps({"source_commit": "b" * 40, "version": "2.6.4"}).encode(),
            "qualification/patches/forwarding.patch": b"test patch fixture\n",
            "qualification/patches/managed-network.patch": b"test managed patch fixture\n",
            "crates/avian-network-helper/src/lib.rs": b"test helper protocol fixture\n",
            "crates/avian-network-helper/src/client.rs": b"test helper client fixture\n",
            "crates/avian-network-helper/src/transport.rs": b"test helper transport fixture\n",
            "platforms/android/android-sockets.patch": b"test Android socket adapter fixture\n",
            "crates/avian-routing/src/lib.rs": b"test planner fixture\n",
        }.items():
            path = self.repository / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)

    def payload(self, platform="linux"):
        targets = {"linux": "aarch64-unknown-linux-gnu", "macos": "aarch64-apple-darwin",
                   "windows": "x86_64-pc-windows-msvc", "android": "aarch64-linux-android"}
        stage = self.root / (platform + "-stage")
        stage.mkdir()
        required = artifacts.COMMON_ROLES | ({"application", "native_bridge"} if platform == "android" else artifacts.DESKTOP_ROLES)
        if platform == "linux":
            required |= {"revocation_service", "network_helper_service", "network_helper_socket", "service_account", "release_validator"}
        roles = {role: "payload/" + role for role in required}
        for role, name in roles.items():
            path = stage / name
            path.parent.mkdir(exist_ok=True)
            path.write_bytes(("test payload: " + role).encode())
        (stage / roles["license"]).write_bytes((self.repository / "LICENSE.md").read_bytes())
        source = artifacts.source_hashes(self.repository)
        patches = {Path(path).name: digest for path, digest in source["protocol_sha256"].items()
                   if path.endswith(".patch") and (path != "platforms/android/android-sockets.patch" or platform == "android")}
        engine = {"source_commit": "b" * 40, "version": "2.6.4", "shared_routing_sha256": {
            "src/lib.rs": source["shared_planner_sha256"]["crates/avian-routing/src/lib.rs"]}, "patches": [
            {"name": name, "sha256": digest} for name, digest in patches.items()],
            "binary_sha256": {Path(roles[role]).name: artifacts.digest_file(stage / roles[role]) for role in ["engine", "engine_cli"] if role in roles}}
        engine["managed_network"] = True
        engine["shared_network_helper_sha256"] = {
            name: source["shared_planner_sha256"]["crates/avian-network-helper/" + name]
            for name in ["src/lib.rs", "src/client.rs", "src/transport.rs"]}
        engine["shared_network_helper_sha256"].update({"Cargo.toml": hashlib.sha256(b"manifest fixture").hexdigest(),
            "LICENSE": artifacts.digest_file(self.repository / "LICENSE.md")})
        if platform == "linux":
            engine["binary_sha256"]["avian-network-helper"] = artifacts.digest_file(stage / roles["network_helper"])
        (stage / roles["engine_manifest"]).write_text(json.dumps(engine))
        metadata = {"target": targets[platform], "platform": platform, "source_commit": self.commit,
                    "version": self.version, "roles": roles, "privileged_manager": False,
                    "engine": {"source_commit": "b" * 40, "version": "2.6.4", "patch_sha256": patches}, **source}
        archive = self.root / (platform + ".tar")
        manifest = artifacts.build_artifact(stage, metadata, archive)
        return stage, archive, manifest

    def evidence(self, archive, manifest):
        digest = artifacts.digest_file(archive)
        return {"schema_version": 1, "status": "pass", **{name: manifest[name] for name in [
            "target", "platform", "source_commit", "version", "engine", "protocol_sha256", "shared_planner_sha256"]},
            "archive_sha256": digest, "installed_file_sha256": {name: record["sha256"] for name, record in manifest["files"].items()},
            "execution": {"kind": "native_installed_runtime", "engine_forwarding": True, "compiler_only": False,
                          "platform_api_probe": False, "physical_hardware": True},
            "gates": [{"name": name, "status": "pass", "archive_sha256": digest}
                      for name in sorted(artifacts.REQUIRED_GATES[manifest["platform"]])]}

    def test_missing_feature_and_changed_helper_client_provenance_fail(self):
        stage, archive, manifest = self.payload()
        engine_path = stage / manifest["roles"]["engine_manifest"]
        original = json.loads(engine_path.read_text())
        metadata = {name: value for name, value in manifest.items() if name not in {"files", "schema_version"}}
        for mutation in ["feature", "client", "binary"]:
            engine = copy.deepcopy(original)
            if mutation == "feature":
                engine["managed_network"] = False
            elif mutation == "client":
                engine["shared_network_helper_sha256"]["src/client.rs"] = "0" * 64
            else:
                engine["binary_sha256"]["avian-network-helper"] = "0" * 64
            engine_path.write_text(json.dumps(engine))
            with self.assertRaises(ValueError):
                artifacts.build_artifact(stage, metadata, self.root / (mutation + ".tar"))

    def test_archive_is_deterministic_and_preserves_exact_embedded_manifest(self):
        stage, archive, manifest = self.payload()
        second = self.root / "second.tar"
        metadata = {name: value for name, value in manifest.items() if name not in {"files", "schema_version"}}
        artifacts.build_artifact(stage, metadata, second)
        self.assertEqual(archive.read_bytes(), second.read_bytes())
        self.assertEqual(artifacts.verify_archive(archive, manifest)["archive_sha256"], artifacts.digest_file(archive))

    def rewrite_archive(self, archive, *, omit=None, extra=None, alter=None):
        changed = self.root / "changed.tar"
        with tarfile.open(archive, "r:") as original, tarfile.open(changed, "w", format=tarfile.USTAR_FORMAT) as output:
            for member in original:
                if member.name == omit:
                    continue
                content = original.extractfile(member).read()
                if member.name == alter:
                    content = b"x" * len(content)
                output.addfile(member, io.BytesIO(content))
            if extra:
                info, content = extra
                output.addfile(info, io.BytesIO(content))
        return changed

    def test_missing_modified_unexpected_and_duplicate_members_fail(self):
        _, archive, manifest = self.payload()
        name = manifest["roles"]["engine"]
        for changed in [self.rewrite_archive(archive, omit=name), self.rewrite_archive(archive, alter=name)]:
            with self.assertRaises(ValueError):
                artifacts.verify_archive(changed, manifest)
        for name in ["unexpected", name]:
            extra = tarfile.TarInfo(name)
            extra.mode = 0o644
            changed = self.rewrite_archive(archive, extra=(extra, b""))
            with self.assertRaises(ValueError):
                artifacts.verify_archive(changed, manifest)

    def test_unsafe_paths_and_links_are_rejected_without_extraction(self):
        _, archive, manifest = self.payload()
        for name, kind in [("../escape", tarfile.REGTYPE), ("/escape", tarfile.REGTYPE),
                           ("symbolic", tarfile.SYMTYPE), ("hard", tarfile.LNKTYPE)]:
            extra = tarfile.TarInfo(name)
            extra.type = kind
            extra.linkname = "../escape" if kind != tarfile.REGTYPE else ""
            changed = self.rewrite_archive(archive, extra=(extra, b""))
            with self.assertRaises(ValueError):
                artifacts.verify_archive(changed, manifest)
        self.assertFalse((self.root / "escape").exists())

    def test_portable_path_aliases_and_normalization_collisions_fail(self):
        _, archive, manifest = self.payload()
        for name in ["payload/file.", "payload/file ", "payload/CON", "payload/con.txt", "payload/NUL.key",
                     "payload/AUX", "payload/PRN", "payload/COM1.txt", "payload/LPT9", "payload/COM¹",
                     "payload/a<b", "payload/a>b", "payload/a|b", "payload/a?b", "payload/a*b", 'payload/a"b']:
            with self.subTest(name=name):
                extra = tarfile.TarInfo(name)
                extra.mode = 0o644
                changed = self.rewrite_archive(archive, extra=(extra, b""))
                with self.assertRaises(ValueError):
                    artifacts.verify_archive(changed, manifest)
        for first, second in [("payload/identity", "payload/IDENTITY"), ("payload/café", "payload/cafe\u0301")]:
            changed = copy.deepcopy(manifest)
            record = next(iter(changed["files"].values()))
            changed["files"][first] = dict(record)
            changed["files"][second] = dict(record)
            with self.assertRaisesRegex(ValueError, "collide"):
                artifacts.verify_archive(archive, changed)

    def test_insecure_payload_inputs_and_missing_native_helper_fail_packaging(self):
        stage, _, manifest = self.payload()
        metadata = {name: value for name, value in manifest.items() if name not in {"files", "schema_version"}}
        del metadata["roles"]["network_helper"]
        with self.assertRaisesRegex(ValueError, "network_helper"):
            artifacts.build_artifact(stage, metadata, self.root / "invalid.tar")
        metadata["roles"]["network_helper"] = "payload/network_helper"
        (stage / "link").symlink_to(stage / "payload/engine")
        with self.assertRaisesRegex(ValueError, "Symbolic links"):
            artifacts.build_artifact(stage, metadata, self.root / "invalid.tar")

    def test_evidence_must_identify_installed_native_runtime_and_exact_artifact(self):
        _, archive, manifest = self.payload()
        evidence = self.evidence(archive, manifest)
        digest = artifacts.digest_file(archive)
        artifacts.verify_native_evidence(evidence, manifest, digest)
        for field, value in [("source_commit", "c" * 40), ("archive_sha256", "0" * 64),
                             ("target", "x86_64-pc-windows-msvc"), ("installed_file_sha256", {}), ("status", "fail")]:
            changed = copy.deepcopy(evidence)
            changed[field] = value
            with self.assertRaises(ValueError):
                artifacts.verify_native_evidence(changed, manifest, digest)
        for field, value in [("kind", "compiler_check"), ("kind", "emulator_api_probe"),
                             ("engine_forwarding", False), ("compiler_only", True), ("platform_api_probe", True)]:
            changed = copy.deepcopy(evidence)
            changed["execution"][field] = value
            with self.assertRaises(ValueError):
                artifacts.verify_native_evidence(changed, manifest, digest)

    def test_core_checks_do_not_require_hardware_interoperability(self):
        _, archive, manifest = self.payload()
        evidence = self.evidence(archive, manifest)
        evidence["execution"]["physical_hardware"] = False
        evidence["gates"] = [gate for gate in evidence["gates"] if gate["name"] not in artifacts.REQUIRED_RELEASE_GATES]
        artifacts.verify_native_evidence(evidence, manifest, artifacts.digest_file(archive), stage="native_core")
        with self.assertRaises(ValueError):
            artifacts.verify_native_evidence(evidence, manifest, artifacts.digest_file(archive))

    def test_failed_missing_duplicate_and_wrong_artifact_gates_fail(self):
        _, archive, manifest = self.payload()
        evidence = self.evidence(archive, manifest)
        variants = []
        for field, value in [("status", "fail"), ("archive_sha256", "0" * 64)]:
            changed = copy.deepcopy(evidence)
            changed["gates"][0][field] = value
            variants.append(changed)
        changed = copy.deepcopy(evidence)
        changed["gates"].pop()
        variants.append(changed)
        changed = copy.deepcopy(evidence)
        changed["gates"].append(changed["gates"][0])
        variants.append(changed)
        for changed in variants:
            with self.assertRaises(ValueError):
                artifacts.verify_native_evidence(changed, manifest, artifacts.digest_file(archive))

    def release_set(self, platforms):
        entries = []
        for platform in platforms:
            _, archive, manifest = self.payload(platform)
            manifest_path = self.root / (platform + ".json")
            evidence_path = self.root / (platform + "-evidence.json")
            manifest_path.write_text(json.dumps(manifest))
            evidence_path.write_text(json.dumps(self.evidence(archive, manifest)))
            entries.append({"target": manifest["target"], "archive": archive.name,
                            "manifest": manifest_path.name, "evidence": evidence_path.name})
        index = {"schema_version": 1, "source_commit": self.commit, "version": self.version, "artifacts": entries}
        (self.root / "release-set.json").write_text(json.dumps(index))
        return index

    def test_publication_requires_all_four_native_platforms_at_the_same_source(self):
        self.release_set(["linux", "macos", "windows", "android"])
        report = artifacts.verify_release_set(self.root, repository=self.repository, source_commit=self.commit, version=self.version)
        self.assertEqual(len(report["artifacts"]), 4)
        with self.assertRaises(ValueError):
            artifacts.verify_release_set(self.root, repository=self.repository, source_commit="c" * 40, version=self.version)
        index = json.loads((self.root / "release-set.json").read_text())
        index["artifacts"].pop()
        (self.root / "release-set.json").write_text(json.dumps(index))
        with self.assertRaisesRegex(ValueError, "Linux, macOS, Windows, and Android"):
            artifacts.verify_release_set(self.root, repository=self.repository, source_commit=self.commit, version=self.version)

    def test_android_socket_patch_is_bound_to_android_engine_only(self):
        _, _, linux = self.payload("linux")
        _, _, android = self.payload("android")
        self.assertIn("platforms/android/android-sockets.patch", linux["protocol_sha256"])
        self.assertNotIn("android-sockets.patch", linux["engine"]["patch_sha256"])
        self.assertIn("android-sockets.patch", android["engine"]["patch_sha256"])
        linux["engine"]["patch_sha256"]["android-sockets.patch"] = android["engine"]["patch_sha256"]["android-sockets.patch"]
        with self.assertRaisesRegex(ValueError, "target-specific"):
            artifacts._verify_source(linux, self.repository)
        del android["engine"]["patch_sha256"]["android-sockets.patch"]
        with self.assertRaisesRegex(ValueError, "target-specific"):
            artifacts._verify_source(android, self.repository)

    def test_source_patch_or_planner_changes_invalidate_existing_evidence(self):
        self.release_set(["linux", "macos", "windows", "android"])
        (self.repository / "crates/avian-routing/src/lib.rs").write_text("changed planner")
        with self.assertRaisesRegex(ValueError, "shared_planner"):
            artifacts.verify_release_set(self.root, repository=self.repository, source_commit=self.commit, version=self.version)

    def test_installer_stages_only_the_operator_approved_qualified_payload(self):
        _, archive, manifest = self.payload()
        installer_path = Path(__file__).resolve().parents[1] / "packaging/linux/install.py"
        specification = importlib.util.spec_from_file_location("linux_install", installer_path)
        installer = importlib.util.module_from_spec(specification)
        specification.loader.exec_module(installer)
        destination = self.root / "installed"
        with self.assertRaisesRegex(ValueError, "operator-approved"):
            installer.install(archive, manifest, self.evidence(archive, manifest), destination, "0" * 64)
        self.assertFalse(destination.exists())
        result = installer.install(archive, manifest, self.evidence(archive, manifest), destination, artifacts.digest_file(archive))
        self.assertFalse(result["services_enabled"])
        self.assertEqual(artifacts.digest_file(destination / manifest["roles"]["engine"]), manifest["files"][manifest["roles"]["engine"]]["sha256"])
        with self.assertRaisesRegex(ValueError, "new absolute"):
            installer.install(archive, manifest, self.evidence(archive, manifest), destination, artifacts.digest_file(archive))


if __name__ == "__main__":
    unittest.main()
