# ARC emulator PowerShell walkthrough

For today's separate demo checks, use the Desktop testing launcher without switches; see [demo readiness testing](demo-readiness-testing.md). To explicitly run this blocked integration walkthrough through that launcher, add `-RadioIntegration`.

Run `scripts/Start-ArcEmulatorWalkthrough.ps1` from an interactive Windows PowerShell terminal. The guide prompts for Y/Yes, N/No, U/Unknown, S/Skip, or Q/Quit, ignoring capitalization and surrounding whitespace. Unknown records BLOCKED; skipped or interrupted tests cannot produce a passing verdict.

## Current prerequisite

ARC branch `codex/42-drone-radio-association` at `7df0eea82e0cdededd07aa17f851cd3cf0e3bc32` starts mock Fleet without calling `configure_radio_control`. Its `--radio-management-api-url` argument is not forwarded into `run_mock`. Consequently, `/api/radio/networks` is unavailable in mock mode. The earlier plan incorrectly assumed that setting this argument was sufficient.

The guide records this as BLOCKED and stops before frontend or operator onboarding. Completing isolated radio-control initialization in ARC's mock runtime is a separate prerequisite. Do not remove `--mock` to work around it: normal startup may discover real endpoints.

## Usage

```powershell
& "$env:USERPROFILE\Desktop\AVIAN-simulator-validation\scripts\Start-ArcEmulatorWalkthrough.ps1"
```

Automated startup/preflight only (no browser or interactive tests):

```powershell
& "$env:USERPROFILE\Desktop\AVIAN-simulator-validation\scripts\Start-ArcEmulatorWalkthrough.ps1" -PreflightOnly
```

`-ArcRoot` selects the ARC checkout. `-OutputRoot` changes the evidence directory. Ports default to 19101 (bridge HTTP), 19100 (bridge TCP), 13212 (emulator), and 13000 (UI). They must all be unused and distinct. Existing services are never reused or stopped. Required Node, installed ArcUI dependencies, and the ARC debug executable must already exist. The guide does not install, build, fetch, or update applications.

The recorder starts the real ARC binary in mock Fleet mode, the existing AVIAN CHUD emulator with an ephemeral bearer token, and then Vite only after radio-control readiness passes. It assigns a fresh ARC state directory to each run. It displays the UI URL; it does not open or take over a browser. Radio writes remain simulated. At exit, it stops only processes it started, after checking PID and start time. Checkpoints remain available after interruption; hard terminal termination may leave child processes running, and a subsequent run refuses occupied ports.

## Evidence

Each run is saved under `Desktop/Radio Test Results/arc-emulator/<timestamp-id>/`:

- `report.json`: overall verdict, source branches/commits/dirty status, executable and guide SHA-256, limitations, and every answer with its evidence source.
- `steps.csv` and `summary.txt`: readable results updated after every step.
- `radio-control-preflight.json`: actual readiness response, including a blocked result.
- Named checkpoint JSON: Fleet, radio mesh, network journal API, bindings, emulator devices, operations, and mutation ledger.
- `state/`: isolated ARC journal retained for inspection and restart tests.
- Process stdout/stderr logs. The guide supplies only a disposable test bearer credential; never enter real secrets in notes or test radio fields. JSON and answer output redacts that test token.

Test prompts cover the two-radio happy path (one drone and one GCS), same-IP identity, sequential changes, confirmation/readback, injected faults, applying/awaiting-confirmation restart attempts, and definition deletion/removal/archive. Automatic checks compare mutation ledger digests for no-write actions and inspect API state after the happy path and restarts. UI observations are explicitly attributed to the operator.

## Limits and acceptance

This is an attended guide and data recorder, not the complete automated #43 integration harness. The current emulator has two always-present devices, global faults, an immediate apply, and a 504 response pretending to be a timeout. It does not expose plug/unplug control, a real delayed request, a durable reboot, or exact crash barriers. Use Unknown when a state cannot be reached. The guide records the remaining extended matrix as BLOCKED; it cannot by itself close #43 or hardware issue #19.

The default emulator fixtures are one Silvus and one TrellisWare radio sharing an IP. This is a synthetic contract fixture, not a claim that these vendors form one RF mesh or that two physical TW-950s were tested. Network completion must remain `configured_unverified`. Assignment alone does not establish physical attachment or command authority. Simulator provenance lost by ARC normalization must be recorded as a UI finding, not silently waived.

`scripts/Test-ArcEmulatorWalkthrough.ps1` tests recorder behavior, answer classification, safe output paths, redaction, incremental report replacement, ledger mismatch detection, and URL rejection without launching ARC or asking operator questions.
