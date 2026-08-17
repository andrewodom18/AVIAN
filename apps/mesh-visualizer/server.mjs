import { spawn } from "node:child_process";
import { createReadStream } from "node:fs";
import { access, readFile } from "node:fs/promises";
import { createServer } from "node:http";
import path from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(appRoot, "..", "..");
const port = Number.parseInt(process.env.AVIAN_VISUALIZER_PORT ?? "3211", 10);

const contentTypes = new Map([
  [".css", "text/css; charset=utf-8"],
  [".html", "text/html; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".png", "image/png"],
]);

export function validateScenario(value) {
  if (!value || value.schema_version !== 1 || !Array.isArray(value.steps) || value.steps.length < 2) {
    throw new Error("mesh-sim returned an unsupported visual trace");
  }
  for (const step of value.steps) {
    if (!step.id || !Array.isArray(step.nodes) || !Array.isArray(step.links) || !step.metrics) {
      throw new Error(`mesh-sim returned an invalid step: ${step?.id ?? "unknown"}`);
    }
  }
  return value;
}

export function loadScenario() {
  return new Promise((resolve, reject) => {
    const cargo = spawn("cargo", ["run", "--quiet", "-p", "mesh-sim", "--", "--trace"], {
      cwd: workspaceRoot,
      windowsHide: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    const timer = setTimeout(() => {
      cargo.kill();
      reject(new Error("mesh-sim trace generation timed out"));
    }, 90_000);

    cargo.stdout.setEncoding("utf8");
    cargo.stderr.setEncoding("utf8");
    cargo.stdout.on("data", (chunk) => { stdout += chunk; });
    cargo.stderr.on("data", (chunk) => { stderr += chunk; });
    cargo.on("error", reject);
    cargo.on("close", (code) => {
      clearTimeout(timer);
      if (code !== 0) {
        reject(new Error(stderr.trim() || `mesh-sim exited with ${code}`));
        return;
      }
      try {
        resolve(validateScenario(JSON.parse(stdout)));
      } catch (error) {
        reject(error);
      }
    });
  });
}

function staticPath(urlPath) {
  const relative = urlPath === "/" ? "index.html" : urlPath.slice(1);
  const resolved = path.resolve(appRoot, relative);
  return resolved.startsWith(`${appRoot}${path.sep}`) || resolved === path.join(appRoot, "index.html")
    ? resolved
    : null;
}

let scenario;
let scenarioError;

async function refreshScenario() {
  try {
    scenario = await loadScenario();
    scenarioError = undefined;
  } catch (error) {
    scenarioError = error instanceof Error ? error.message : String(error);
    throw error;
  }
}

function json(response, status, value) {
  response.writeHead(status, {
    "cache-control": "no-store",
    "content-type": "application/json; charset=utf-8",
  });
  response.end(JSON.stringify(value));
}

async function requestHandler(request, response) {
  const requestUrl = new URL(request.url ?? "/", `http://${request.headers.host ?? "localhost"}`);
  if (requestUrl.pathname === "/api/health") {
    json(response, scenario ? 200 : 503, { ok: Boolean(scenario), error: scenarioError ?? null });
    return;
  }
  if (requestUrl.pathname === "/api/scenario") {
    if (!scenario) {
      json(response, 503, { error: scenarioError ?? "scenario is still loading" });
      return;
    }
    json(response, 200, scenario);
    return;
  }
  if (requestUrl.pathname === "/brand/avian-mark.png") {
    const logo = path.join(workspaceRoot, "assets", "brand", "avian-mark-white-on-black.png");
    response.writeHead(200, { "cache-control": "public, max-age=3600", "content-type": "image/png" });
    createReadStream(logo).pipe(response);
    return;
  }

  const file = staticPath(requestUrl.pathname);
  if (!file) {
    response.writeHead(404).end("Not found");
    return;
  }
  try {
    await access(file);
    const body = await readFile(file);
    response.writeHead(200, {
      "cache-control": "no-store",
      "content-type": contentTypes.get(path.extname(file)) ?? "application/octet-stream",
    });
    response.end(body);
  } catch {
    response.writeHead(404).end("Not found");
  }
}

await refreshScenario();
const server = createServer((request, response) => {
  requestHandler(request, response).catch((error) => {
    json(response, 500, { error: error instanceof Error ? error.message : String(error) });
  });
});

server.listen(port, "127.0.0.1", () => {
  console.log(`AVIAN visualizer ready at http://127.0.0.1:${port}`);
});

