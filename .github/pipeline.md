# AVIAN pipeline

The AVIAN pipeline uses the root `justfile` as the shared local and CI command
contract. Run the complete required Rust verification locally with:

```sh
just verify
```

Build the ARC radio-plugin container without publishing it with:

```sh
just docker-smoke
```

Run the additional required gates individually with:

```sh
just audit
just feature-check
just coverage
just mutate-radio
just powershell-quality
```

## Required pull-request lanes

- **Rust quality and tests** checks formatting, denies Clippy warnings, runs the
  workspace tests (including documentation tests), and builds with the locked
  dependency graph.
- **Dependency audit** rejects RustSec vulnerabilities except narrow, documented
  exceptions with explicit exposure rationale and reevaluation conditions.
- **Feature and release build** tests all workspace features and compiles the
  locked release configuration.
- **Coverage floor** prevents line coverage from falling below 75 percent. The
  floor is based on the measured 77.48-percent workspace baseline.
- **Vendor identifier mutation test** requires the topic- and record-safe radio
  identifier contract to catch every viable generated mutant in that bounded
  scope.
- **arc-radio-plugin container** builds from digest-pinned images, verifies the
  runtime uses the non-root `avian` account, and smoke-tests the command entry.
- **PowerShell bench quality** parses and statically analyzes every bench script,
  then runs mocked tests without changing workstation networking or touching
  hardware.
- **AVIAN CI required** provides one stable status for repository rules after
  all required lanes succeed. Repository rules should require only this status,
  named `ci / AVIAN CI required`, so future informational jobs do not become
  merge blockers accidentally.

Superseded runs on the same pull request are cancelled. Third-party workflow
actions and container bases are pinned to immutable commit SHAs or digests, and
workflow permissions are read-only. Hardware testing, container-image scanner
selection, container publication, releases, and deployment remain outside this
pipeline.
