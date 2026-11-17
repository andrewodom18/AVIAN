"""Upload qualified files to content-addressed package URLs without overwriting them."""

import hashlib
import io
import json
from pathlib import Path
import ssl
import urllib.error
import urllib.parse
import urllib.request

import release_artifacts


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, file, code, message, headers, new_url):
        return None


def encoded(value):
    return (
        json.dumps(value, sort_keys=True, indent=2, ensure_ascii=True) + "\n"
    ).encode()


def prepare_files(directory, verified, package_base):
    directory = Path(directory)
    index = json.loads(
        release_artifacts._regular(directory, "release-set.json").read_bytes()
    )
    if (
        index["source_commit"] != verified["source_commit"]
        or index["version"] != verified["version"]
    ):
        raise ValueError("Release files changed after qualification")
    by_target = {entry["target"]: entry for entry in index["artifacts"]}
    files = []
    for artifact in verified["artifacts"]:
        entry = by_target[artifact["target"]]
        archive = release_artifacts._regular(directory, artifact["archive"])
        manifest = json.loads(
            release_artifacts._regular(directory, entry["manifest"]).read_bytes()
        )
        evidence = json.loads(
            release_artifacts._regular(directory, entry["evidence"]).read_bytes()
        )
        release_artifacts.verify_native_evidence(
            evidence, manifest, artifact["archive_sha256"]
        )
        if hashlib.sha256(encoded(manifest)).hexdigest() != artifact["manifest_sha256"]:
            raise ValueError("Artifact manifest changed after verification")
        files.extend(
            [
                {
                    "name": artifact["target"] + " package",
                    "path": archive,
                    "body": None,
                    "sha256": artifact["archive_sha256"],
                    "suffix": ".tar",
                },
                {
                    "name": artifact["target"] + " manifest",
                    "path": None,
                    "body": encoded(manifest),
                    "sha256": hashlib.sha256(encoded(manifest)).hexdigest(),
                    "suffix": ".manifest.json",
                },
                {
                    "name": artifact["target"] + " native evidence",
                    "path": None,
                    "body": encoded(evidence),
                    "sha256": hashlib.sha256(encoded(evidence)).hexdigest(),
                    "suffix": ".evidence.json",
                },
            ]
        )
    files.append(
        {
            "name": "release artifact index",
            "path": None,
            "body": encoded(index),
            "sha256": hashlib.sha256(encoded(index)).hexdigest(),
            "suffix": ".release-set.json",
        }
    )
    for file in files:
        filename = file["sha256"] + file["suffix"]
        file["url"] = package_base + "/" + filename
        file["size"] = (
            file["path"].stat().st_size if file["path"] else len(file["body"])
        )
    return files


def asset_links(files):
    return [
        {"name": file["name"], "url": file["url"], "link_type": "package"}
        for file in files
    ]


def check_existing_links(release, files):
    existing = {
        (link.get("name"), link.get("url"))
        for link in release.get("assets", {}).get("links", [])
    }
    expected = {(link["name"], link["url"]) for link in asset_links(files)}
    if existing != expected:
        raise ValueError("Existing release assets differ from qualified artifacts")


def synchronize(files, token, *, cafile=None, transfer=None, allow_upload=True):
    opener = urllib.request.build_opener(
        NoRedirect(),
        urllib.request.HTTPSHandler(context=ssl.create_default_context(cafile=cafile)),
    )

    def remote(method, file):
        headers = {"JOB-TOKEN": token}
        if method == "GET":
            request = urllib.request.Request(file["url"], headers=headers)
            try:
                with opener.open(request, timeout=30) as response:
                    digest = hashlib.sha256()
                    count = 0
                    while chunk := response.read(1024 * 1024):
                        count += len(chunk)
                        if count > file["size"]:
                            raise ValueError(
                                "Remote package exceeds the qualified size"
                            )
                        digest.update(chunk)
                    return digest.hexdigest()
            except urllib.error.HTTPError as problem:
                if problem.code == 404:
                    return None
                raise ValueError(
                    f"Package download returned HTTP {problem.code}"
                ) from None
        # Content length bounds the upload; no credentials enter URLs, argv, or logs.
        headers["Content-Length"] = str(file["size"])
        headers["Content-Type"] = "application/octet-stream"
        stream = file["path"].open("rb") if file["path"] else io.BytesIO(file["body"])
        with stream:
            request = urllib.request.Request(
                file["url"], method="PUT", headers=headers, data=stream
            )
            with opener.open(request, timeout=60) as response:
                if response.status != 201:
                    raise ValueError("Package upload was not accepted")
        return None

    send = remote if transfer is None else transfer
    for file in files:
        endpoint = urllib.parse.urlparse(file["url"])
        if (
            endpoint.scheme != "https"
            or endpoint.netloc != "repo.bespin.cce.af.mil"
            or not endpoint.path.startswith("/api/v4/projects/")
            or "/packages/generic/avian/" not in endpoint.path
            or endpoint.query
            or endpoint.fragment
        ):
            raise ValueError("Unexpected package upload endpoint")
        digest = hashlib.sha256()
        source = file["path"].open("rb") if file["path"] else io.BytesIO(file["body"])
        with source:
            while chunk := source.read(1024 * 1024):
                digest.update(chunk)
        digest = digest.hexdigest()
        if digest != file["sha256"]:
            raise ValueError("Qualified package changed before publication")
        present = send("GET", file)
        if present is not None and present != file["sha256"]:
            raise ValueError("Existing package content differs; refusing overwrite")
        if present is None:
            if not allow_upload:
                raise ValueError("Existing release has a missing qualified package")
            send("PUT", file)
            if send("GET", file) != file["sha256"]:
                raise ValueError("Uploaded package hash differs from qualification")
