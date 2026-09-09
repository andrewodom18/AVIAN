# Two-TW950 live test

This is the real Windows/CHUD/AVIAN/ARC bench test, not the simulator guide.
Keep both radios powered off until prompted. Use approved antennas or loads
and the authorized bench RF setup. No flight controller/companion is attached
during radio-only Fleet checks. Do not issue flight commands.

## Run from PowerShell

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "$env:USERPROFILE\Desktop\Start-AVIAN-TW950-Live-Test.ps1"
```

Normal runs now always build fresh, run the live test, then clean up in a
`finally` block, including failed startup and operator quit. `-StartApps` is
accepted for old commands but is no longer necessary. Type BUILD to begin.
Allow potentially lengthy compilation with both radios OFF and internet available.
The recorder itself is read-only; its session wrapper owns app startup/shutdown.
Neither changes host networking, certificates, firmware, or radio settings.
An existing bench or occupied port is not silently adopted or destroyed.

If your old bench was started as Administrator, run this one-time cleanup from
Administrator PowerShell before the new test:

```powershell
& "$env:USERPROFILE\Desktop\Clear-AVIAN-Old-Test-Apps.ps1"
```

The new session builds ARC containers with unique tags in a dedicated Docker
builder, compiles AVIAN into `%LOCALAPPDATA%\AVIAN-Test-Builds\test-...`, runs
the radio UI regression tests, and builds/previews the actual UI bundle.
It does not reuse an old compiled AVIAN executable. On exit it stops owned
processes, removes owned containers/images and the dedicated builder/cache,
and removes that session's build directory. Fleet/archive volumes, installed
CHUD/certificates, dependency downloads, unrelated Docker cache and evidence
are preserved. CHUD is stopped afterward only if this session started it.

Build logs and the ownership manifest are saved under
`Desktop\Radio Test Results\test-sessions\test-...`. If the terminal is forcibly
closed or the PC loses power, `finally` cannot be guaranteed to execute. Use
the printed recovery command with the same privileges used to run the test:

```powershell
& "$env:USERPROFILE\Desktop\Stop-AVIAN-Test-Session.ps1" -ManifestPath "<full path to session.json>"
```

`-PreflightOnly` is a read-only check of an already-running stack and does not
build, connect to radios, or create an owned session.

Optional switches: `-PreflightOnly`, `-EthernetAdapter 'Ethernet 2'`,
`-ObserveSeconds 60`, `-PromptForChudApiKey`. The API key is entered hidden,
kept in memory, and never put in command arguments or evidence files.
401/403 from CHUD is an API authorization problem, not proof of bad radio
certificates. Do not send certificate passwords or API keys in test notes.

## Walkthrough

1. Verify real runtime/image IDs, absence of detected mock flags, ARC radio-only
   transport, APIs, and coordination health. CHUD failure is recorded separately
   so Windows observations can still be collected; it never grants authority.
2. With both radios off, check zero online radios and truthful offline Fleet.
3. Read Radio 1's Ethernet MAC from its label and enter its management IP.
   Power/connect only Radio 1. Record selected-interface routing, ICMP, and a
   reachable neighbor matching that MAC. Stale cache entries cannot pass.
4. Record AVIAN diagnostic, CHUD inventory, ARC mesh and Fleet timelines.
   A unique eligible CHUD record permits a read-only CHUD configuration snapshot.
   Open configuration in ARC only to read; do not apply/save. Missing authority
   must keep configuration blocked. Radio discovery must not create a drone.
5. Disconnect/power off Radio 1; record expiry. Repeat with Radio 2 alone.
   Different MAC labels do not prove concurrent duplicate-IP support.
6. Offer over-air testing only if both radios already have approved matching RF
   settings AND distinct management IPs or verified vendor address isolation.
   Same factory IP may collide across an RF bridge even with one Ethernet cable.
   If uncertain, answer U and skip the both-powered phase.
7. Radio 1 remains Ethernet-connected. Radio 2 has power only. Capture peer-on,
   peer-off and peer-return timelines. Check real identity, measured RF-source
   evidence, loss/return, and no duplicates. A remembered node or PEAT path is
   not proof of a live measured RF peer. These visual claims remain operator
   observations until API provenance/freshness evidence is reviewed.
8. Power off/disconnect both radios and check final offline truthfulness.

The managed run also asks you to verify the latest UI changes on each radio:
only Management IP/MAC/Interface in Live radio details, no disconnected-peers
checkbox, automatic appearance/disappearance without clicking refresh, a
truthful CHUD pending label for discovery-only nodes, and no loading flashes
when collapsing/reopening the panel. Automated UI tests verify the one-second
polling cadence and protection against overlapping slow requests. Discovery
and stale expiry still depend on the upstream observation interval; one-second
UI polling is not a promise of one-second hardware detection.

Answer Y/N/U (unknown)/S (skip)/Q (quit). Typed confirmations ignore case and
extra spaces. Unknown prerequisites do not become fabricated failures. An
unexpected interruption retains previously saved checkpoints; power off radios
manually when safe. The managed Desktop wrapper cleans its applications and
builds after the recorder exits. Invoking the low-level recorder directly does
not adopt ownership of independently running applications.

## Evidence and scope

Results: `Desktop\Radio Test Results\tw950-live\<timestamp-id>`, containing
`report.json`, `summary.txt`, `steps.csv`, identities, runtime image IDs, network
observations and API timelines. Configuration values and secret-named fields
are redacted. Evidence still contains MAC/IP inventory; review before sharing.

PASS is scoped to listed checks. Skipped tests make the verdict INCOMPLETE;
missing prerequisites make it BLOCKED; observed failed checks make it FAIL.
CHUD snapshot HTTP 200 is required in addition to an eligible inventory state
before claiming configuration reads work. A native Windows observation alone
never makes a radio managed. No automatic enrollment, certificate import,
radio apply/persist/rollback, duplicate-IP remediation, flight, or 200-radio
hardware validation is performed. A separate approved write/recovery test
comes after successful real CHUD snapshot and ARC configuration-read gates.

The recorder's helper tests use fixtures only:

```powershell
& "$env:USERPROFILE\Desktop\AVIAN-arc-main-compat\scripts\radio-bench\Test-Tw950LiveValidation.ps1"
```
