"""Package qualified artifacts locally and retrieve the same bundle in main CI."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import ssl
import tarfile
import tempfile
import urllib.request

from release_artifacts import _relative, _regular, verify_release_set
from release_version import release_identity

MAXIMUM_BUNDLE_BYTES = 8 * 1024**3
MAXIMUM_FILES = 128


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        while chunk := stream.read(1024 * 1024):
            value.update(chunk)
    return value.hexdigest()


def create(directory, output, *, repository, source_commit, version):
    directory, output = Path(directory), Path(output)
    verify_release_set(
        directory, repository=repository, source_commit=source_commit, version=version
    )
    index = json.loads(_regular(directory, "release-set.json").read_bytes())
    names = {"release-set.json"}
    for artifact in index["artifacts"]:
        names.update(artifact[key] for key in ("archive", "manifest", "evidence"))
    if len(names) > MAXIMUM_FILES:
        raise ValueError("Release bundle exceeds its file budget")
    # Exclusive creation prevents replacing an existing qualification bundle.
    with output.open("xb") as destination:
        try:
            with tarfile.open(fileobj=destination, mode="w|") as archive:
                for name in sorted(names):
                    source = _regular(directory, name)
                    info = tarfile.TarInfo(name)
                    info.size = source.stat().st_size
                    info.mode = 0o600
                    with source.open("rb") as stream:
                        archive.addfile(info, stream)
        except BaseException:
            output.unlink(missing_ok=True)
            raise
    if output.stat().st_size > MAXIMUM_BUNDLE_BYTES:
        output.unlink()
        raise ValueError("Release bundle exceeds its byte budget")
    return {"sha256": digest(output), "bytes": output.stat().st_size}


def extract(bundle, destination):
    """Extract regular files only, without trusting tar paths, links, or metadata."""
    destination = Path(destination)
    seen, total = set(), 0
    with tarfile.open(bundle, mode="r|") as archive:
        for member in archive:
            name = _relative(member.name)
            if not member.isfile() or name in seen or len(seen) >= MAXIMUM_FILES:
                raise ValueError(
                    "Release bundle contains an invalid or duplicate member"
                )
            seen.add(name)
            total += member.size
            if member.size < 0 or total > MAXIMUM_BUNDLE_BYTES:
                raise ValueError("Release bundle exceeds its extracted byte budget")
            target = destination / name
            target.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
            source = archive.extractfile(member)
            if source is None:
                raise ValueError("Release bundle member is unreadable")
            with target.open("xb") as output:
                shutil.copyfileobj(source, output, 1024 * 1024)
            target.chmod(0o600)
    if "release-set.json" not in seen:
        raise ValueError("Release bundle has no artifact index")


def fetch(destination, *, repository, environment=None):
    environment = os.environ if environment is None else environment
    if (
        environment.get("CI_COMMIT_BRANCH") != "main"
        or environment.get("CI_PIPELINE_SOURCE") not in {"push", "web"}
        or environment.get("CI_COMMIT_TAG")
    ):
        raise ValueError("Qualified bundles may be retrieved only by a main pipeline")
    identity = release_identity(repository, remote="origin", environment=environment)
    if identity["commit"] != environment.get("CI_COMMIT_SHA"):
        raise ValueError("Qualified bundle pipeline commit differs")
    checksum = environment.get("AVIAN_RELEASE_BUNDLE_SHA256", "")
    size = environment.get("AVIAN_RELEASE_BUNDLE_BYTES", "")
    project = environment.get("CI_PROJECT_ID", "")
    token = environment.get("CI_JOB_TOKEN", "")
    if (
        len(checksum) != 64
        or any(character not in "0123456789abcdef" for character in checksum)
        or not size.isdigit()
        or not 0 < int(size) <= MAXIMUM_BUNDLE_BYTES
        or not project.isdigit()
        or not token
        or environment.get("CI_API_V4_URL") != "https://repo.bespin.cce.af.mil/api/v4"
    ):
        raise ValueError("Missing or invalid qualified bundle identity")
    destination = Path(destination).absolute()
    if destination.exists():
        raise ValueError("Qualified bundle destination already exists")
    destination.parent.mkdir(parents=True, exist_ok=True)
    url = (
        f"https://repo.bespin.cce.af.mil/api/v4/projects/{project}/packages/generic/"
        f"avian-qualified/{identity['version']}-{identity['commit']}/{checksum}.tar"
    )
    context = ssl.create_default_context(
        cafile=environment.get("CI_SERVER_TLS_CA_FILE") or None
    )
    opener = urllib.request.build_opener(
        NoRedirect(), urllib.request.HTTPSHandler(context=context)
    )
    with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
        temporary = Path(temporary)
        bundle = temporary / "bundle.tar"
        request = urllib.request.Request(url, headers={"JOB-TOKEN": token})
        with opener.open(request, timeout=60) as response, bundle.open("xb") as output:
            count = 0
            while chunk := response.read(1024 * 1024):
                count += len(chunk)
                if count > int(size):
                    raise ValueError("Qualified bundle exceeds its expected size")
                output.write(chunk)
        if count != int(size) or digest(bundle) != checksum:
            raise ValueError("Qualified bundle differs from its expected hash or size")
        staged = temporary / "artifacts"
        staged.mkdir(mode=0o700)
        extract(bundle, staged)
        verified = verify_release_set(
            staged,
            repository=repository,
            source_commit=identity["commit"],
            version=identity["version"],
        )
        staged.rename(destination)
    return verified


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    package = subcommands.add_parser("create")
    package.add_argument("directory", type=Path)
    package.add_argument("output", type=Path)
    package.add_argument("--commit", required=True)
    package.add_argument("--version", required=True)
    retrieve = subcommands.add_parser("fetch")
    retrieve.add_argument("destination", type=Path)
    arguments = parser.parse_args()
    repository = Path(__file__).resolve().parents[1]
    if arguments.command == "create":
        result = create(
            arguments.directory,
            arguments.output,
            repository=repository,
            source_commit=arguments.commit,
            version=arguments.version,
        )
    else:
        result = fetch(arguments.destination, repository=repository)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, tarfile.TarError) as problem:
        raise SystemExit(str(problem))
