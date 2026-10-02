import { createServer } from "node:http";
import { pathToFileURL } from "node:url";
import { ChudEmulator } from "./emulator.mjs";
import { Barriers } from "./barriers.mjs";

export function createChudServer({ token = process.env.AVIAN_CHUD_EMULATOR_TOKEN ?? "", barrierTimeoutMs = 10000, ...options } = {}) {
  const emulator = new ChudEmulator({ token, ...options });
  const barriers = new Barriers(barrierTimeoutMs);
  let activeMutations = 0;
  let responseDelayMs = 0;
  const control = () => ({ ...emulator.controlState(), barriers: barriers.snapshot(), response_delay_ms: responseDelayMs });

  const server = createServer(async (request, response) => {
    const send = (status, value) => {
      if (response.destroyed || response.writableEnded) return;
      response.writeHead(status, { "cache-control": "no-store", "content-type": "application/json; charset=utf-8" });
      response.end(JSON.stringify(value));
    };
    try {
      const url = new URL(request.url ?? "/", "http://127.0.0.1");
      if (!emulator.authorize(request.headers.authorization)) {
        send(401, { error: "unauthorized", simulated: true, hardware_write: false });
        return;
      }
      const body = request.method === "POST" ? await readBody(request) : {};
      if (request.method === "GET" && url.pathname === "/api/radio/devices") send(200, emulator.listDevices());
      else if (request.method === "GET" && url.pathname === "/api/radio/snapshot") send(200, emulator.snapshot(url.searchParams.get("mac")));
      else if (request.method === "GET" && url.pathname === "/api/radio/operations") send(200, emulator.listOperations(url.searchParams.get("mac")));
      else if (request.method === "POST" && ["/api/radio/apply", "/api/radio/confirm"].includes(url.pathname)) {
        if (activeMutations >= 32) throw Object.assign(new Error("too many pending mutations"), { status: 429 });
        activeMutations++;
        try {
          await barriers.reach("request_accepted", request, response);
          if (response.destroyed) return;
          const apply = url.pathname.endsWith("/apply");
          const result = apply ? emulator.apply(body) : emulator.confirm(body);
          await barriers.reach("side_effect_recorded", request, response);
          if (apply && emulator.faultFor(body.mac) === "accepted_response_lost") {
            response.destroy();
            return;
          }
          await barriers.reach("before_response", request, response);
          if (responseDelayMs) await new Promise((resolve) => {
            const done = () => { clearTimeout(timer); response.off("close", done); resolve(); };
            const timer = setTimeout(done, responseDelayMs);
            response.once("close", done);
            if (response.destroyed) done();
          });
          send(200, result);
        } finally { activeMutations--; }
      }
      else if (request.method === "GET" && url.pathname === "/__sim/ledger") send(200, emulator.getLedger());
      else if (request.method === "GET" && url.pathname === "/__sim/control") send(200, control());
      else if (request.method === "POST" && url.pathname === "/__sim/control") {
        // One control per request avoids partially applied multi-control payloads.
        const actions = ["fault", "device", "advance_ms", "barrier", "release", "response_delay_ms"].filter((key) => Object.hasOwn(body, key));
        if (actions.length !== 1) throw Object.assign(new Error("exactly one simulation control is required"), { status: 400 });
        switch (actions[0]) {
          case "fault": emulator.setFault(body.fault, body.mac); break;
          case "device": emulator.setDevice(body.device); break;
          case "advance_ms": emulator.advance(body.advance_ms); break;
          case "barrier": barriers.arm(body.barrier); break;
          case "release": barriers.release(body.release); break;
          case "response_delay_ms":
            if (!Number.isSafeInteger(body.response_delay_ms) || body.response_delay_ms < 0 || body.response_delay_ms > 10000) throw Object.assign(new Error("invalid response delay"), { status: 400 });
            responseDelayMs = body.response_delay_ms;
            break;
        }
        send(200, control());
      }
      else send(404, { error: "not found", simulated: true, hardware_write: false });
    } catch (error) {
      send(error.status ?? 500, { error: error instanceof Error ? error.message : String(error), simulated: true, hardware_write: false });
    }
  });
  const listen = server.listen.bind(server);
  server.listen = (...args) => {
    const host = typeof args[0] === "object" ? args[0]?.host : args[1];
    if (!["127.0.0.1", "::1"].includes(host)) throw new Error("emulator must bind an explicit loopback address");
    return listen(...args);
  };
  server.requestTimeout = 15000;
  server.headersTimeout = 10000;
  server.maxConnections = 64;
  server.on("close", () => barriers.close());
  return { server, emulator, barriers };
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    let size = 0;
    const chunks = [];
    request.on("data", (chunk) => {
      size += chunk.length;
      if (size > 65_536) {
        chunks.length = 0;
        reject(Object.assign(new Error("request too large"), { status: 413 }));
      } else chunks.push(chunk);
    });
    request.on("end", () => {
      if (size > 65_536) return;
      try {
        const value = JSON.parse(Buffer.concat(chunks).toString("utf8") || "{}");
        if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error("object required");
        resolve(value);
      }
      catch { reject(Object.assign(new Error("request must be valid JSON"), { status: 400 })); }
    });
    request.on("error", reject);
    request.on("aborted", () => reject(Object.assign(new Error("request aborted"), { status: 400 })));
  });
}

const launchedDirectly = process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (launchedDirectly) {
  const port = Number(process.env.AVIAN_CHUD_EMULATOR_PORT ?? "3212");
  if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error("invalid emulator port");
  const { server } = createChudServer({ stateFile: process.env.AVIAN_CHUD_EMULATOR_STATE_FILE });
  server.listen(port, "127.0.0.1", () => {
    console.log(`AVIAN CHUD contract emulator ready at http://127.0.0.1:${server.address().port} (SIMULATION; no hardware writes)`);
  });
}
