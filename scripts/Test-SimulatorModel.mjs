// Tests the exact dependency-free-of-PEAT model source in an external Cargo package.
// This is partial unit evidence and never replaces the workspace's locked gates.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { validateReport } from "../simulators/mesh-operations/schema-validation.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const destination = path.resolve(process.env.AVIAN_VALIDATION_OUTPUT ?? path.join(os.homedir(), "Downloads", "AVIAN-validation"));
mkdirSync(destination, { recursive: true });
const resolved = realpathSync(destination);
if (resolved === root || resolved.startsWith(`${root}${path.sep}`)) throw new Error("Evidence must be outside the repository");
const output = mkdtempSync(path.join(resolved, "model-unit-"));
const scratch = mkdtempSync(path.join(os.tmpdir(), "avian-model-unit-"));
const manifest = { schema_version: "1.0.0", scope: "isolated model source; NOT workspace, ARC, PEAT, RF or hardware acceptance",
  verdict: "FAIL", checks: [], files: {}, source_hashes: {} };
const hash = (bytes) => createHash("sha256").update(bytes).digest("hex");
function save(name, data) { writeFileSync(path.join(output, name), data); manifest.files[name] = hash(data); }
function run(name, command, args, cwd = root) {
  const result = spawnSync(command, args, { cwd, encoding: "utf8", maxBuffer: 128 * 1024 * 1024, timeout: 180000 });
  save(`${name}.log`, `${result.stdout ?? ""}\n${result.stderr ?? ""}\n${result.error?.message ?? ""}`);
  manifest.checks.push({ name, command, args, status: result.status, signal: result.signal });
  if (result.error || result.status !== 0) throw new Error(`${name} failed; see ${output}`);
  return result.stdout;
}
try {
  manifest.commit = run("git-head", "git", ["rev-parse", "HEAD"]).trim();
  manifest.dirty = run("git-status", "git", ["status", "--porcelain"]);
  manifest.rustc = run("rustc-version", "rustc", ["--version"]).trim();
  manifest.node = process.version;
  manifest.platform = { os: process.platform, arch: process.arch, release: os.release() };
  manifest.runner_sha256 = hash(readFileSync(fileURLToPath(import.meta.url)));
  for (const name of ["validation.rs", "transport.rs"]) {
    manifest.source_hashes[name] = hash(readFileSync(path.join(root, "simulators/mesh-operations/mesh-sim/src", name)));
  }
  // This intentionally has its own lockfile. Record it; never modify or claim the workspace lock.
  const workspace = readFileSync(path.join(root, "Cargo.toml"), "utf8");
  const version = (name) => {
    const match = workspace.match(new RegExp(`^${name} = (?:\\{ version = )?"([^"]+)"`, "m"));
    if (!match) throw new Error(`missing workspace constraint for ${name}`);
    return match[1];
  };
  const source = path.join(root, "simulators/mesh-operations/mesh-sim/src/validation.rs");
  writeFileSync(path.join(scratch, "Cargo.toml"), `[package]\nname = "avian-model-unit-check"\nversion = "0.0.0"\nedition = "2021"\n[lib]\npath = ${JSON.stringify(source)}\n[dependencies]\nserde = { version = "${version("serde")}", features = ["derive"] }\nserde_json = "${version("serde_json")}"\nsha2 = "${version("sha2")}"\n`);
  mkdirSync(path.join(scratch, "src"));
  writeFileSync(path.join(scratch, "src/main.rs"), 'fn main() { let seed = std::env::args().nth(1).unwrap().parse().unwrap(); println!("{}", serde_json::to_string(&avian_model_unit_check::run_validation_matrix(seed)).unwrap()); }\n');
  run("unit-tests", "cargo", ["test", "--offline"], scratch);
  save("isolated-Cargo.lock", readFileSync(path.join(scratch, "Cargo.lock")));
  manifest.dependency_scope = "separate offline resolution of workspace semver constraints; not the workspace Cargo.lock";
  run("clippy", "cargo", ["clippy", "--offline", "--all-targets", "--", "-D", "warnings"], scratch);
  for (const seed of [20260825, 20260929, 43]) {
    const report = validateReport(JSON.parse(run(`seed-${seed}`, "cargo", ["run", "--offline", "--quiet", "--", String(seed)], scratch)));
    if (!report.passed) throw new Error(`seed ${seed} failed`);
    // Nested schema negatives use real emitted reports, not hand-written imitations.
    for (const mutate of [
      (r) => { r.scenarios[0].config.transport.ttl_ms = "1"; },
      (r) => { r.scenarios[0].metrics.delivery_ratio = 2; },
      (r) => { r.scenarios[0].metrics.extra = true; },
      (r) => { delete r.scenarios[0].events[0].outcome; },
    ]) {
      const invalid = structuredClone(report); mutate(invalid);
      let rejected = false;
      try { validateReport(invalid); } catch { rejected = true; }
      if (!rejected) throw new Error("schema accepted malformed nested evidence");
    }
    save(`seed-${seed}.json`, `${JSON.stringify(report)}\n`);
  }
  manifest.executable_sha256 = hash(readFileSync(path.join(scratch, "target/debug", process.platform === "win32" ? "avian-model-unit-check.exe" : "avian-model-unit-check")));
  for (const name of ["transport-event-v1.schema.json", "validation-report-v2.schema.json"]) {
    manifest.source_hashes[name] = hash(readFileSync(path.join(root, "simulators/mesh-operations/schemas", name)));
  }
  manifest.verdict = "PASS";
} catch (error) {
  manifest.error = error.message;
  process.exitCode = 1;
} finally {
  writeFileSync(path.join(output, "manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  rmSync(scratch, { recursive: true, force: true });
  console.log(`${manifest.verdict}: isolated model evidence: ${output}`);
}
