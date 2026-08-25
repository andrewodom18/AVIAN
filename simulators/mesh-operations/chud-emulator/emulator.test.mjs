import assert from "node:assert/strict";
import test from "node:test";
import { createChudServer } from "./server.mjs";

async function fixture(options = {}) {
  const { server, emulator } = createChudServer(options);
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  return { base: `http://127.0.0.1:${port}`, emulator, close: () => new Promise((resolve) => server.close(resolve)) };
}

test("keeps same-IP radios distinct by canonical MAC", async (t) => {
  const app = await fixture();
  t.after(app.close);
  const response = await fetch(`${app.base}/api/radio/devices`);
  const body = await response.json();
  assert.equal(response.status, 200);
  assert.equal(body.devices.length, 2);
  assert.equal(new Set(body.devices.map((device) => device.reach_addr)).size, 1);
  assert.equal(new Set(body.devices.map((device) => device.mac)).size, 2);
  assert.equal(body.hardware_write, false);
});

test("implements guarded apply, operation polling, confirmation, and readback", async (t) => {
  const app = await fixture();
  t.after(app.close);
  const mac = "00-1E-3F-20-9A-10";
  const desired = { network_id: { value: "SWARM-7" }, transmit_power_dbm: { value: 24 } };
  const apply = await fetch(`${app.base}/api/radio/apply`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ mac, desired, defer_persist: true, confirm_timeout_seconds: 30 }),
  }).then((response) => response.json());
  assert.match(apply.operation_id, /^sim-op-/);
  assert.equal(apply.hardware_write, false);
  const operations = await fetch(`${app.base}/api/radio/operations?mac=${encodeURIComponent(mac)}`).then((response) => response.json());
  assert.equal(operations.operations[0].awaiting_confirmation, true);
  await fetch(`${app.base}/api/radio/confirm`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ operation_id: apply.operation_id }),
  });
  const snapshot = await fetch(`${app.base}/api/radio/snapshot?mac=${encodeURIComponent(mac)}`).then((response) => response.json());
  assert.deepEqual(snapshot.config.network_id, { value: "SWARM-7" });
  const ledger = await fetch(`${app.base}/__sim/ledger`).then((response) => response.json());
  assert.deepEqual(ledger.events.map((event) => event.action), ["apply", "confirm"]);
  assert.match(ledger.digest, /^[0-9a-f]{64}$/);
});

test("rejects mutation without auth and supports deterministic fault injection", async (t) => {
  const app = await fixture({ token: "test-secret" });
  t.after(app.close);
  const unauthorized = await fetch(`${app.base}/api/radio/devices`);
  assert.equal(unauthorized.status, 401);
  const headers = { authorization: "Bearer test-secret", "content-type": "application/json" };
  await fetch(`${app.base}/__sim/control`, { method: "POST", headers, body: JSON.stringify({ fault: "apply_error" }) });
  const failed = await fetch(`${app.base}/api/radio/apply`, {
    method: "POST", headers,
    body: JSON.stringify({ mac: "00:1e:3f:20:9a:10", desired: { network_id: { value: "NOPE" } } }),
  });
  assert.equal(failed.status, 502);
  assert.match((await failed.json()).error, /fault-injected/);
});

test("never proxies unknown routes", async (t) => {
  const app = await fixture();
  t.after(app.close);
  const response = await fetch(`${app.base}/vendor/live-radio`);
  assert.equal(response.status, 404);
  assert.equal((await response.json()).hardware_write, false);
});

test("covers ARC-relevant state and transaction faults", async (t) => {
  const app = await fixture();
  t.after(app.close);
  const mac = "00:1e:3f:20:9a:10";
  const control = async (fault) => fetch(`${app.base}/__sim/control`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ fault }),
  });

  await control("stale_device");
  assert.equal((await fetch(`${app.base}/api/radio/devices`).then((response) => response.json())).devices[0].state, "stale");
  await control("authentication_failed");
  assert.equal((await fetch(`${app.base}/api/radio/devices`).then((response) => response.json())).devices[0].state, "authentication_failed");

  await control("missing_operation_id");
  let apply = await fetch(`${app.base}/api/radio/apply`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ mac, desired: { network_id: { value: "MISSING-ID" } } }),
  }).then((response) => response.json());
  assert.equal(apply.operation_id, "");

  await control("operation_expired");
  apply = await fetch(`${app.base}/api/radio/apply`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ mac, desired: { network_id: { value: "ROLLBACK" } } }),
  }).then((response) => response.json());
  const operations = await fetch(`${app.base}/api/radio/operations?mac=${mac}`).then((response) => response.json());
  const expired = operations.operations.find((operation) => operation.operation_id === apply.operation_id);
  assert.deepEqual(expired.result, { rolled_back: true, state: "rolled_back" });

  await control("readback_mismatch");
  const snapshot = await fetch(`${app.base}/api/radio/snapshot?mac=${mac}`).then((response) => response.json());
  assert.equal(snapshot.config.network_id.value, "FAULT-INJECTED");

  await control("reboot_required");
  apply = await fetch(`${app.base}/api/radio/apply`, {
    method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ mac, desired: { transmit_power_dbm: { value: 18 } } }),
  }).then((response) => response.json());
  assert.equal(apply.reboot_required, true);
});
