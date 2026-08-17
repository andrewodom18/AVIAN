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
  for (const id of ["topology", "timeline", "playButton", "stepButton", "nodeDetails"]) {
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
  assert.equal(scenario.steps.length, 10);
  assert.equal(scenario.steps.at(-1).metrics.signed_command_verified, true);
  assert.equal(scenario.steps.find((step) => step.id === "ground-partitioned").metrics.connected_components, 2);
});

