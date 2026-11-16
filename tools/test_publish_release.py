"""Release publication must remain main-only, immutable, and retry-safe."""

from pathlib import Path
import unittest
from unittest.mock import patch

import publish_release


class PublishReleaseTests(unittest.TestCase):
    def setUp(self):
        self.identity = {
            "tag": "avian-v0.1.0",
            "version": "0.1.0",
            "commit": "a" * 40,
            "already_tagged": False,
        }
        self.environment = {
            "CI_COMMIT_BRANCH": "main",
            "CI_PIPELINE_SOURCE": "push",
            "CI_COMMIT_SHA": self.identity["commit"],
            "CI_API_V4_URL": "https://repo.bespin.cce.af.mil/api/v4",
            "CI_PROJECT_ID": "4099",
            "CI_JOB_TOKEN": "test-token-never-sent",
        }

    def publish(self, transport):
        with patch.object(publish_release, "release_identity", return_value=self.identity):
            return publish_release.publish(Path("unused"), self.environment, transport)

    def test_first_release_creates_tag_at_the_verified_commit(self):
        requests = []

        def transport(method, url, body=None):
            requests.append((method, url, body))
            if method == "GET":
                return None
            return {"commit": {"id": self.identity["commit"]}}

        self.assertEqual(self.publish(transport), self.identity)
        self.assertEqual([request[0] for request in requests], ["GET", "POST"])
        payload = requests[1][2]
        self.assertEqual(payload["tag_name"], "avian-v0.1.0")
        self.assertEqual(payload["ref"], self.identity["commit"])
        self.assertNotIn(self.environment["CI_JOB_TOKEN"], str(payload))

    def test_matching_existing_release_is_idempotent(self):
        requests = []

        def transport(method, url, body=None):
            requests.append(method)
            return {"commit": {"id": self.identity["commit"]}}

        self.assertEqual(self.publish(transport), self.identity)
        self.assertEqual(requests, ["GET"])

    def test_existing_release_for_another_commit_is_never_rewritten(self):
        requests = []

        def transport(method, url, body=None):
            requests.append(method)
            return {"commit": {"id": "b" * 40}}

        with self.assertRaisesRegex(ValueError, "different commit"):
            self.publish(transport)
        self.assertEqual(requests, ["GET"])

    def test_dev_tag_and_unapproved_pipeline_sources_cannot_publish(self):
        for updates in [
            {"CI_COMMIT_BRANCH": "dev"},
            {"CI_COMMIT_TAG": "avian-v0.1.0"},
            {"CI_PIPELINE_SOURCE": "merge_request_event"},
            {"CI_PIPELINE_SOURCE": "schedule"},
            {"CI_PIPELINE_SOURCE": "api"},
        ]:
            with self.subTest(updates=updates):
                environment = self.environment | updates
                with patch.object(publish_release, "release_identity") as identity:
                    with self.assertRaises(ValueError):
                        publish_release.publish(Path("unused"), environment)
                    identity.assert_not_called()

    def test_unexpected_sha_or_api_endpoint_is_rejected_before_requests(self):
        for name, value in [
            ("CI_COMMIT_SHA", "b" * 40),
            ("CI_API_V4_URL", "https://untrusted.example/api/v4"),
            ("CI_PROJECT_ID", "not-a-project"),
            ("CI_JOB_TOKEN", ""),
        ]:
            with self.subTest(name=name):
                original = self.environment[name]
                self.environment[name] = value
                with self.assertRaises(ValueError):
                    self.publish(lambda *arguments: self.fail("unexpected API request"))
                self.environment[name] = original

    def test_post_response_must_match_the_verified_commit(self):
        def transport(method, url, body=None):
            return None if method == "GET" else {"commit": {"id": "b" * 40}}

        with self.assertRaisesRegex(ValueError, "did not match"):
            self.publish(transport)

    def test_manual_main_pipeline_is_supported(self):
        self.environment["CI_PIPELINE_SOURCE"] = "web"
        self.assertEqual(
            self.publish(lambda *arguments: {"commit": {"id": self.identity["commit"]}}),
            self.identity,
        )

    def test_redirects_are_not_followed(self):
        handler = publish_release.NoRedirect()
        self.assertIsNone(handler.redirect_request(None, None, 302, "Found", {}, "https://untrusted.example"))


if __name__ == "__main__":
    unittest.main()
