#!/usr/bin/env python3
"""Run pinned upstream networking checks in an isolated Linux container."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import urllib.request
import zipfile

DIRECTORY = Path(__file__).resolve().parent


def execute(arguments):
    return subprocess.run(arguments, check=True, text=True, capture_output=True).stdout.strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True, help="Directory for measured results")
    parser.add_argument("--archive", type=Path, help="Previously downloaded official release archive")
    parser.add_argument("--engine-directory", type=Path, help="Candidate Linux binaries; candidate provenance is recorded separately")
    parser.add_argument("--timeout", type=float, default=15, help="Seconds to wait for route readiness")
    arguments = parser.parse_args()
    if arguments.engine_directory and not (arguments.engine_directory / "easytier-core").is_file():
        parser.error("--engine-directory must contain easytier-core")
    if arguments.timeout <= 0:
        parser.error("--timeout must be positive")
    architecture = execute(["docker", "info", "--format", "{{.Architecture}}"])
    architecture = {"arm64": "aarch64", "amd64": "x86_64"}.get(architecture, architecture)
    lock = json.loads((DIRECTORY / "engine.lock.json").read_text())
    if architecture not in lock["assets"]:
        parser.error(f"No pinned release asset for Docker architecture {architecture}")
    asset = lock["assets"][architecture]
    arguments.output = arguments.output.resolve()
    arguments.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="avian-qualification-") as temporary_directory:
        temporary_path = Path(temporary_directory)
        archive_path = arguments.archive or temporary_path / "engine.zip"
        if arguments.archive is None:
            urllib.request.urlretrieve(asset["url"], archive_path)
        actual_digest = hashlib.sha256(archive_path.read_bytes()).hexdigest()
        if actual_digest != asset["sha256"]:
            raise SystemExit("Release archive checksum does not match engine.lock.json")
        binary_directory = temporary_path / "bin"
        binary_directory.mkdir()
        with zipfile.ZipFile(archive_path) as archive:
            for binary_name in ["easytier-core", "easytier-cli"]:
                matching = [name for name in archive.namelist() if name.endswith("/" + binary_name)]
                if len(matching) != 1:
                    raise SystemExit(f"Archive must contain exactly one {binary_name}")
                target = binary_directory / binary_name
                target.write_bytes(archive.read(matching[0]))
                target.chmod(0o755)
        if arguments.engine_directory:
            for binary_name in ["easytier-core", "easytier-cli"]:
                candidate = arguments.engine_directory / binary_name
                if candidate.exists():
                    target = binary_directory / binary_name
                    target.write_bytes(candidate.read_bytes())
                    target.chmod(0o755)
            provenance_path = arguments.engine_directory / "build.json"
            if provenance_path.exists():
                provenance = json.loads(provenance_path.read_text())
                expected_hashes = provenance.get("binary_sha256", {})
                for binary_name in ["easytier-core", "easytier-cli"]:
                    expected_hash = expected_hashes.get(binary_name)
                    if expected_hash is None or hashlib.sha256((binary_directory / binary_name).read_bytes()).hexdigest() != expected_hash:
                        raise SystemExit(f"Candidate provenance does not match {binary_name}")
                (arguments.output / "candidate-build.json").write_text(json.dumps(provenance, indent=2) + "\n")
        subprocess.run(["docker", "build", "-t", "avian-qualification:local", str(DIRECTORY)], check=True)
        result = subprocess.run([
            "docker", "run", "--rm", "--network", "none", "--privileged",
            "--mount", f"type=bind,source={DIRECTORY},target=/qualification,readonly",
            "--mount", f"type=bind,source={binary_directory},target=/engine,readonly",
            "--mount", f"type=bind,source={arguments.output},target=/results",
            "--tmpfs", "/state:mode=0700", "avian-qualification:local",
            "--timeout", str(arguments.timeout),
        ])
        raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
