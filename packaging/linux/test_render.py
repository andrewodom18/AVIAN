import importlib.util
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

specification = importlib.util.spec_from_file_location(
    "service_renderer", Path(__file__).with_name("render.py")
)
renderer = importlib.util.module_from_spec(specification)
specification.loader.exec_module(renderer)


class ServiceAccountTests(unittest.TestCase):
    def render(self, temporary, configuration, uid=1000):
        account = SimpleNamespace(pw_uid=uid, pw_gid=1000)
        group = SimpleNamespace(gr_name="bridge")
        with patch.object(renderer.pwd, "getpwnam", return_value=account), patch.object(
            renderer.grp, "getgrgid", return_value=group
        ):
            return renderer.render(
                "bridge", "mesh", configuration, Path(temporary) / "rendered"
            )

    def test_account_scope_must_match_and_root_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            for configuration, uid in [
                ({"allowed_uid": 1001, "runtime_directory": "/run/avian/mesh"}, 1000),
                ({"allowed_uid": 0, "runtime_directory": "/run/avian/mesh"}, 0),
                ({"allowed_uid": 1000, "runtime_directory": "/run/other"}, 1000),
            ]:
                with self.assertRaises(ValueError):
                    self.render(temporary, configuration, uid)
            self.assertFalse((Path(temporary) / "rendered").exists())

    def test_activated_socket_and_unprivileged_service_roles_are_rendered(self):
        with tempfile.TemporaryDirectory() as temporary:
            metadata = self.render(
                temporary, {"allowed_uid": 1000, "runtime_directory": "/run/avian/mesh"}
            )
            directory = Path(temporary) / "rendered"
            manager = (directory / "avian@mesh.service").read_text()
            helper = (directory / "avian-network-helper@mesh.service").read_text()
            socket = (directory / "avian-network-helper@mesh.socket").read_text()
            self.assertIn("User=1000", manager)
            self.assertIn("KillMode=control-group", manager)
            self.assertIn("CapabilityBoundingSet=\n", manager)
            self.assertIn("User=root", helper)
            self.assertIn("StandardInput=socket", helper)
            self.assertIn("network.json", helper)
            self.assertNotIn("RuntimeDirectory=", helper)
            self.assertNotIn("CAP_CHOWN", helper)
            self.assertIn("SocketUser=1000", socket)
            self.assertIn("SocketMode=0600", socket)
            self.assertEqual(len(metadata["service_sha256"]), 4)
            self.assertFalse(metadata["services_enabled"])


if __name__ == "__main__":
    unittest.main()
