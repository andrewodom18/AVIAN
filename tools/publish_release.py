"""Publish an immutable release from a verified main pipeline."""

import json
import os
import ssl
from pathlib import Path
import urllib.error
import urllib.parse
import urllib.request

from release_version import release_identity


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


def publish(repository, environment=None, request_json=None):
    environment = os.environ if environment is None else environment
    if environment.get("CI_COMMIT_BRANCH") != "main" or environment.get("CI_PIPELINE_SOURCE") not in {"push", "web"} or environment.get("CI_COMMIT_TAG"):
        raise ValueError("Release publication requires a main push or manual main pipeline")
    identity = release_identity(repository, remote="origin", environment=environment)
    if environment.get("CI_COMMIT_SHA") != identity["commit"]:
        raise ValueError("Pipeline commit differs from the checked-out main commit")
    api_url = environment.get("CI_API_V4_URL", "")
    if api_url != "https://repo.bespin.cce.af.mil/api/v4":
        raise ValueError("Unexpected GitLab API endpoint")
    project = environment.get("CI_PROJECT_ID", "")
    token = environment.get("CI_JOB_TOKEN", "")
    if not project.isdigit() or not token:
        raise ValueError("Missing project identity or CI job token")
    base = f"{api_url}/projects/{project}/releases"

    def send(method, url, body=None):
        request = urllib.request.Request(url, method=method, headers={"JOB-TOKEN": token, "Content-Type": "application/json"}, data=None if body is None else json.dumps(body).encode())
        try:
            context = ssl.create_default_context(cafile=environment.get("CI_SERVER_TLS_CA_FILE") or None)
            with urllib.request.build_opener(NoRedirect, urllib.request.HTTPSHandler(context=context)).open(request, timeout=30) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            if method == "GET" and error.code == 404:
                return None
            raise ValueError(f"GitLab release API returned HTTP {error.code}") from None

    send = send if request_json is None else request_json
    existing = send("GET", base + "/" + urllib.parse.quote(identity["tag"], safe=""))
    if existing is not None:
        if existing.get("commit", {}).get("id") != identity["commit"]:
            raise ValueError("Existing release belongs to a different commit")
        return identity
    release = send("POST", base, {
        "name": f"AVIAN {identity['version']}",
        "tag_name": identity["tag"],
        "ref": identity["commit"],
        "description": f"AVIAN {identity['version']}\n\nSource commit: `{identity['commit']}`.",
    })
    if release.get("commit", {}).get("id") != identity["commit"]:
        raise ValueError("Published release commit did not match the verified commit")
    return identity


if __name__ == "__main__":
    try:
        print(json.dumps(publish(Path(__file__).resolve().parents[1]), indent=2))
    except (ValueError, OSError, urllib.error.URLError) as error:
        raise SystemExit(str(error))
