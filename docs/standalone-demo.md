# Standalone ARC / AVIAN demo

Run in Windows PowerShell, with no chat session or Docker required:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:USERPROFILE\Desktop\Start-AVIAN-ARC-Demo.ps1"
```

The script prints two URLs. Open them in your preferred browser:

- ARC Fleet: http://127.0.0.1:13000/home/devices
- AVIAN network visualizer: http://127.0.0.1:13211

Add `-OpenBrowser` to explicitly open your default browser. Otherwise nothing takes over the screen.
You can close the launching PowerShell window after READY. Services remain running.

To stop only this launch's services:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:USERPROFILE\Desktop\Stop-AVIAN-ARC-Demo.ps1"
```

## What runs

ARC uses the locally built `dev-bridge.exe --mock`, with mock detections/video enabled, a fresh isolated Fleet state directory, and the installed ArcUI Vite dependencies. AVIAN uses its Rust mesh-sim trace and Node visualizer. The AVIAN scenario includes 200 simulated aircraft plus one ground station; ARC's mock Fleet is a separate, small mock Fleet, not 200 connected aircraft.

**These are separate demos, not an end-to-end CHUD radio integration.** The current ARC mock backend returns `radio_management_api_unavailable` for radio-management routes. No real radio configuration, radio discovery, or flight validation is performed. Voice/AI require additional model assets and are not included. The native backend binary is reused, not rebuilt; this launcher does not update source or claim it reflects newer source edits.

## Prerequisites and recovery

Keep these local folders and their built/installed dependencies:

- `Desktop/arc-uas-main-20260827` with `services/dev-bridge/target/debug/dev-bridge.exe` and `services/arc-ui/node_modules`.
- `Desktop/AVIAN-simulator-validation` with its locally cached Cargo dependencies.
- Node.js and Cargo on PATH.

The script builds mesh-sim with `--offline --locked`; it never installs or downloads missing dependencies. Cold builds may take time. Do not clean the required dependencies immediately before the demo. External map tiles or other browser resources may still need internet; this is not an offline guarantee for every ARC feature.

Run with `-CheckOnly` to check dependency paths and available ports without starting services or building. This does not certify runtime readiness. Ports 13000, 19101, 19100, and 13211 are reserved by default. Conflicts are reported, not killed. Override `-UiPort`, `-BridgePort`, `-TcpPort`, and `-VisualizerPort` if needed.

Logs and process ownership are stored in `Desktop/AVIAN Demo Runtime`. Stop verifies PID, executable path, and process start time; it does not stop unrelated services or close browsers. Repeat launches refuse while a recorded service remains alive: stop first, then start again. Partial startup failures clean up processes started by that launch and retain logs. A reboot ends the services; rerun the launcher afterward.

## Launcher validation (2026-09-08)

Windows PowerShell 5.1 helper checks: 10 passed (`scripts/Test-AvianArcDemo.ps1`). Background startup smoke check: ARC page HTTP 200, proxied Fleet health healthy with two mock devices, AVIAN trace 18 steps and maximum 201 nodes. Duplicate launch was rejected. Stop removed all three owned service processes; no listener remained on the four demo ports. This verifies launch/health/stop, not every interactive ARC capability or physical hardware. Evidence logs: `Desktop/AVIAN Demo Runtime/20260908-094201-7c33ed`.
