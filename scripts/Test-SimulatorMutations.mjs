// Selected behavior mutations against disposable exact-source copies, not workspace qualification.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = realpathSync(path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..'));
const outputRoot = process.env.AVIAN_VALIDATION_OUTPUT;
if (!outputRoot) throw new Error('AVIAN_VALIDATION_OUTPUT must name an external evidence directory');
// Require an existing, canonical external directory before creating output.
const destination = realpathSync(outputRoot);
const relative = path.relative(root, destination);
if (!relative || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative))) throw new Error('Evidence must be outside the repository');
const output = mkdtempSync(path.join(destination, 'model-mutations-'));
const scratch = mkdtempSync(path.join(os.tmpdir(), 'avian-mutations-'));
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
const manifest = { schema_version: 1, scope: 'selected isolated model mutations; NOT full mutation, locked workspace, PEAT, ARC or hardware qualification', verdict: 'FAIL', results: [], files: {}, source_hashes: {} };
function save(name, bytes) { writeFileSync(path.join(output, name), bytes); manifest.files[name] = hash(bytes); }
function command(name, args) {
  const result = spawnSync('cargo', ['+1.91.1', ...args], { cwd: scratch, encoding: 'utf8', timeout: 120000, maxBuffer: 8 * 1024 * 1024 });
  const log = `${result.stdout ?? ''}\n${result.stderr ?? ''}\n${result.error?.message ?? ''}`;
  save(`${name}.log`, log);
  return { status: result.status, signal: result.signal, error: result.error?.code, log };
}
const tests = {
  queue: 'overflow_ttl_and_retry_limits_are_real_terminal_outcomes',
  duplicate: 'duplicate_and_reordered_commands_cannot_replay_or_regress_state',
  bandwidth: 'bandwidth_serializes_shared_queue_and_measures_convergence',
  link: 'a_cut_link_blocks_in_flight_packets_until_restoration',
  node: 'partition_recovery_requires_delivery_after_restoration',
  ttl: 'delivery_at_ttl_boundary_expires',
};
// Each replacement must match exactly once. Source drift fails the run instead of silently skipping a mutant.
const mutants = [
  ['queue-bound', tests.queue, [['finishes.len() >= self.policy.queue_capacity', 'finishes.len() > self.policy.queue_capacity']]],
  ['retry-bound', tests.queue, [['attempt < self.policy.retry_limit', 'attempt <= self.policy.retry_limit']]],
  ['command-application', tests.bandwidth, [['self.result.commands_applied += 1;', 'self.result.commands_applied += 0;']]],
  ['generation-update', tests.duplicate, [['self.result.generations[to] = message.generation;', 'self.result.generations[to] = 0;']]],
  ['duplicate-rejection', tests.duplicate, [['if self.seen.contains(&(to, message.id)) {', 'if false && self.seen.contains(&(to, message.id)) {']]],
  ['ttl-boundary', tests.ttl, [
    ['message.created_ms + policy.ttl_ms, Action::Expire(index)', 'message.created_ms + policy.ttl_ms + 1, Action::Expire(index)'],
    ['if at >= message.created_ms + self.policy.ttl_ms {\n            self.drop_message(at, index, "ttl_expired");\n            return;\n        }\n        if self.online[message.source]', 'if at > message.created_ms + self.policy.ttl_ms {\n            self.drop_message(at, index, "ttl_expired");\n            return;\n        }\n        if self.online[message.source]'],
    ['if at >= message.created_ms + self.policy.ttl_ms {\n            self.drop_message(at, index, "ttl_expired");\n            return;\n        }\n        if !self.online[from]', 'if at > message.created_ms + self.policy.ttl_ms {\n            self.drop_message(at, index, "ttl_expired");\n            return;\n        }\n        if !self.online[from]'],
  ]],
  ['bandwidth-serialization', tests.bandwidth, [['finishes.back().copied().unwrap_or(at).max(at)', 'at']]],
  ['link-outage', tests.link, [['self.disabled_links.contains(&(from.min(to), from.max(to)))', '(false && self.disabled_links.contains(&(from.min(to), from.max(to))))']]],
  ['node-outage', tests.node, [['engine.online[node] = online;', 'engine.online[node] = true;']]],
  ['stale-generation', tests.duplicate, [['message.generation < self.result.generations[to]', 'message.generation > self.result.generations[to]']]],
  ['convergence-time', tests.bandwidth, [['.or_insert(at);', '.or_insert(0);']]],
  ['delivered-accounting', tests.bandwidth, [['self.result.delivered += 1;', 'self.result.delivered += 0;']]],
];
try {
  manifest.commit = spawnSync('git', ['-C', root, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).stdout.trim();
  manifest.dirty = spawnSync('git', ['-C', root, 'status', '--porcelain'], { encoding: 'utf8' }).stdout;
  manifest.platform = { os: process.platform, arch: process.arch };
  manifest.runner_sha256 = hash(readFileSync(fileURLToPath(import.meta.url)));
  const version = command('rust-toolchain', ['--version']);
  if (version.status !== 0) throw new Error('Pinned toolchain unavailable');
  manifest.cargo = version.log.trim();
  manifest.rustc = spawnSync('rustc', ['+1.91.1', '--version'], { encoding: 'utf8' }).stdout.trim();
  let original;
  for (const name of ['validation.rs', 'transport.rs']) {
    const bytes = readFileSync(path.join(root, 'simulators/mesh-operations/mesh-sim/src', name));
    manifest.source_hashes[name] = hash(bytes);
    writeFileSync(path.join(scratch, name), bytes);
    if (name === 'transport.rs') original = bytes.toString('utf8');
  }
  const workspace = readFileSync(path.join(root, 'Cargo.toml'), 'utf8');
  const constraint = (name) => {
    const match = workspace.match(new RegExp(`^${name} = (?:\\{ version = )?"([^"]+)"`, 'm'));
    if (!match) throw new Error(`Missing dependency constraint: ${name}`);
    return match[1];
  };
  const cargo = `[package]\nname = "avian-selected-mutations"\nversion = "0.0.0"\nedition = "2021"\n[lib]\npath = "validation.rs"\n[dependencies]\nserde = { version = "${constraint('serde')}", features = ["derive"] }\nserde_json = "${constraint('serde_json')}"\nsha2 = "${constraint('sha2')}"\n`;
  writeFileSync(path.join(scratch, 'Cargo.toml'), cargo); save('isolated-Cargo.toml', cargo);
  const baseline = command('baseline', ['test', '--offline', '--lib']);
  if (baseline.status !== 0) throw new Error('Unmodified baseline failed');
  save('isolated-Cargo.lock', readFileSync(path.join(scratch, 'Cargo.lock')));
  for (const [name, test, replacements] of mutants) {
    let source = original;
    for (const [before, after] of replacements) {
      if (source.split(before).length !== 2) throw new Error(`Source mismatch for ${name}`);
      source = source.replace(before, after);
    }
    save(`${name}.patch.json`, JSON.stringify({ file: 'transport.rs', replacements }, null, 2));
    writeFileSync(path.join(scratch, 'transport.rs'), source);
    const build = command(`${name}-build`, ['test', '--offline', '--locked', '--lib', '--no-run']);
    let disposition, result = build;
    if (build.error === 'ETIMEDOUT') disposition = 'timeout';
    else if (build.error || build.signal) disposition = 'infrastructure_failure';
    else if (build.status !== 0) disposition = 'compile_failure';
    else {
      result = command(name, ['test', '--offline', '--locked', '--lib', `transport::tests::${test}`, '--', '--exact', '--nocapture']);
      disposition = result.error === 'ETIMEDOUT' ? 'timeout'
        : result.error || result.signal ? 'infrastructure_failure'
          : result.status === 0 ? 'survived'
            : /running 1 test/.test(result.log) && /assertion/.test(result.log) && /panicked at/.test(result.log) ? 'caught' : 'infrastructure_failure';
    }
    manifest.results.push({ name, test, disposition, exit_code: result.status, signal: result.signal, mutant_sha256: hash(source) });
    writeFileSync(path.join(output, 'manifest.json'), JSON.stringify(manifest, null, 2));
    console.log(`${name}: ${disposition}`);
  }
  for (const [name, expected] of Object.entries(manifest.source_hashes)) {
    if (hash(readFileSync(path.join(root, 'simulators/mesh-operations/mesh-sim/src', name))) !== expected) throw new Error('Source changed during mutation run');
  }
  manifest.verdict = manifest.results.every((r) => r.disposition === 'caught') ? 'PASS' : 'FAIL';
} catch (error) { manifest.error = error.message; }
finally {
  writeFileSync(path.join(output, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
  rmSync(scratch, { recursive: true, force: true });
  console.log(`${manifest.verdict}: selected mutation evidence: ${output}`);
  if (manifest.verdict !== 'PASS') process.exitCode = 1;
}
