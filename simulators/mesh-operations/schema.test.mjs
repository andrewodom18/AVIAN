import { createHash } from "node:crypto";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import Ajv from "ajv/dist/2020.js";
import { validateReport } from "./schema-validation.mjs";

const schema = JSON.parse(readFileSync(new URL("./schemas/transport-event-v1.schema.json", import.meta.url)));
test("event contract rejects missing, extra, mistyped and unknown fields", () => {
  const validate = new Ajv({ strict: true }).compile(schema);
  const valid = { sequence: 1, at_ms: 0, message_id: 1, from: 0, to: 1, generation: 1, attempt: 0, outcome: "queued" };
  assert.equal(validate(valid), true);
  for (const mutation of [{ ...valid, at_ms: -1 }, { ...valid, attempt: "0" }, { ...valid, outcome: "magic" }, { ...valid, extra: true }]) assert.equal(validate(mutation), false);
  for (const key of Object.keys(valid)) {
    const partial = { ...valid }; delete partial[key];
    assert.equal(validate(partial), false);
  }
});

test("report consumer refuses v1 evidence and incomplete v2 reports", () => {
  for (const version of ["1.0.0", "2.0.0", "3.0.0"]) {
    assert.throws(() => validateReport({ schema_version: version, passed: true }), /Invalid v2 report/);
  }
});


// Small truthful failure reports exercise consumer semantics without a Rust build.
function fixtureReport() {
  const scenario = (aircraft, profile) => {
    const events = [{ sequence: 1, at_ms: 10, message_id: 1, from: 0, to: 1, generation: 1, attempt: 0, outcome: "ttl_expired" }];
    const metrics = Object.fromEntries(["links", "max_degree", "self_links", "duplicate_links", "messages_delivered", "delivery_latency_p50_ms", "delivery_latency_p95_ms", "partitioned_nodes", "converged_nodes", "peak_queue", "queue_drops", "retry_exhausted", "duplicate_rejected", "stale_generation_rejected", "commands_applied", "delivery_ratio"].map((key) => [key, 0]));
    Object.assign(metrics, { nodes: aircraft + 1, messages_attempted: 1, messages_dropped: 1, ttl_expired: 1,
      generation_one_convergence_ms: null, generation_two_convergence_ms: null, recovery_ms: null, latest_generation_converged: false });
    return { name: `${profile}-${aircraft}`, config: { profile, aircraft, seed: 43, direct_peer_limit: 2,
      messages_per_node: 1, failure_percent: 0, transport: { bytes_per_ms: 1, queue_capacity: 1, packet_bytes: 1,
        ttl_ms: 10, retry_limit: 0, retry_ms: 1, loss_basis_points: 0, duplicate_basis_points: 0, reorder_ms: 0 } },
      links: [], messages: [{ id: 1, source: 0, target: 1, generation: 1, created_ms: 0, command: true }], faults: [],
      metrics, passed: false, events, event_digest_sha256: digest(events) };
  };
  const sizes = [5, 25, 50, 100, 150, 200];
  return { schema_version: "2.0.0", model: "consumer-test-fixture", limitations: ["Fixture, not qualification"],
    passed: false, events_included: true, scenarios: sizes.map((size) => scenario(size, "recovery")),
    fault_scenarios: sizes.flatMap((size) => ["congestion", "loss", "duplication", "expiry"].map((profile) => scenario(size, profile))) };
}
const digest = (events) => createHash("sha256").update(JSON.stringify(events)).digest("hex");

test("truthful failed reports remain readable and summary mode is explicit", () => {
  const report = fixtureReport();
  assert.equal(validateReport(report).passed, false);
  report.events_included = false;
  for (const s of [...report.scenarios, ...report.fault_scenarios]) s.events = [];
  assert.throws(() => validateReport(report), /Full event evidence/);
  assert.equal(validateReport(report, { requireEvents: false }).passed, false);
});

test("a valid digest cannot conceal contradictory matrix or event evidence", () => {
  const changes = [
    (r) => { r.passed = true; },
    (r) => { r.scenarios[0].passed = true; },
    (r) => { r.scenarios[1] = structuredClone(r.scenarios[0]); },
    (r) => { r.scenarios[0].messages[0].target = 99; },
    (r) => { r.scenarios[0].events[0].message_id = 99; },
    (r) => { r.scenarios[0].events = []; },
    (r) => { r.scenarios[0].events[0].outcome = "queued"; },
    (r) => { r.scenarios[0].events.push({ ...r.scenarios[0].events[0], sequence: 2 }); },
    (r) => { r.scenarios[0].metrics.ttl_expired = 0; },
    (r) => { r.scenarios[0].metrics.commands_applied = 1; },
    (r) => { r.scenarios[0].metrics.delivery_ratio = 0.5; },
    (r) => { r.scenarios[0].metrics.links = 1; },
    (r) => { r.scenarios[0].metrics.recovery_ms = 0; },
    (r) => { r.scenarios[0].faults.push({ at_ms: 0, node: 1, link_peer: null, online: false }); },
    (r) => { r.scenarios[0].messages.push({ ...r.scenarios[0].messages[0] }); r.scenarios[0].metrics.messages_attempted++; r.scenarios[0].metrics.messages_dropped++; },
  ];
  for (const change of changes) {
    const report = fixtureReport(); change(report);
    for (const s of [...report.scenarios, ...report.fault_scenarios]) s.event_digest_sha256 = digest(s.events);
    assert.throws(() => validateReport(report), /Inconsistent report/);
  }
});
