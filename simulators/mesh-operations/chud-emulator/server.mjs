import { createServer } from "node:http";
import { pathToFileURL } from "node:url";
import { ChudEmulator } from "./emulator.mjs";

export function createChudServer({ token = process.env.AVIAN_CHUD_EMULATOR_TOKEN ?? "" } = {}) {
  const emulator = new ChudEmulator({ token });

  const server = createServer(async (request, response) => {
    const url = new URL(request.url ?? "/", `http://${request.headers.host ?? "127.0.0.1"}`);
    const send = (status, value) => {
      response.writeHead(status, { "cache-control": "no-store", "content-type": "application/json; charset=utf-8" });
      response.end(JSON.stringify(value));
    };
    try {
      if (!emulator.authorize(request.headers.authorization)) {
        send(401, { error: "unauthorized", simulated: true, hardware_write: false });
        return;
      }
      const body = request.method === "POST" ? await readBody(request) : {};
      if (request.method === "GET" && url.pathname === "/api/radio/devices") send(200, emulator.listDevices());
      else if (request.method === "GET" && url.pathname === "/api/radio/snapshot") send(200, emulator.snapshot(url.searchParams.get("mac")));
      else if (request.method === "GET" && url.pathname === "/api/radio/operations") send(200, emulator.listOperations(url.searchParams.get("mac")));
      else if (request.method === "POST" && url.pathname === "/api/radio/apply") send(200, emulator.apply(body));
      else if (request.method === "POST" && url.pathname === "/api/radio/confirm") send(200, emulator.confirm(body));
      else if (request.method === "GET" && url.pathname === "/__sim/ledger") send(200, emulator.getLedger());
      else if (request.method === "GET" && url.pathname === "/__sim/control") send(200, emulator.controlState());
      else if (request.method === "POST" && url.pathname === "/__sim/control") send(200, emulator.setFault(body.fault ?? null));
      else send(404, { error: "not found", simulated: true, hardware_write: false });
    } catch (error) {
      send(error.status ?? 500, { error: error instanceof Error ? error.message : String(error), simulated: true, hardware_write: false });
    }
  });
  return { server, emulator };
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk) => {
      body += chunk;
      if (body.length > 65_536) reject(Object.assign(new Error("request too large"), { status: 413 }));
    });
    request.on("end", () => {
      try { resolve(JSON.parse(body || "{}")); }
      catch { reject(Object.assign(new Error("request must be valid JSON"), { status: 400 })); }
    });
    request.on("error", reject);
  });
}

const launchedDirectly = process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (launchedDirectly) {
  const port = Number.parseInt(process.env.AVIAN_CHUD_EMULATOR_PORT ?? "3212", 10);
  const { server } = createChudServer();
  server.listen(port, "127.0.0.1", () => {
    console.log(`AVIAN CHUD contract emulator ready at http://127.0.0.1:${port} (SIMULATION; no hardware writes)`);
  });
}
