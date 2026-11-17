import hashlib
import importlib.util
from pathlib import Path
import tempfile
import tomllib
import unittest

specification = importlib.util.spec_from_file_location(
    "engine_builder", Path(__file__).resolve().parents[1] / "build.py"
)
builder = importlib.util.module_from_spec(specification)
specification.loader.exec_module(builder)


class EngineHelperBoundaryTests(unittest.TestCase):
    def test_engine_vendor_has_only_client_dependencies_and_complete_source_hashes(
        self,
    ):
        with tempfile.TemporaryDirectory() as temporary:
            destination = Path(temporary) / "helper"
            hashes = builder.prepare_shared_network_helper(destination)
            manifest = tomllib.loads((destination / "Cargo.toml").read_text())
            self.assertEqual(manifest["features"]["default"], [])
            self.assertEqual(manifest["features"]["server"], [])
            self.assertNotIn("avian-storage", manifest["dependencies"])
            self.assertFalse((destination / "src/server.rs").exists())
            self.assertFalse((destination / "src/main.rs").exists())
            self.assertEqual(
                set(hashes),
                {
                    "Cargo.toml",
                    "src/lib.rs",
                    "src/client.rs",
                    "src/transport.rs",
                    "LICENSE",
                },
            )
            for name, digest in hashes.items():
                self.assertEqual(
                    digest,
                    hashlib.sha256((destination / name).read_bytes()).hexdigest(),
                )


if __name__ == "__main__":
    unittest.main()
