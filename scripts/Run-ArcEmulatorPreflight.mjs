import { spawn, spawnSync } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { mkdir, mkdtemp, readFile, realpath, rename, writeFile } from "node:fs/promises";
import { createServer } from "node:net";
import { request as httpRequest } from "node:http";
import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

const avianRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const emulatorFile = path.join(avianRoot, "simulators/mesh-operations/chud-emulator/server.mjs");
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");

export function isolatedEnvironment(directory, additions = {}) {
  const env = {};
  for (const key of ["PATH", "Path", "SystemRoot", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT"]) {
    if (process.env[key]) env[key] = process.env[key];
  }
  return { ...env, HOME: directory, USERPROFILE: directory, TMPDIR: directory, TMP: directory,
    TEMP: directory, XDG_CONFIG_HOME: directory, XDG_DATA_HOME: directory, ...additions };
}

function provenance(root) {
  const git = (...args) => {
    const r = spawnSync("git", ["-C", root, ...args], { encoding: "utf8", timeout: 10000 });
    if (r.status !== 0) throw new Error("cannot read source provenance");
    return r.stdout.trim();
  };
  return { root, commit: git("rev-parse", "HEAD"), branch: git("branch", "--show-current"),
    status: git("status", "--porcelain") };
}

export function validatePorts(ports) {
  if (ports.length !== 3 || new Set(ports).size !== 3
    || ports.some((p) => !Number.isSafeInteger(p) || p < 1024 || p > 65535)) {
    throw new Error("three distinct unprivileged ports are required");
  }
}

async function assertUnused(port) {
  await new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", () => reject(new Error(`port ${port} is occupied or unavailable`)));
    server.listen(port, "127.0.0.1", () => server.close(resolve));
  });
}

async function localJson(port, route, token, signal) {
  // Node's direct HTTP client does not consult HTTP_PROXY or follow redirects.
  return new Promise((resolve, reject) => {
    const request = httpRequest({ hostname: "127.0.0.1", port, path: route,
      signal: AbortSignal.any([signal, AbortSignal.timeout(1000)]),
      headers: token ? { authorization: `Bearer ${token}` } : {},
    }, (response) => {
      let size = 0;
      const chunks = [];
      response.on("error", reject);
      response.on("data", (chunk) => {
        size += chunk.length;
        if (size > 1024 * 1024) request.destroy(new Error("response exceeds evidence limit"));
        else chunks.push(chunk);
      });
      response.on("end", () => {
        let body;
        try { body = JSON.parse(Buffer.concat(chunks).toString("utf8")); } catch { body = null; }
        resolve({ status: response.statusCode, body });
      });
    });
    request.on("error", reject);
    request.end();
  });
}

async function canonicalDestination(directory) {
  const absolute = path.resolve(directory);
  try { return await realpath(absolute); }
  catch (error) {
    if (error.code !== "ENOENT") throw error;
    return path.join(await canonicalDestination(path.dirname(absolute)), path.basename(absolute));
  }
}

function startChild(name, file, args, cwd, env) {
  const child = spawn(file, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"], shell: false });
  const entry = { name, child, logs: [], size: 0, failure: null, exited: false };
  entry.closed = new Promise((resolve) => {
    child.once("error", (error) => { entry.failure = `launch failed: ${error.code ?? "unknown"}`; });
    child.once("close", (code, signal) => { entry.exited = true; entry.code = code; entry.signal = signal; resolve(); });
  });
  for (const stream of [child.stdout, child.stderr]) stream.on("data", (chunk) => {
    entry.size += chunk.length;
    if (entry.size <= 1024 * 1024) entry.logs.push(chunk);
    else { entry.failure = "child log limit exceeded"; child.kill("SIGKILL"); }
  });
  return entry;
}

function assertRunning(entries) {
  for (const entry of entries) {
    if (entry.failure || entry.exited || entry.child.exitCode !== null || entry.child.signalCode !== null) {
      throw new Error(`${entry.name}: ${entry.failure ?? "exited before completion"}`);
    }
  }
}

async function stopChild(entry) {
  if (!entry.exited) {
    entry.child.kill("SIGTERM");
    await Promise.race([entry.closed, delay(1500)]);
    if (!entry.exited) {
      entry.child.kill("SIGKILL");
      await Promise.race([entry.closed, delay(1500)]);
    }
  }
  if (!entry.exited) throw new Error(`${entry.name} did not terminate`);
}

/** A read-only startup prerequisite, never an apply/readback or UI qualification. */
export async function runPreflight({ arcRoot, bridgeExe, outputRoot,
  bridgePort = 19101, commandPort = 19100, emulatorPort = 13212,
  timeoutMs = 15000, signal = new AbortController().signal,
  bridgePrefixArgs = [] } = {}) {
  validatePorts([bridgePort, commandPort, emulatorPort]);
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 60000) throw new Error("invalid readiness timeout");
  // Resolve symlinks before checking the output boundary.
  const source = await realpath(arcRoot);
  const executable = await realpath(bridgeExe);
  const output = await canonicalDestination(outputRoot);
  for (const root of [await realpath(avianRoot), source]) {
    const relative = path.relative(root, output);
    if (!relative || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative))) {
      throw new Error("evidence output must be outside source repositories");
    }
  }
  await mkdir(output, { recursive: true });
  const directory = await mkdtemp(path.join(output, "arc-preflight-"));
  const state = path.join(directory, "state");
  await mkdir(state);
  const token = randomBytes(32).toString("hex");
  const redact = (text) => text.replaceAll(token, "[REDACTED]");
  const owned = [];
  const report = { schema_version: 1, scope: "mock-radio-control-preflight", verdict: "INCOMPLETE",
    simulation_only: true, hardware_validation: false, workflow_qualified: false,
    fixture_bridge: bridgePrefixArgs.length > 0, steps: [], processes: [], files: {},
    started_utc: new Date().toISOString(), limitations: [
      "Read-only startup evidence; no apply, readback, browser or independent-process PEAT acceptance.",
      "Executable hash is recorded; source provenance does not independently prove which source built the executable.",
    ] };
  const save = async () => {
    report.updated_utc = new Date().toISOString();
    await writeFile(path.join(directory, "report.json.pending"), redact(JSON.stringify(report, null, 2)) + "\n", { mode: 0o600 });
    await rename(path.join(directory, "report.json.pending"), path.join(directory, "report.json"));
  };
  const step = async (id, status, detail) => { report.steps.push({ id, status, detail }); await save(); };
  const controller = new AbortController();
  const combined = AbortSignal.any([signal, controller.signal]);
  const timer = setTimeout(() => controller.abort(new Error("readiness deadline exceeded")), timeoutMs);
  async function ready(port, route, auth, accept) {
    while (!combined.aborted) {
      assertRunning(owned);
      try {
        const result = await localJson(port, route, auth, combined);
        assertRunning(owned);
        if (accept(result)) return result;
      } catch (error) { if (combined.aborted) throw error; }
      await delay(30, undefined, { signal: combined });
    }
    throw combined.reason;
  }
  try {
    await save();
    combined.throwIfAborted();
    report.repositories = { avian: provenance(avianRoot), arc: provenance(source) };
    report.executable_sha256 = hash(await readFile(executable));
    report.runner_sha256 = hash(await readFile(fileURLToPath(import.meta.url)));
    report.emulator_sha256 = hash(await readFile(emulatorFile));
    for (const port of [bridgePort, commandPort, emulatorPort]) await assertUnused(port);
    await step("ports", "PASS", "All three explicit loopback ports were unused before launch.");
    owned.push(startChild("emulator", process.execPath, [emulatorFile], avianRoot, isolatedEnvironment(state, {
      AVIAN_CHUD_EMULATOR_TOKEN: token, AVIAN_CHUD_EMULATOR_PORT: String(emulatorPort),
      AVIAN_CHUD_EMULATOR_STATE_FILE: path.join(state, "emulator.json"),
    })));
    const devices = await ready(emulatorPort, "/api/radio/devices", token,
      (r) => r.status === 200 && r.body?.simulated === true && r.body?.hardware_write === false);
    report.emulator_devices = devices;
    const before = await localJson(emulatorPort, "/__sim/ledger", token, combined);
    await step("emulator", "PASS", "Owned authenticated simulator is ready.");
    owned.push(startChild("bridge", executable, [...bridgePrefixArgs, "--mock", "--device-id", "SIMULATION-PREFLIGHT",
      "--port", String(commandPort), "--http-port", String(bridgePort), "--tcp-bind", "127.0.0.1",
      "--http-bind", "127.0.0.1", "--radio-management-api-url", `http://127.0.0.1:${emulatorPort}`], source,
    isolatedEnvironment(state, { ARC_DEV_BRIDGE_STATE_DIR: path.join(state, "arc"),
      RADIO_MANAGEMENT_API_TOKEN: token, ARC_RADIO_MUTATIONS_ENABLED: "false", RUST_LOG: "warn" })));
    await ready(bridgePort, "/api/health", null, (r) => r.status === 200 && r.body !== null);
    await step("bridge", "PASS", "Owned bridge health is ready in explicit mock mode; mutations disabled.");
    const networks = await localJson(bridgePort, "/api/radio/networks", null, combined);
    report.radio_control = networks;
    const after = await localJson(emulatorPort, "/__sim/ledger", token, combined);
    report.ledger = { before, after };
    if (before.status !== 200 || after.status !== 200 || !Array.isArray(before.body?.events)
      || !Array.isArray(after.body?.events) || JSON.stringify(before.body) !== JSON.stringify(after.body)) {
      throw new Error("readiness did not preserve a valid mutation ledger");
    }
    await step("no_writes", "PASS", "Discovery and readiness left the simulation mutation ledger unchanged.");
    assertRunning(owned);
    if (networks.status === 200 && Array.isArray(networks.body?.networks)) {
      await step("radio_control", "PASS", "Radio-network API is available; workflow acceptance remains outstanding.");
      report.verdict = "PASS";
    } else {
      await step("radio_control", "BLOCKED", "Mock radio-control prerequisite is unavailable; do not fall back to non-mock startup.");
      report.verdict = "BLOCKED";
    }
  } catch (error) {
    report.verdict = "FAIL";
    await step("execution", "FAIL", redact(combined.aborted ? "run cancelled or readiness deadline exceeded" : error.message));
  } finally {
    clearTimeout(timer);
    for (const entry of owned.toReversed()) {
      try { await stopChild(entry); }
      catch (error) { report.verdict = "FAIL"; report.steps.push({ id: "cleanup", status: "FAIL", detail: error.message }); }
      report.processes.push({ name: entry.name, pid: entry.child.pid, exited: entry.exited, exit_code: entry.code, signal: entry.signal });
      // Join before redaction so credentials split across stream chunks cannot leak.
      const log = redact(Buffer.concat(entry.logs).toString("utf8"));
      const name = `${entry.name}.log`;
      await writeFile(path.join(directory, name), log, { mode: 0o600 });
      report.files[name] = hash(log);
    }
    report.completed_utc = new Date().toISOString();
    await save();
  }
  return { directory, report };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const controller = new AbortController();
  for (const name of ["SIGINT", "SIGTERM"]) process.once(name, () => controller.abort());
  const names = { "--arc-root": "arcRoot", "--bridge-exe": "bridgeExe", "--output-root": "outputRoot",
    "--bridge-port": "bridgePort", "--command-port": "commandPort", "--emulator-port": "emulatorPort", "--timeout-ms": "timeoutMs" };
  try {
    const options = { signal: controller.signal };
    for (let i = 2; i < process.argv.length; i += 2) {
      const key = names[process.argv[i]], value = process.argv[i + 1];
      if (!key || !value || Object.hasOwn(options, key)) throw new Error("invalid or duplicate argument");
      options[key] = key.endsWith("Port") || key === "timeoutMs" ? Number(value) : value;
    }
    if (![options.arcRoot, options.bridgeExe, options.outputRoot].every(Boolean)) throw new Error("--arc-root, --bridge-exe and --output-root are required");
    const result = await runPreflight(options);
    console.log(`${result.report.verdict}: ${result.directory} (preflight only; workflow not qualified)`);
    process.exitCode = result.report.verdict === "PASS" ? 0 : result.report.verdict === "BLOCKED" ? 2 : 1;
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
