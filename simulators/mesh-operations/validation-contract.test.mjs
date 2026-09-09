import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

test("versioned schemas retain the expected public contracts", () => {
  const scenario = JSON.parse(readFileSync(path.join(root, "simulators/mesh-operations/schemas/scenario-v2.schema.json")));
  const report = JSON.parse(readFileSync(path.join(root, "simulators/mesh-operations/schemas/validation-report-v1.schema.json")));
  assert.equal(scenario.properties.schema_version.const, "2.0.0");
  assert.equal(scenario.properties.aircraft.maximum, 200);
  assert.equal(report.properties.schema_version.const, "1.0.0");
  assert.ok(report.properties.scenarios.items.properties.metrics.required.includes("delivery_ratio"));
});

test("validation CLI emits deterministic 5-200 node evidence", () => {
  const run = () => spawnSync("cargo", ["run", "--quiet", "-p", "mesh-sim", "--", "--validate", "--seed", "20260825"], {
    cwd: root, encoding: "utf8", windowsHide: true, maxBuffer: 16 * 1024 * 1024,
  });
  const first = run();
  assert.equal(first.status, 0, first.stderr);
  const second = run();
  assert.equal(second.status, 0, second.stderr);
  const report = JSON.parse(first.stdout);
  const repeated = JSON.parse(second.stdout);
  assert.equal(report.schema_version, "1.0.0");
  assert.equal(report.passed, true);
  assert.deepEqual(report.scenarios.map((item) => item.config.aircraft), [5, 25, 50, 100, 150, 200]);
  assert.deepEqual(report.scenarios.map((item) => item.event_digest_sha256), repeated.scenarios.map((item) => item.event_digest_sha256));
  assert.ok(report.limitations.some((value) => /no real radio/i.test(value)));
  for (const scenario of report.scenarios) {
    assert.equal(scenario.metrics.nodes, scenario.config.aircraft + 1);
    assert.ok(scenario.metrics.max_degree <= 8);
    assert.equal(scenario.metrics.self_links, 0);
    assert.equal(scenario.metrics.duplicate_links, 0);
    assert.ok(scenario.metrics.delivery_ratio >= 0.95);
    assert.equal(scenario.metrics.recovered_connected, true);
  }
});
