import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { DEFAULT_RADIO_CONFIGURATION, validateRadioConfiguration } from "./radio-config.mjs";

const appRoot = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(appRoot, "..", "..", "..");

test("visualizer shell exposes playback and topology surfaces", () => {
  const html = readFileSync(path.join(appRoot, "index.html"), "utf8");
  for (const id of [
    "topology", "timeline", "playButton", "stepButton", "nodeDetails", "controlActivity",
    "simulationTab", "aboutTab", "aboutView", "guidedFocus",
    "radioConfigForm", "bandInput", "frequencyInput", "bandwidthInput", "powerInput",
    "validateRadioButton", "applyRadioButton", "radioConfigResult",
  ]) {
    assert.match(html, new RegExp(`id=["']${id}["']`));
  }
  assert.match(html, /What the AVIAN simulator actually does/);
  assert.match(html, /Executed 200-aircraft scale run/);
});

test("visualizer renders individual scale nodes and guided pacing", () => {
  const source = readFileSync(path.join(appRoot, "app.js"), "utf8");
  assert.match(source, /aircraftNodes\.forEach/);
  assert.match(source, /class: `scale-aircraft \$\{node\.status\}`/);
  assert.match(source, /class: `scale-link \$\{link\.state\}`/);
  assert.match(source, /stepIndex < 5\s*\? 4400/);
  assert.match(source, /guidedSteps/);
});

test("mesh-sim emits the visualizer schema", () => {
  const result = spawnSync("cargo", ["run", "--quiet", "-p", "mesh-sim", "--", "--trace"], {
    cwd: workspaceRoot,
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(result.status, 0, result.stderr);
  const scenario = JSON.parse(result.stdout);
  assert.equal(scenario.schema_version, 1);
  assert.equal(scenario.steps.length, 18);
  assert.deepEqual(scenario.steps.slice(0, 5).map((step) => step.id), [
    "radio-connected",
    "radios-discovered",
    "radio-snapshot",
    "radio-config-applied",
    "radio-config-confirmed",
  ]);
  assert.equal(scenario.steps[0].nodes.length, 1);
  assert.equal(scenario.steps[1].nodes.length, 4);
  assert.equal(scenario.steps[1].control_event.path, "/api/radio/devices");
  assert.equal(scenario.steps[3].control_event.path, "/api/radio/apply");
  assert.equal(scenario.steps[4].control_event.path, "/api/radio/confirm");
  assert.equal(scenario.steps[4].control_event.simulated, true);
  assert.equal(scenario.steps.find((step) => step.id === "command-acknowledged").metrics.signed_command_verified, true);
  assert.equal(scenario.steps.find((step) => step.id === "ground-partitioned").metrics.connected_components, 2);
  const scaleOnline = scenario.steps.find((step) => step.id === "maximum-formation-online");
  assert.equal(scaleOnline.metrics.online_nodes, 201);
  const scaleRerouting = scenario.steps.find((step) => step.id === "maximum-formation-rerouting");
  assert.equal(scaleRerouting.metrics.online_nodes, 180);
  assert.equal(scaleRerouting.metrics.connected_components, 1);
  assert.equal(scaleRerouting.metrics.mission_synced_nodes, 180);
  assert.equal(scaleRerouting.nodes.filter((node) => node.status === "offline").length, 21);
  const maximum = scenario.steps.at(-1);
  assert.equal(maximum.id, "maximum-formation-mission");
  assert.equal(maximum.formation_summary.simulated_aircraft, 200);
  assert.equal(maximum.formation_summary.control_stations, 1);
  assert.equal(maximum.formation_summary.direct_peer_limit, 8);
  assert.equal(maximum.formation_summary.maximum_overlay_links, 800);
  assert.equal(maximum.nodes.length, 201);
  assert.equal(maximum.links.length, 801);
  assert.equal(maximum.metrics.online_nodes, 201);
  assert.equal(maximum.metrics.active_links, 801);
  assert.equal(maximum.metrics.connected_components, 1);
  assert.equal(maximum.metrics.mission_synced_nodes, 201);
  assert.ok(maximum.nodes.every((node) => node.mission_synced));
  assert.equal(maximum.formation_summary.ground_partition_continuity_verified, true);
  assert.equal(maximum.formation_summary.distributed_loss_nodes, 20);
  assert.equal(maximum.formation_summary.distributed_loss_continuity_verified, true);
  assert.equal(maximum.formation_summary.recovery_converged, true);
  assert.equal(maximum.formation_summary.field_validated, false);
});

test("radio configuration controls enforce the simulated contract", () => {
  assert.deepEqual(validateRadioConfiguration(DEFAULT_RADIO_CONFIGURATION), DEFAULT_RADIO_CONFIGURATION);
  assert.throws(
    () => validateRadioConfiguration({ ...DEFAULT_RADIO_CONFIGURATION, center_frequency_mhz: 5800 }),
    /1850–2600 MHz/,
  );
  assert.throws(
    () => validateRadioConfiguration({ ...DEFAULT_RADIO_CONFIGURATION, transmit_power_dbm: 40 }),
    /0–39 dBm/,
  );
});
