"""Stage an explicitly approved, natively qualified Linux package; never enable services."""
import argparse
import fcntl
import os
import stat
import json
from pathlib import Path
import shutil
import sys
import tarfile
import tempfile

# Distribution payloads place the verifier alongside this script in libexec/.
sys.path.insert(0, str(Path(__file__).resolve().parent / "libexec"))
from release_artifacts import digest_file, verify_archive, verify_native_evidence


def install(archive, manifest, evidence, destination, approved_sha256):
    archive = Path(archive)
    destination = Path(destination)
    if manifest["platform"] != "linux":
        raise ValueError("This installer accepts only Linux packages")
    if destination.exists() or destination.is_symlink() or not destination.is_absolute():
        raise ValueError("Choose a new absolute installation directory")
    if destination.parent.is_symlink() or not destination.parent.is_dir():
        raise ValueError("Installation parent must be an existing ordinary directory")
    parent_metadata = destination.parent.stat()
    if parent_metadata.st_uid != os.geteuid() or stat.S_IMODE(parent_metadata.st_mode) & 0o022:
        raise ValueError("Installation parent must be owner-controlled")
    # A private snapshot prevents source replacement between approval and extraction.
    parent_descriptor = os.open(destination.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        fcntl.flock(parent_descriptor, fcntl.LOCK_EX)
        return _stage(archive, manifest, evidence, destination, approved_sha256, parent_descriptor)
    finally:
        os.close(parent_descriptor)


def _stage(archive, manifest, evidence, destination, approved_sha256, parent_descriptor):
    with tempfile.TemporaryFile() as snapshot:
        with archive.open("rb") as source:
            shutil.copyfileobj(source, snapshot)
        snapshot.seek(0)
        verified = verify_archive(snapshot, manifest)
        if verified["archive_sha256"] != approved_sha256:
            raise ValueError("Package differs from the operator-approved SHA256")
        verify_native_evidence(evidence, manifest, verified["archive_sha256"], stage="native_core")
        with tempfile.TemporaryDirectory(prefix=".avian-install-", dir=destination.parent) as temporary:
            temporary = Path(temporary)
            snapshot.seek(0)
            with tarfile.open(fileobj=snapshot, mode="r:") as payload:
                for member in payload:
                    target = temporary / member.name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with payload.extractfile(member) as source, target.open("xb") as output:
                        shutil.copyfileobj(source, output)
                    target.chmod(member.mode)
            for name, entry in manifest["files"].items():
                if digest_file(temporary / name) != entry["sha256"]:
                    raise ValueError("Staged payload differs from verified package")
            if destination.exists() or destination.is_symlink():
                raise ValueError("Installation destination appeared while staging")
            os.rename(temporary.name, destination.name, src_dir_fd=parent_descriptor, dst_dir_fd=parent_descriptor)
    return {"installed_directory": str(destination), "archive_sha256": approved_sha256,
            "services_enabled": False}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--evidence", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--approved-sha256", required=True)
    arguments = parser.parse_args()
    try:
        print(json.dumps(install(arguments.archive, json.loads(arguments.manifest.read_text()),
                                 json.loads(arguments.evidence.read_text()), arguments.destination,
                                 arguments.approved_sha256), indent=2))
    except (ValueError, OSError, KeyError, tarfile.TarError) as error:
        parser.exit(1, str(error) + "\n")


if __name__ == "__main__":
    main()
