# AVIAN simulators

This directory contains AVIAN's stakeholder-facing and engineering simulation
projects. Simulators must remain clearly separated from hardware-validated
behavior and must not enable physical radio writes by default.

## Mesh operations

`mesh-operations/` contains the Rust `mesh-sim` engine and its local web
visualizer. It demonstrates CHUD-style radio discovery and configuration,
leaderless mesh formation, mission synchronization, changing paths during
distributed node loss, and recovery across 200 simulated aircraft plus one GCS.

Run it from the repository root:

```powershell
.\scripts\Start-AVIAN-Visualizer.ps1
```

## RF Planning Suite

`rf-planning-suite/` contains the local RF link-budget and multi-node planning
application. Its calculations and network scenes are planning aids, not field
measurements or authorization to configure radios.

Run it from its directory with Node.js 22.13 or newer:

```sh
npm install
npm run dev
```

The imported source intentionally excludes dependencies, build outputs, local
logs, generated decks, credentials, nested Git history, and agent metadata.
