"""Exercise release identity against real disposable Git repositories."""

import importlib.util
from pathlib import Path
import subprocess
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("release_version", Path(__file__).with_name("release_version.py"))
RELEASE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RELEASE)


class ReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repository = Path(self.temporary.name)
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Release Test")
        self.git("config", "user.email", "release@example.invalid")
        (self.repository / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.0"\n')
        self.git("add", "Cargo.toml")
        self.git("commit", "-m", "Initial version")

    def git(self, *arguments):
        return subprocess.check_output(["git", "-C", str(self.repository), *arguments], stderr=subprocess.DEVNULL, text=True).strip()

    def test_initial_release_and_same_commit_retry(self):
        identity = RELEASE.release_identity(self.repository)
        self.assertEqual(identity["tag"], "avian-v0.1.0")
        self.assertFalse(identity["already_tagged"])
        self.git("tag", "-a", identity["tag"], "-m", "Initial release")
        self.assertTrue(RELEASE.release_identity(self.repository)["already_tagged"])

    def test_development_branch_cannot_spoof_main(self):
        self.git("switch", "-c", "dev")
        with self.assertRaisesRegex(ValueError, "current main"):
            RELEASE.release_identity(self.repository, environment={"CI_COMMIT_BRANCH": "main"})

    def test_detached_ci_requires_matching_main_reference(self):
        self.git("checkout", "--detach")
        with self.assertRaises(ValueError):
            RELEASE.release_identity(self.repository, environment={})
        self.assertEqual(RELEASE.release_identity(self.repository, environment={"CI_COMMIT_BRANCH": "main"})["tag"], "avian-v0.1.0")
        self.assertRaises(ValueError, RELEASE.release_identity, self.repository, None, {"CI_COMMIT_BRANCH": "main", "CI_COMMIT_TAG": "other"})

    def test_non_main_pipeline_cannot_publish_even_after_checkout_override(self):
        with self.assertRaisesRegex(ValueError, "target main"):
            RELEASE.release_identity(self.repository, environment={"CI_COMMIT_BRANCH": "dev"})
        with self.assertRaisesRegex(ValueError, "target main"):
            RELEASE.release_identity(self.repository, environment={"GITHUB_REF": "refs/heads/dev"})

    def test_detached_ci_cannot_release_a_commit_outside_main(self):
        self.git("switch", "-c", "dev")
        (self.repository / "file").write_text("development change")
        self.git("add", "file")
        self.git("commit", "-m", "Development change")
        self.git("checkout", "--detach")
        with self.assertRaisesRegex(ValueError, "verified main"):
            RELEASE.release_identity(self.repository, environment={"CI_COMMIT_BRANCH": "main"})

    def test_dirty_checkout_and_immutable_tag_are_rejected(self):
        (self.repository / "untracked").write_text("change")
        with self.assertRaisesRegex(ValueError, "clean"):
            RELEASE.release_identity(self.repository)
        (self.repository / "untracked").unlink()
        self.git("tag", "avian-v0.1.0")
        (self.repository / "file").write_text("next")
        self.git("add", "file")
        self.git("commit", "-m", "Next change")
        with self.assertRaisesRegex(ValueError, "Immutable"):
            RELEASE.release_identity(self.repository)

    def test_prereleases_and_invalid_semver(self):
        self.assertEqual(RELEASE.validate_version("0.1.0-rc.1"), "0.1.0-rc.1")
        for version in ["01.0.0", "0.1", "0.1.0-rc.01", "0.1.0+build", "0.1.0-"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                RELEASE.validate_version(version)

    def test_remote_tag_cannot_be_replaced(self):
        self.git("tag", "avian-v0.1.0")
        remote = self.repository / "remote.git"
        subprocess.run(["git", "init", "--bare", str(remote)], check=True, capture_output=True)
        self.git("remote", "add", "test", str(remote))
        self.git("push", "test", "avian-v0.1.0")
        self.git("tag", "-d", "avian-v0.1.0")
        self.git("config", "core.excludesFile", "/dev/null")
        # Keep the disposable remote outside the release working tree's status.
        (self.repository / ".git/info/exclude").write_text("remote.git/\n")
        (self.repository / "file").write_text("next")
        self.git("add", "file")
        self.git("commit", "-m", "Next change")
        with self.assertRaisesRegex(ValueError, "Immutable remote"):
            RELEASE.release_identity(self.repository, remote="test")


if __name__ == "__main__":
    unittest.main()
