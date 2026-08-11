# AVIAN pipeline

The AVIAN pipeline uses the root `justfile` as the shared local and CI command
contract. Run the complete required Rust verification locally with:

```sh
just verify
```

Build the ARC radio-plugin container without publishing it with:

```sh
just docker-build
```

## Required pull-request lanes

- **Rust quality and tests** checks formatting, denies Clippy warnings, runs the
  workspace tests (including documentation tests), and builds with the locked
  dependency graph.
- **arc-radio-plugin container** verifies the deployable container can be built
  but does not publish or deploy it.
- **PowerShell bench scripts** parses every Windows radio-bench script without
  changing workstation networking or touching hardware.
- **AVIAN CI required** provides one stable status for repository rules after
  the workflow has proven reliable.

Superseded runs on the same pull request are cancelled. Third-party workflow
actions are pinned to immutable commit SHAs, and workflow permissions are
read-only. Hardware testing, container publication, releases, and deployment
remain outside this initial pipeline.
