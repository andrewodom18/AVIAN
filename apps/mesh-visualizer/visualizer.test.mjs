import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const appRoot = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(appRoot, "..", "..");

test("visualizer shell exposes playback and topology surfaces", () => {
  const html = readFileSync(path.join(appRoot, "index.html"), "utf8");
  for (const id of ["topology", "timeline", "playButton", "stepButton", "nodeDetails", "controlActivity"]) {
    assert.match(html, new RegExp(`id=["']${id}["']`));
  }
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
  assert.equal(scenario.steps.length, 15);
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
  assert.equal(scenario.steps.at(-1).metrics.signed_command_verified, true);
  assert.equal(scenario.steps.find((step) => step.id === "ground-partitioned").metrics.connected_components, 2);
});
