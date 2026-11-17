# Release artifacts

Release publication requires exact archives, manifests, and native qualification
evidence for Linux, macOS, Windows, and Android. Compiler checks, API probes,
missing tests, and emulator-only Wi-Fi/cellular checks cannot satisfy the release
gates. Core qualification permits isolated native tests before hardware checks;
release qualification additionally requires CHUD and ARC/Tank interoperability.

Build each artifact from a clean source commit. Render account-specific Linux
service files before archiving; installed file hashes must match the archive.

```sh
python3 tools/release_artifacts.py build \
  --stage /path/to/staged-files --metadata /path/to/metadata.json \
  --output /path/to/linux.tar --manifest /path/to/linux.manifest.json
python3 tools/release_artifacts.py verify \
  --directory /path/to/qualified-artifacts --repository . \
  --source-commit <full-commit> --version 0.1.0
python3 tools/release_bundle.py create /path/to/qualified-artifacts \
  /path/to/qualified.tar --commit <full-commit> --version 0.1.0
```

`release-set.json` contains `schema_version: 1`, `source_commit`, `version`, and
`artifacts`. Each artifact entry contains `target`, `archive`, `manifest`, and
`evidence` relative paths. Native evidence binds the exact archive, installed
files, source commit, engine patches, shared policy sources, and named test gates.
The verifier rejects incomplete sets, changed binaries, links, unsafe paths,
failed gates, and artifacts from a different source revision.

The bundle command prints its SHA-256 and byte count. Store that bundle in this
project's GitLab Generic Package Registry at:

```text
avian-qualified/0.1.0-<full-commit>/<bundle-sha256>.tar
```

Set protected main-pipeline variables `AVIAN_RELEASE_BUNDLE_SHA256` and
`AVIAN_RELEASE_BUNDLE_BYTES` to those printed values. Main CI downloads that exact
bundle using its job token, verifies its size and hash, safely extracts it, and
rechecks every artifact and native gate against the checked-out commit.

Only after verification does publication upload content-addressed assets and
create `avian-v0.1.0`. Existing package content must match exactly; retries never
replace a different file or move a tag. An existing release must have the same
commit and asset links. Non-main branches do not run this pipeline.
