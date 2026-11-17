"""Package publication preserves qualified content across interrupted retries."""

import hashlib
import unittest
import release_uploads


class UploadTests(unittest.TestCase):
    def file(self):
        body = b"qualified payload"
        return {
            "body": body,
            "path": None,
            "size": len(body),
            "sha256": hashlib.sha256(body).hexdigest(),
            "url": "https://repo.bespin.cce.af.mil/api/v4/projects/4099/packages/generic/avian/0.1.0-commit/payload.tar",
        }

    def test_missing_package_is_uploaded_once_then_verified_and_reused(self):
        remote = {}
        calls = []
        file = self.file()

        def transfer(method, item):
            calls.append(method)
            if method == "PUT":
                remote[item["url"]] = item["sha256"]
            return remote.get(item["url"])

        release_uploads.synchronize([file], "test", transfer=transfer)
        release_uploads.synchronize([file], "test", transfer=transfer)
        self.assertEqual(calls, ["GET", "PUT", "GET", "GET"])

    def test_existing_different_package_is_never_overwritten(self):
        calls = []

        def transfer(method, item):
            calls.append(method)
            return "0" * 64

        with self.assertRaisesRegex(ValueError, "refusing overwrite"):
            release_uploads.synchronize([self.file()], "test", transfer=transfer)
        self.assertEqual(calls, ["GET"])

    def test_missing_existing_release_asset_is_not_repaired(self):
        with self.assertRaisesRegex(ValueError, "missing qualified"):
            release_uploads.synchronize(
                [self.file()], "test", transfer=lambda *args: None, allow_upload=False
            )

    def test_changed_local_or_remote_bytes_and_redirect_targets_are_rejected(self):
        file = self.file()
        file["body"] = b"changed"
        with self.assertRaisesRegex(ValueError, "changed before"):
            release_uploads.synchronize(
                [file], "test", transfer=lambda *args: self.fail("unexpected request")
            )
        file = self.file()
        file["url"] = "https://untrusted.example/payload"
        with self.assertRaisesRegex(ValueError, "endpoint"):
            release_uploads.synchronize(
                [file], "test", transfer=lambda *args: self.fail("unexpected request")
            )
        with self.assertRaisesRegex(ValueError, "Uploaded package hash"):
            release_uploads.synchronize(
                [self.file()], "test", transfer=lambda *args: None
            )
        self.assertIsNone(
            release_uploads.NoRedirect().redirect_request(
                None, None, 302, "redirect", {}, "https://untrusted.example"
            )
        )
