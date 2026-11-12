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
just ci-contracts
```

CI runs only on pushes to `main`. Pushes to `dev` deliberately do not trigger
GitHub Actions, to conserve Actions minutes; run verification locally on `dev`.

## Required lanes

- **Rust quality and tests** checks formatting, denies Clippy warnings, runs the
  workspace tests (including documentation tests), and builds with the locked
  dependency graph.
- **Dependency audit** rejects RustSec vulnerabilities except narrow, documented
  exceptions with explicit exposure rationale and reevaluation conditions. Both
  the local and hosted command also reject `unsound` advisories; a warning-only
  success from the default cargo-audit behavior is not sufficient. Unmaintained
  package warnings remain visible and require separate disposition.
- **Feature and release build** tests all workspace features and compiles the
  locked release configuration.
- **Coverage floor** prevents line coverage from falling below 75 percent. The
  floor stays below the measured workspace baseline of more than 79 percent.
- **Vendor identifier mutation test** requires the topic- and record-safe radio
  identifier contract to catch every viable generated mutant in that bounded
  scope. It runs a locked, unmodified build/test baseline, then checks the actual
  generated/outcome reports. Empty, incomplete, duplicate, missing-baseline,
  unviable-only, missed or timed-out runs fail qualification. Unviable counts
  remain visible; they cannot substitute for at least one caught viable mutant.
- **arc-radio-plugin container** builds from digest-pinned images, verifies the
  runtime uses the non-root `avian` account, and smoke-tests the command entry.
- **PowerShell bench quality** parses and statically analyzes every bench script,
  then runs mocked tests without changing workstation networking or touching
  hardware.
- **Emulator lifecycle and schema contracts** installs the simulator's locked
  test dependencies and exercises its fast, non-hardware contracts. This lane is
  included in both the aggregate dependencies and its success assertions.
- **RF planning application** installs locked Node dependencies, builds the
  application, runs its rendered-page and calculation contracts, and checks
  lint and TypeScript types.
- **AVIAN CI required** provides one stable status for repository rules after
  all required lanes succeed. Repository rules should require only this status,
  named `ci / AVIAN CI required`, so future informational jobs do not become
  merge blockers accidentally.

Superseded runs on the same branch are cancelled. Third-party workflow
actions and container bases are pinned to immutable commit SHAs or digests, and
workflow permissions are read-only. Hardware testing, container-image scanner
selection, container publication, releases, and deployment remain outside this
pipeline.

## Aggregate regression checks

`just ci-contracts` is included in `just verify`. It executes the actual literal
aggregate shell from the workflow: all-success must pass, and each required
lane's failure, cancellation, skip or missing result must fail. It also checks
the dependency/result wiring, the stable aggregate identity and the mutation
recipe's mandatory baseline. The test reader deliberately supports this
workflow's explicit literal format rather than attempting to parse arbitrary
YAML; changes to that structure require corresponding test review.
The shell checks require Node.js and Bash (for example Git Bash on Windows).
Workflow/recipe CRLF is normalized in memory; tracked files are not rewritten.
WSL execution is Linux evidence, not native Windows qualification.

## Current delivery boundaries

Keep the main-push-only trigger policy. Local results do not automatically
create a GitHub required check or constitute maintainer approval. Record the
exact candidate commit/tree, lockfile hash, tool/database revisions, platform,
commands, exits and logs. An approved premerge verification route and actual
repository enforcement still need maintainer agreement; this change does not
configure rules or enable PR/manual triggers. Verify the actual resulting main
revision after reviewed delivery.

Issue #41 reuses PR49's mutation-baseline and simulator aggregate fixes and
PR48's focused identity tests. Reported Windows integration/aggregate results
are prior evidence, not results rerun or source recovered on this Mac. The
November 12 candidate adds current advisory fixes and durable aggregate tests
without adopting the unrelated stationary-runtime integration. DADS execution
and mirror work are deferred at Joseph's November 12 direction while awaiting
others; they are not a prerequisite for this software work or claimed complete.
