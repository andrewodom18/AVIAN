import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { ChudEmulator } from "./emulator.mjs";
import { createChudServer } from "./server.mjs";

const mac = "00:1e:3f:20:9a:10";
const other = "20:9b:60:20:9a:11";
const payload = { mac, desired: { network_id: { value: "LIFECYCLE" } }, confirm_timeout_seconds: 30 };

function storage(t) {
  const directory = mkdtempSync(path.join(os.tmpdir(), "avian-emulator-"));
  t.after(() => rmSync(directory, { recursive: true, force: true }));
  return path.join(directory, "state.json");
}

async function server(t, options = {}) {
  const app = createChudServer(options);
  await new Promise((resolve) => app.server.listen(0, "127.0.0.1", resolve));
  t.after(async () => {
    app.barriers.close();
    app.server.closeAllConnections();
    await new Promise((resolve) => app.server.close(resolve));
  });
  return { ...app, base: `http://127.0.0.1:${app.server.address().port}` };
}

async function request(base, route, body) {
  const response = await fetch(`${base}${route}`, { method: body === undefined ? "GET" : "POST",
    headers: { authorization: "Bearer fixture-token", "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(5000) });
  return { status: response.status, body: await response.json() };
}

test("persistent apply, idempotent confirm and rollback survive restart without duplicate writes", (t) => {
  const stateFile = storage(t);
  let now = 1000;
  const options = { stateFile, now: () => now };
  let emulator = new ChudEmulator(options);
  const applied = emulator.apply(payload);
  assert.throws(() => emulator.apply(payload), /unconfirmed/);
  emulator = new ChudEmulator(options);
  assert.equal(emulator.snapshot(mac).config.network_id.value, "LIFECYCLE");
  assert.equal(emulator.snapshot(other).config.network_id.value, "AVIAN-DEMO");
  emulator.confirm(applied);
  emulator.confirm(applied);
  assert.deepEqual(emulator.getLedger().events.map((e) => e.action), ["apply", "confirm"]);
  const second = emulator.apply({ ...payload, desired: { network_id: { value: "UNCONFIRMED" } } });
  now += 31_000;
  emulator = new ChudEmulator(options);
  assert.equal(emulator.snapshot(mac).config.network_id.value, "LIFECYCLE");
  assert.throws(() => emulator.confirm(second), /rolled back/);
  assert.deepEqual(emulator.getLedger().events.map((e) => e.action), ["apply", "confirm", "apply", "rollback"]);
  assert.equal(new ChudEmulator(options).getLedger().digest, emulator.getLedger().digest);
});

test("per-device unplug/reboot, auth, capability and driver failures do not write", () => {
  const emulator = new ChudEmulator();
  for (const state of ["unplugged", "rebooting", "stale", "authentication_failed"]) {
    emulator.setDevice({ mac, state });
    assert.throws(() => emulator.apply(payload), /unavailable|authentication/);
    assert.equal(emulator.snapshot(other).mac, other);
    if (["unplugged", "rebooting"].includes(state)) assert.equal(emulator.listDevices().devices.length, 1);
  }
  emulator.setDevice({ mac, state: "connected", driver_available: false });
  assert.throws(() => emulator.apply(payload), /driver/);
  emulator.setDevice({ mac, driver_available: true, meta: { network_id: { writable: false } } });
  assert.throws(() => emulator.apply(payload), /not writable/);
  emulator.setFault("readback_mismatch", mac);
  assert.equal(emulator.snapshot(mac).config.network_id.value, "FAULT-INJECTED");
  assert.equal(emulator.snapshot(other).config.network_id.value, "AVIAN-DEMO");
  assert.equal(emulator.getLedger().events.length, 0);
});

test("corrupt/truncated state fails closed and persistence failure rolls back memory", (t) => {
  const stateFile = storage(t);
  const emulator = new ChudEmulator({ stateFile });
  emulator.apply(payload);
  const saved = JSON.parse(readFileSync(stateFile, "utf8"));
  saved.state.ledger = [];
  writeFileSync(stateFile, JSON.stringify(saved));
  assert.throws(() => new ChudEmulator({ stateFile }), /checksum/);
  writeFileSync(stateFile, "{");
  assert.throws(() => new ChudEmulator({ stateFile }));
  const unavailable = new ChudEmulator({ stateFile: path.join(path.dirname(stateFile), "absent", "state.json") });
  assert.throws(() => unavailable.apply(payload), /ENOENT/);
  assert.equal(unavailable.getLedger().events.length, 0);
  assert.equal(unavailable.snapshot(mac).config.network_id.value, "AVIAN-DEMO");
});

test("bounded operation/ledger storage refuses work without partial mutation", () => {
  const emulator = new ChudEmulator({ maxOperations: 1, maxEvents: 2 });
  const op = emulator.apply(payload);
  emulator.confirm(op);
  assert.throws(() => emulator.apply({ ...payload, mac: other }), /capacity/);
  assert.equal(emulator.snapshot(other).config.network_id.value, "AVIAN-DEMO");
  const ledgerFull = new ChudEmulator({ maxEvents: 1 });
  const pending = ledgerFull.apply(payload);
  assert.throws(() => ledgerFull.confirm(pending), /capacity/);
  assert.equal(ledgerFull.listOperations().operations[0].awaiting_confirmation, true);
});

test("reboot state persists and return to management preserves operation identity", (t) => {
  const stateFile = storage(t);
  let emulator = new ChudEmulator({ stateFile });
  const operation = emulator.apply(payload);
  emulator.setDevice({ mac, state: "rebooting" });
  emulator = new ChudEmulator({ stateFile });
  assert.equal(emulator.listDevices().devices.length, 1);
  assert.throws(() => emulator.confirm(operation), /unavailable/);
  emulator.setDevice({ mac, state: "connected" });
  assert.equal(emulator.listOperations(mac).operations[0].operation_id, operation.operation_id);
  emulator.confirm(operation);
  assert.deepEqual(emulator.getLedger().events.map((e) => e.action), ["apply", "confirm"]);
});

test("loopback binding, malformed bodies and route errors are enforced", async (t) => {
  const app = await server(t, { token: "fixture-token" });
  assert.throws(() => createChudServer().server.listen(0, "0.0.0.0"), /loopback/);
  assert.throws(() => createChudServer().server.listen(0), /loopback/);
  assert.equal((await request(app.base, "/api/radio/snapshot?mac=bad")).status, 400);
  assert.equal((await request(app.base, "/api/radio/snapshot?mac=00:00:00:00:00:00")).status, 404);
  assert.equal((await request(app.base, "/api/radio/confirm", { operation_id: "absent" })).status, 404);
  for (const body of [null, [], {}, { ...payload, desired: {} }, { ...payload, desired: { network_id: 1 } }, { ...payload, confirm_timeout_seconds: -1 }]) {
    assert.equal((await request(app.base, "/api/radio/apply", body)).status, 400);
  }
  for (const body of ["{", JSON.stringify({ data: "é".repeat(40000) })]) {
    const response = await fetch(`${app.base}/api/radio/apply`, { method: "POST", headers: { authorization: "Bearer fixture-token" }, body });
    assert.equal(response.status, body === "{" ? 400 : 413);
  }
  assert.equal((await request(app.base, "/__sim/control", { fault: "unknown" })).status, 400);
  assert.equal((await request(app.base, "/__sim/control", { fault: null, advance_ms: 1 })).status, 400);
  assert.equal(app.emulator.getLedger().events.length, 0);
});

test("response delay and accepted response loss preserve the actual operation", async (t) => {
  const app = await server(t);
  await request(app.base, "/__sim/control", { response_delay_ms: 80 });
  const start = performance.now();
  const applied = await request(app.base, "/api/radio/apply", payload);
  assert.equal(applied.status, 200);
  assert.ok(performance.now() - start >= 70);
  app.emulator.confirm(applied.body);
  await request(app.base, "/__sim/control", { fault: "accepted_response_lost", mac: other });
  await assert.rejects(() => request(app.base, "/api/radio/apply", { ...payload, mac: other }));
  const operations = (await request(app.base, `/api/radio/operations?mac=${other}`)).body.operations;
  assert.equal(operations.length, 1);
  assert.equal(operations[0].awaiting_confirmation, true);
  assert.equal(app.emulator.getLedger().events.filter((e) => e.mac === other && e.action === "apply").length, 1);
});

async function child(t, stateFile) {
  const process = spawn(globalThis.process.execPath, [fileURLToPath(new URL("./server.mjs", import.meta.url))], {
    env: { ...globalThis.process.env, AVIAN_CHUD_EMULATOR_PORT: "0", AVIAN_CHUD_EMULATOR_STATE_FILE: stateFile, AVIAN_CHUD_EMULATOR_TOKEN: "fixture-token" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  const stopped = once(process, "exit");
  t.after(async () => { if (process.exitCode === null && process.signalCode === null) process.kill("SIGKILL"); await stopped; });
  let output = "";
  const base = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`child startup timeout: ${output}`)), 5000);
    process.once("error", (error) => { clearTimeout(timer); reject(error); });
    process.once("exit", () => { clearTimeout(timer); reject(new Error(`child exited: ${output}`)); });
    process.stdout.on("data", (data) => {
      output = (output + data).slice(-8192);
      const match = output.match(/http:\/\/127\.0\.0\.1:\d+/);
      if (match) { clearTimeout(timer); resolve(match[0]); }
    });
    process.stderr.on("data", (data) => { output = (output + data).slice(-8192); });
  });
  return { base, process, stopped };
}

for (const [phase, expectedWrites] of [["request_accepted", 0], ["side_effect_recorded", 1], ["before_response", 1]]) {
  test(`SIGKILL at ${phase} preserves exactly ${expectedWrites} applies`, { timeout: 15000 }, async (t) => {
    const stateFile = storage(t);
    const app = await child(t, stateFile);
    await request(app.base, "/__sim/control", { barrier: phase });
    const pending = request(app.base, "/api/radio/apply", payload).catch(() => null);
    const deadline = Date.now() + 4000;
    let reached = false;
    while (Date.now() < deadline) {
      const control = (await request(app.base, "/__sim/control")).body;
      if (control.barriers.pending.some((b) => b.phase === phase)) { reached = true; break; }
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    assert.equal(reached, true, "explicit crash barrier reached");
    app.process.kill("SIGKILL");
    await app.stopped;
    await pending;
    const restarted = await child(t, stateFile);
    const ledger = (await request(restarted.base, "/__sim/ledger")).body;
    assert.equal(ledger.events.filter((e) => e.action === "apply").length, expectedWrites);
    const operations = (await request(restarted.base, `/api/radio/operations?mac=${mac}`)).body.operations;
    assert.equal(operations.length, expectedWrites);
    if (expectedWrites) {
      const confirm = await request(restarted.base, "/api/radio/confirm", { operation_id: operations[0].operation_id });
      assert.equal(confirm.status, 200);
      assert.equal((await request(restarted.base, "/__sim/ledger")).body.events.length, 2);
    }
  });
}

test("barriers release explicitly and timeout without a pre-acceptance write", async (t) => {
  const app = await server(t, { barrierTimeoutMs: 50 });
  await request(app.base, "/__sim/control", { barrier: "request_accepted" });
  assert.equal((await request(app.base, "/api/radio/apply", payload)).status, 504);
  assert.equal(app.emulator.getLedger().events.length, 0);
  await request(app.base, "/__sim/control", { barrier: "before_response" });
  const pending = request(app.base, "/api/radio/apply", payload);
  const deadline = Date.now() + 1000;
  let id;
  while (!id && Date.now() < deadline) {
    id = (await request(app.base, "/__sim/control")).body.barriers.pending[0]?.id;
  }
  assert.ok(id);
  await request(app.base, "/__sim/control", { release: id });
  assert.equal((await pending).status, 200);
});
