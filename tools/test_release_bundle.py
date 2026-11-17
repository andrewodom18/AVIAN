import io
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import release_bundle


class BundleTests(unittest.TestCase):
    def archive(self, path, entries):
        with tarfile.open(path, "w") as archive:
            for name, kind, body in entries:
                member = tarfile.TarInfo(name)
                member.type = kind
                member.size = len(body) if kind == tarfile.REGTYPE else 0
                member.linkname = "outside"
                archive.addfile(member, io.BytesIO(body))

    def test_regular_files_extract_without_tar_permissions(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            bundle = root / "bundle.tar"
            self.archive(
                bundle,
                [
                    ("release-set.json", tarfile.REGTYPE, b"{}"),
                    ("linux/package.tar", tarfile.REGTYPE, b"payload"),
                ],
            )
            destination = root / "result"
            destination.mkdir()
            release_bundle.extract(bundle, destination)
            self.assertEqual(
                (destination / "linux/package.tar").read_bytes(), b"payload"
            )
            self.assertEqual(
                (destination / "linux/package.tar").stat().st_mode & 0o777, 0o600
            )

    def test_paths_links_and_duplicate_members_are_rejected(self):
        for entries in [
            [("../outside", tarfile.REGTYPE, b"payload")],
            [("/outside", tarfile.REGTYPE, b"payload")],
            [("link", tarfile.SYMTYPE, b"")],
            [("link", tarfile.LNKTYPE, b"")],
            [
                ("release-set.json", tarfile.REGTYPE, b"{}"),
                ("release-set.json", tarfile.REGTYPE, b"{}"),
            ],
        ]:
            with self.subTest(
                entries=entries
            ), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                self.archive(root / "bundle.tar", entries)
                (root / "result").mkdir()
                with self.assertRaises(ValueError):
                    release_bundle.extract(root / "bundle.tar", root / "result")
                self.assertFalse((root.parent / "outside").exists())

    def test_missing_index_and_byte_budget_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.archive(
                root / "bundle.tar", [("payload", tarfile.REGTYPE, b"oversized")]
            )
            (root / "result").mkdir()
            with patch.object(
                release_bundle, "MAXIMUM_BUNDLE_BYTES", 2
            ), self.assertRaises(ValueError):
                release_bundle.extract(root / "bundle.tar", root / "result")
            with self.assertRaises(ValueError):
                release_bundle.extract(root / "bundle.tar", root / "result")

    def test_unqualified_input_never_creates_a_bundle(self):
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / "bundle.tar"
            with patch.object(
                release_bundle,
                "verify_release_set",
                side_effect=ValueError("unqualified"),
            ):
                with self.assertRaises(ValueError):
                    release_bundle.create(
                        temporary,
                        output,
                        repository=temporary,
                        source_commit="a" * 40,
                        version="0.1.0",
                    )
            self.assertFalse(output.exists())

    def test_non_main_or_missing_bundle_identity_never_downloads(self):
        with patch.object(release_bundle.urllib.request, "build_opener") as opener:
            for environment in [
                {},
                {"CI_COMMIT_BRANCH": "dev", "CI_PIPELINE_SOURCE": "web"},
            ]:
                with self.assertRaises(ValueError):
                    release_bundle.fetch(
                        "unused", repository="unused", environment=environment
                    )
            identity = {"commit": "a" * 40, "version": "0.1.0"}
            with patch.object(
                release_bundle, "release_identity", return_value=identity
            ):
                with self.assertRaises(ValueError):
                    release_bundle.fetch(
                        "unused",
                        repository="unused",
                        environment={
                            "CI_COMMIT_BRANCH": "main",
                            "CI_PIPELINE_SOURCE": "push",
                            "CI_COMMIT_SHA": "a" * 40,
                        },
                    )
            opener.assert_not_called()


if __name__ == "__main__":
    unittest.main()
