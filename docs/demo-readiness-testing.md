# Demo testing for 2026-09-08

The Desktop `Start-AVIAN-ARC-Simulator-Test.ps1` now defaults to the separate demo-readiness walkthrough. It does not stop at the known ARC mock-radio integration gap. The original integration walkthrough remains available with `-RadioIntegration` and still fails closed when that prerequisite is missing.

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:USERPROFILE\Desktop\Start-AVIAN-ARC-Simulator-Test.ps1"
```

Leave radios disconnected. The guide starts its own mock ARC backend, ArcUI and AVIAN visualizer on dedicated ports. Open the URLs it prints (ARC 13001, AVIAN 13221), not existing demo tabs. Nothing opens a browser or changes networking/certificates/hardware. It reuses locally installed dependencies and performs the launcher's offline mesh-sim build; no source updates or dependency downloads.

Answer Y, N, U (unknown), S (skip), or Q (quit). Case and surrounding spaces do not matter. Unknown is BLOCKED, skip/quit cannot pass, and unavailable prerequisite UI checks skip dependent interactions. Each response immediately updates JSON, CSV and text evidence under `Desktop/Radio Test Results/demo-readiness/<run>/`.

Checks cover ARC's two connected mock Fleet devices plus its separate unauthorized discovery fixture, active-device selection, AVIAN discovery sequence, validation without apply, simulated configuration/readback, invalid-frequency rejection, 200-aircraft/one-GCS topology loss and recovery, About documentation and presentation quality. Do not authorize/pair the third fixture. API checkpoints accompany operator observations. Automatic scale checks inspect trace consistency; they are not independent performance or hardware validation. A demo PASS never means real CHUD integration or hardware testing passed.

`-PreflightOnly` performs startup/read-only API checks and records the attended checks as skipped. `-KeepRunning` leaves only this test's services available afterward and prints the exact stop command. By default, the guide stops its own processes and retains evidence; other dev apps are untouched.

As checked September 8, Windows discovery/intake, CHUD promotion of external observations, duplicate-IP onboarding and TrellisWare RF peers remain blockers. Newly merged CHUD API authentication is not evidence that those features exist. Real-radio testing and full integration remain outside this demo guide.

## Recorder verification

Windows PowerShell 5.1: 22 readiness helper checks and the existing 23 recorder checks passed. Background preflight verified the actual Fleet fixture (two connected authorized mocks plus one unauthorized discovery fixture), simulation provenance, and the 201 -> 180 -> 201 node trace. Two degraded survivors count toward the 180 online nodes. The preflight verdict remains INCOMPLETE because attended UI checks were skipped. Test services were stopped and evidence retained in `Desktop/Radio Test Results/demo-readiness/20260908-103143-4513ce`.
# Build lifecycle update — 2026-09-08

The default demo testing guide now compiles ARC and AVIAN into a unique
`%LOCALAPPDATA%\AVIAN-Test-Builds\test-...` directory rather than requiring
an old repository `target` binary. Its finally cleanup stops the owned demo
processes and deletes that directory. Results/build logs remain with the run.
`KeepRunning` no longer keeps a testing session alive. The standalone demo
launcher remains available for presentations; its Stop script now also removes
the owned builds. Offline builds require already-cached dependencies; a missing
dependency is a build blocker, not a reason to fall back to an old binary.
For a forcibly closed terminal, run the demo Stop script with that run's
`-StateRoot`; cleanup needs the same privileges used to launch the demo.
