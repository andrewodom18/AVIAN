# Real-radio bench test

## Current entry point (build, test, cleanup)

Use `Desktop\Start-AVIAN-TW950-Live-Test.ps1` for new testing sessions.
It creates fresh disposable builds, runs the live/UI regression walkthrough,
then destroys its owned runtime/builds while retaining results, certificates
and saved Fleet data. See [the current guide](../../docs/tw950-live-testing.md).
The older standalone launchers and recorders below are retained for diagnostics;
they are not the managed build-and-cleanup entry point.

This Windows bench harness restarts the real-hardware-safe ARC/CHUD stack and records what happens when a powered radio is attached by Ethernet. It deliberately does not start AVIAN's radio simulator, a `local-sim` node, or the development MAVLink simulator.

## Run

For the complete operator-led walkthrough—from preflight through individual
identity/certificate checks, a two-radio RF-path check, ARC/CHUD startup, UI
checkpoints, and disconnect/recovery evidence—run:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
& "$env:USERPROFILE\Desktop\AVIAN\scripts\radio-bench\Start-FullAvianArcValidation.ps1"
```

The full walkthrough asks explicit Y/N questions for visual ARC checks and
writes every machine result and operator answer beneath
`Desktop\Radio Test Results\full-avian-arc-validation`. It is read-only for
the radios: configuration apply/readback/rollback and automatic authenticated
ARC session attestation are reported as blocked until their remaining safety
and transport gates are complete. The compatibility PKCS#12 identity is
converted to a restricted temporary PEM only for the read; cleanup is verified
before the walkthrough continues. A derived IPv6 link-local address is treated
as a candidate and must answer directly before the RF-path monitor runs.
The default client identity is read from
`Desktop\Work Docs\Security\OEM Certificates\oemcert-compat.p12`; it is never
copied into the repository or written to the evidence directory.

For the guided two-radio workflow—including separate reachability checks,
blank-password PKCS#12 authentication attempts, evidence capture, and an
optional handoff to the full ARC/CHUD stack—run:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
& "$env:USERPROFILE\Desktop\AVIAN\scripts\radio-bench\Start-GuidedRadioValidation.ps1"
```

The guided workflow is read-only and requires an exact `READY` safety
confirmation before power-up and `DISCONNECTED` between radios. It never
connects both factory-address radios at once and does not change radio or host
network configuration. By default it uses `Desktop\oemcert-compat.p12` with a
blank password and a lab-only server-certificate override. Supply
`-CaCertificatePem <path>` when the approved radio CA is available.
Use `-PreflightOnly` to verify the adapter, bench address, certificate presence,
and probe build without contacting a radio.

To start only the existing full ARC/CHUD evidence stack, open PowerShell and
run:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
& "$env:USERPROFILE\Desktop\AVIAN\scripts\radio-bench\Start-RadioBenchTest.ps1"
```

Follow the prompts. Leave the radio Ethernet cable unplugged until the monitor
requests it. The launcher verifies every CHUD bind source before starting ARC,
waits for the CHUD device API, and rebuilds the ARC dev-bridge from the selected
ARC checkout so an older Docker image cannot silently run against newer CLI
arguments. It also applies `docker-compose.real-hardware.yml`, which removes
ARC's development-only `comms:7447` fleet endpoint so the local Docker heartbeat
cannot appear as a connected drone during physical-radio testing. Physical fleet
records use a dedicated persistent state volume and therefore remain saved when
the bench containers restart. Keep at least 12 GiB free on the Windows system drive for that
release build. The default radio management address is `10.1.0.2`; multiple
addresses may be entered as a comma-separated list.

The monitor distinguishes these milestones:

1. Physical Ethernet carrier and the assigned Ethernet IPv4 address.
2. ICMP and ARP/neighbor-table reachability to each radio management address.
3. AVIAN's native Windows TrellisWare diagnostic watcher publishing a non-authoritative observation to ARC.
4. Real-node and link ingestion through ARC's read-only mesh endpoint.
5. CHUD discovery as the separate authoritative configuration-management milestone. The diagnostic watcher does not satisfy this gate.

Only if the running CHUD build implements the setting, configure the known bench address:

```yaml
tw_probe: 10.1.0.2
```

If CHUD reports the radio as `auth-failed`, discovery succeeded but the supplied
management authentication was rejected. Inspect CHUD's detailed TLS/API evidence
before deciding whether a client certificate, different credential, or firmware
configuration is required. Do not copy credentials into this repository, ARC
configuration, AVIAN arguments, PEAT, or test logs.

The monitor writes a timestamped CSV timeline, before/after Windows network snapshots, and a summary under `Desktop\Radio Test Results`. All checks use local Windows and localhost endpoints, so collection continues if Wi-Fi or the internet connection drops.

Press **Q** in the monitoring window to finish cleanly and generate the summary. `Ctrl+C` also stops the loop, although **Q** is preferred.

## CHUD-authority regression suite

After the applications are already running, use the guided authority suite to
validate the disconnected baseline, two radios sharing a factory IP, v2 AVIAN
diagnostic provenance, CHUD configuration gating, disconnect expiry, Fleet
persistence, and measured-versus-logical topology:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
& "$env:USERPROFILE\Desktop\AVIAN\scripts\radio-bench\Start-ChudAuthorityValidation.ps1"
```

The default run is read-only and never opens a browser or changes host
networking. Add `-AllowManualConfigurationTest` only after CHUD reports the
selected MAC confirmed/connected with an available driver and the approved
recovery/baseline procedure is ready. Even with that switch, the script does
not issue a write itself; it records the operator's explicitly confirmed CHUD
transaction, readback, and restoration results.

Use `-PreflightOnly` to verify the running applications, local APIs, simulator
exclusion, and existing Ethernet address without connecting either radio.

Each run builds the current AVIAN plugin and ARC service images and writes a
bench manifest containing the ARC and AVIAN commits, plugin SHA-256, Compose
file SHA-256, and Link Manager image ID. Run `Stop-RadioBenchTest.ps1` after a
test to stop only the recorded AVIAN process and the dedicated Link Manager
container.

If the onboard `Ethernet 2` adapter is disabled, run `Enable-RadioEthernet.ps1`.
It requests Windows administrator approval, records a CLIXML recovery snapshot,
preserves existing addresses and routes, and adds `10.1.0.20/24`. It refuses to
continue when the adapter already has a default route. Use
`Restore-RadioEthernet.ps1 -SnapshotPath <path>` to remove the added bench
address and restore the recorded DHCP, metric, addresses, and disabled state.
