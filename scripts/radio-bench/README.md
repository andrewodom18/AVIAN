# Real-radio bench test

This Windows bench harness restarts the real-hardware-safe ARC/CHUD stack and records what happens when a powered radio is attached by Ethernet. It deliberately does not start AVIAN's radio simulator, a `local-sim` node, or the development MAVLink simulator.

## Run

For the complete operator-led walkthrough—from preflight through individual
identity/certificate checks, a two-radio RF-path check, ARC/CHUD startup, UI
checkpoints, and disconnect/recovery evidence—run:

```powershell
Set-ExecutionPolicy -Scope Process Bypass
& "$env:USERPROFILE\Desktop\AVIAN-arc-main-compat\scripts\radio-bench\Start-FullAvianArcValidation.ps1"
```

The full walkthrough asks explicit Y/N questions for visual ARC checks and
writes every machine result and operator answer beneath
`Desktop\Radio Test Results\full-avian-arc-validation`. It is read-only for
the radios: configuration apply/readback/rollback and automatic authenticated
ARC session attestation are reported as blocked until their remaining safety
and transport gates are complete.

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

Follow the prompts. Leave the radio Ethernet cable unplugged until the monitor requests it. The default radio management address is `10.1.0.2`; multiple addresses may be entered as a comma-separated list.

The monitor distinguishes these milestones:

1. Physical Ethernet carrier and the assigned Ethernet IPv4 address.
2. ICMP and ARP/neighbor-table reachability to each radio management address.
3. AVIAN's native Windows TrellisWare discovery watcher publishing the physical radio to ARC.
4. Real-node and link ingestion through ARC's read-only mesh endpoint.
5. CHUD discovery as a separate configuration-management milestone; it does not gate ARC discovery.

Before the test, configure CHUD with the known bench address:

```yaml
tw_probe: 10.1.0.2
```

If CHUD reports the radio as `auth-failed`, discovery succeeded but protected
management requires an approved TrellisWare client certificate. Configure that
identity in CHUD using `radio_cert_TW_<N>_cert` plus `_key`, a bundled PEM, or
`_p12` plus `_password`. Do not copy the certificate or password into this
repository, ARC configuration, AVIAN arguments, PEAT, or test logs.

The monitor writes a timestamped CSV timeline, before/after Windows network snapshots, and a summary under `Desktop\Radio Test Results`. All checks use local Windows and localhost endpoints, so collection continues if Wi-Fi or the internet connection drops.

Press **Q** in the monitoring window to finish cleanly and generate the summary. `Ctrl+C` also stops the loop, although **Q** is preferred.

If the onboard `Ethernet 2` adapter is disabled, run `Enable-RadioEthernet.ps1`. It requests Windows administrator approval, enables only that adapter, assigns `10.1.0.20/24`, and deliberately installs no gateway so Wi-Fi remains the internet path.
