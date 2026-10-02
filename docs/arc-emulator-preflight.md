# Portable ARC emulator preflight

`scripts/Run-ArcEmulatorPreflight.mjs` checks the startup prerequisite for AVIAN #43. It launches an owned CHUD emulator and an explicitly supplied ARC executable in mock mode, using disposable state, generated simulation credentials and three dedicated loopback ports. It performs read-only health and radio-network requests and verifies that the emulator mutation ledger is unchanged.

This is not the automated apply/readback, browser, PEAT or hardware qualification suite. A PASS means only that the startup prerequisite passed. Every report sets `workflow_qualified: false` and `hardware_validation: false`.

## Usage

Use Node 20.3 or later and a previously built, reviewed ARC executable supporting the required mock flags. This command does not build, fetch, install, enable mutations or select non-mock startup. Check the current task's implementation boundary before running ARC qualification; office-only work must remain held until specifically authorized here.

```sh
node scripts/Run-ArcEmulatorPreflight.mjs \
  --arc-root /absolute/path/to/arc-checkout \
  --bridge-exe /absolute/path/to/dev-bridge \
  --output-root /absolute/path/outside/repositories
```

Optional arguments are `--bridge-port` (19101), `--command-port` (19100), `--emulator-port` (13212), and `--timeout-ms` (15000; maximum 60000). Ports must be distinct, unprivileged and unused. The runner never attaches to or terminates an existing service. Its direct HTTP client does not follow redirects or consult proxy settings. Only the child processes it launches receive termination signals.

The child environment is built from an allowlist of operating-system executable settings plus disposable home/config/temp/state paths. It does not inherit production CHUD credentials, Zenoh configuration, backend settings or Node preload options. Radio mutations are explicitly disabled. No shell evaluates executable arguments.

## Evidence and failures

Each unique run directory contains an incrementally replaced `report.json`, bounded and redacted child logs, and disposable state. Reports include source commits/dirty state, executable and runner hashes, assertions, the before/after ledger and child termination results. The executable hash identifies the binary tested; reading a checkout does not prove it built that binary. Evidence directories inside a source checkout, including through symlinks, are rejected.

Exit codes are 0 for preflight PASS, 2 for BLOCKED and 1 for FAIL. A healthy bridge whose radio-network API is unavailable produces BLOCKED; the runner does not fall back to normal startup. Unexpected writes, occupied ports, child failures, timeout, cancellation and cleanup failure cannot pass. SIGINT and SIGTERM produce failure evidence and owned-process cleanup. A forced OS kill cannot guarantee cleanup; the next run refuses occupied ports.

The current #42 ARC source has a known mock radio-control construction gap. Fixing that prerequisite is ARC backend work. The office-only OpenSpec boundary currently prevents implementing that fix here. The AVIAN runner can be tested independently without modifying or running that ticket's implementation.

## Tests

`npm run --prefix simulators/mesh-operations test:fast` includes the runner suite. It uses the real AVIAN emulator plus a deliberately labeled fixture bridge to exercise process handling, unavailable/malformed readiness, redirection refusal, occupied ports, cancellation, output isolation, credential redaction and unexpected-write detection. These tests qualify runner behavior only; fixture-backed reports explicitly set `fixture_bridge: true` and are not actual ARC integration evidence.

The older PowerShell walkthrough remains available for its attended Windows workflow. Its historical limitations should be read alongside the newer emulator lifecycle implementation; this portable prerequisite does not replace its operator/browser acceptance steps.
