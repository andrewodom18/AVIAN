"""Validate the current main commit and print its immutable AVIAN release identity."""

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import tomllib

VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?")


def git(repository, *arguments, optional=False):
    result = subprocess.run(["git", "-C", str(repository), *arguments], text=True, capture_output=True)
    if result.returncode:
        if optional:
            return None
        raise ValueError("Git command failed: " + " ".join(arguments))
    return result.stdout.strip()


def validate_version(version):
    match = VERSION.fullmatch(version)
    if not match:
        raise ValueError("Version must be major.minor.patch with an optional prerelease suffix")
    if match[4] and any(part.isdigit() and len(part) > 1 and part.startswith("0") for part in match[4].split(".")):
        raise ValueError("Numeric prerelease identifiers cannot have leading zeros")
    return version


def release_identity(repository, remote=None, environment=None):
    environment = os.environ if environment is None else environment
    if environment.get("CI_COMMIT_TAG"):
        raise ValueError("Tag-triggered publication is not permitted")
    if "CI_COMMIT_BRANCH" in environment and environment["CI_COMMIT_BRANCH"] != "main":
        raise ValueError("Release CI must target main")
    if "GITHUB_REF" in environment and environment["GITHUB_REF"] != "refs/heads/main":
        raise ValueError("Release CI must target main")
    package = tomllib.loads((repository / "Cargo.toml").read_text())["workspace"]["package"]
    version = validate_version(package["version"])
    commit = git(repository, "rev-parse", "HEAD")
    branch = git(repository, "symbolic-ref", "--quiet", "--short", "HEAD", optional=True)
    main = git(repository, "rev-parse", "--verify", "refs/heads/main", optional=True)
    remote_main = git(repository, "rev-parse", "--verify", "refs/remotes/origin/main", optional=True)
    if branch is not None:
        if branch != "main" or main != commit:
            raise ValueError("Releases must use the current main commit")
    else:
        gitlab_main = environment.get("CI_COMMIT_BRANCH") == "main" and not environment.get("CI_COMMIT_TAG")
        github_main = environment.get("GITHUB_REF") == "refs/heads/main"
        if not (gitlab_main or github_main) or commit not in [main, remote_main]:
            raise ValueError("Detached releases require a verified main CI checkout")
    if git(repository, "status", "--porcelain", "--untracked-files=normal"):
        raise ValueError("Release checkout must be clean")
    tag = "avian-v" + version
    existing = git(repository, "rev-parse", "--verify", f"refs/tags/{tag}^{{commit}}", optional=True)
    if existing is not None and existing != commit:
        raise ValueError(f"Immutable release tag {tag} already identifies another commit")
    if remote:
        lines = git(repository, "ls-remote", "--tags", remote, f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}")
        references = dict((reference, digest) for digest, reference in (line.split() for line in lines.splitlines()))
        remote_commit = references.get(f"refs/tags/{tag}^{{}}", references.get(f"refs/tags/{tag}"))
        if remote_commit is not None and remote_commit != commit:
            raise ValueError(f"Immutable remote tag {tag} already identifies another commit")
        existing = remote_commit or existing
    return {"tag": tag, "version": version, "commit": commit, "already_tagged": existing == commit}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, default=Path(__file__).resolve().parents[1])
    parser.add_argument("--remote", help="Also verify existing tags on this Git remote")
    arguments = parser.parse_args()
    try:
        print(json.dumps(release_identity(arguments.repository, arguments.remote), indent=2))
    except (ValueError, KeyError, OSError, tomllib.TOMLDecodeError) as error:
        parser.exit(1, str(error) + "\n")


if __name__ == "__main__":
    main()
