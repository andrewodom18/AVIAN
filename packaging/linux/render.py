"""Render Linux service definitions for an explicitly selected existing account."""

import argparse
import grp
import hashlib
import json
from pathlib import Path
import pwd
import re

SAFE_ACCOUNT = re.compile(r"[a-z_][a-z0-9_.-]{0,31}")
SAFE_INSTANCE = re.compile(r"[a-z0-9][a-z0-9_-]{0,31}")


def render(username, instance, configuration, destination):
    if not SAFE_ACCOUNT.fullmatch(username) or not SAFE_INSTANCE.fullmatch(instance):
        raise ValueError(
            "Choose an ordinary existing account and a simple instance name"
        )
    account = pwd.getpwnam(username)
    group = grp.getgrgid(account.pw_gid)
    if account.pw_uid <= 0 or account.pw_gid <= 0:
        raise ValueError("The manager account and primary group must be unprivileged")
    if configuration.get("allowed_uid") != account.pw_uid:
        raise ValueError(
            "The helper allowed_uid must match the selected manager account"
        )
    if configuration.get("runtime_directory") != f"/run/avian/{instance}":
        raise ValueError("The helper runtime directory must match the service instance")
    destination = Path(destination)
    if destination.exists() or destination.is_symlink():
        raise ValueError("Render into a new staging directory")
    destination.mkdir(mode=0o700)
    values = {"@AVIAN_UID@": str(account.pw_uid), "@AVIAN_GID@": str(account.pw_gid)}
    hashes = {}
    for template in sorted((Path(__file__).resolve().parent / "systemd").iterdir()):
        content = template.read_text()
        for marker, value in values.items():
            content = content.replace(marker, value)
        if re.search(r"@AVIAN_[A-Z_]+@", content):
            raise ValueError("An unresolved service setting remains")
        name = template.name.replace("@.", f"@{instance}.")
        target = destination / name
        target.write_text(content)
        target.chmod(0o644)
        hashes[name] = hashlib.sha256(target.read_bytes()).hexdigest()
    metadata = {
        "account": username,
        "uid": account.pw_uid,
        "group": group.gr_name,
        "gid": account.pw_gid,
        "instance": instance,
        "service_sha256": hashes,
        "helper_configuration_sha256": hashlib.sha256(
            json.dumps(configuration, sort_keys=True, separators=(",", ":")).encode()
        ).hexdigest(),
        "services_enabled": False,
    }
    (destination / "service-account.json").write_text(
        json.dumps(metadata, indent=2) + "\n"
    )
    return metadata


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--user", required=True)
    parser.add_argument("--instance", required=True)
    parser.add_argument("--helper-config", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        metadata = render(
            arguments.user,
            arguments.instance,
            json.loads(arguments.helper_config.read_text()),
            arguments.output,
        )
    except (OSError, ValueError, KeyError) as error:
        parser.exit(1, str(error) + "\n")
    print(json.dumps(metadata, indent=2))


if __name__ == "__main__":
    main()
