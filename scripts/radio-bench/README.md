# Real-radio bench test

This Windows bench harness restarts the real-hardware-safe ARC/CHUD stack and records what happens when a powered radio is attached by Ethernet. It deliberately does not start AVIAN's radio simulator, a `local-sim` node, or the development MAVLink simulator.

## Run

Open PowerShell and run:

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

If CHUD reports the radio as `auth-failed`, discovery succeeded but the supplied
management authentication was rejected. Inspect CHUD's detailed TLS/API evidence
before deciding whether a client certificate, different credential, or firmware
configuration is required. Do not copy credentials into this repository, ARC
configuration, AVIAN arguments, PEAT, or test logs.

The monitor writes a timestamped CSV timeline, before/after Windows network snapshots, and a summary under `Desktop\Radio Test Results`. All checks use local Windows and localhost endpoints, so collection continues if Wi-Fi or the internet connection drops.

Press **Q** in the monitoring window to finish cleanly and generate the summary. `Ctrl+C` also stops the loop, although **Q** is preferred.

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
