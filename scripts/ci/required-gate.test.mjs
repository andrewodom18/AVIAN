import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { test } from 'node:test';

// Exercise the actual workflow shell, rather than a second implementation of
// the verdict. This intentionally supports this repository's explicit, literal
// aggregate block only; a change to that format needs a corresponding review.
const workflow = readFileSync(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8').replaceAll('\r\n', '\n');
const recipes = readFileSync(new URL('../../justfile', import.meta.url), 'utf8').replaceAll('\r\n', '\n');
const jobs = workflow.split(/^jobs:\s*$/m)[1];
assert.ok(jobs, 'workflow must declare jobs');
const required = jobs.match(/^  required:\n([\s\S]*?)(?=^  [\w-]+:|$(?![\s\S]))/m)?.[1];
assert.ok(required, 'workflow must declare the required job');
const needsBlock = required.match(/^    needs:\n((?:      - [\w-]+\n)+)/m)?.[1];
assert.ok(needsBlock, 'required job must enumerate its dependencies');
const dependencies = [...needsBlock.matchAll(/^      - ([\w-]+)$/gm)].map((match) => match[1]);
const bindings = [...required.matchAll(/^          ([A-Z_]+): \$\{\{ needs\.([\w-]+)\.result \}\}$/gm)]
  .map((match) => ({ variable: match[1], job: match[2] }));
const literal = required.match(/^        run: \|\n((?:(?: {10}[^\n]*|)\n?)+)/m)?.[1];
assert.ok(literal, 'required job must expose its literal verdict shell');
const script = literal.split('\n').map((line) => line.startsWith('          ') ? line.slice(10) : line).join('\n');

test('aggregate keeps its stable identity and runs after unsuccessful dependencies', () => {
  assert.match(required, /^    name: AVIAN CI required$/m);
  assert.match(required, /^    if: always\(\)$/m);
});

test('every required lane has exactly one dependency and result binding', () => {
  // New informational jobs need not become required merely by being added.
  // Changing the required contract itself is an explicit policy review.
  const laneIds = ['simulator-fast', 'dependency-audit', 'rust-quality',
    'feature-and-release', 'coverage', 'radio-mutations', 'container-build',
    'powershell-quality', 'rf-planning'].sort();
  const definedJobs = [...jobs.matchAll(/^  ([\w-]+):$/gm)].map((match) => match[1]);
  assert.ok(laneIds.every((id) => definedJobs.includes(id)), 'required lanes must exist');
  assert.equal(new Set(dependencies).size, dependencies.length);
  assert.equal(new Set(bindings.map(({ variable }) => variable)).size, bindings.length);
  assert.deepEqual([...dependencies].sort(), laneIds);
  assert.deepEqual(bindings.map(({ job }) => job).sort(), laneIds);
});

test('vendor mutations cannot skip their unmodified baseline', () => {
  const recipe = recipes.match(/^mutate-radio:\n((?: {4}[^\n]*\n?)+)/m)?.[1];
  assert.ok(recipe, 'mutation recipe must exist');
  assert.match(recipe, /cargo mutants/);
  assert.doesNotMatch(recipe, /--baseline(?:=|\s+)skip\b/);
  assert.match(recipe, /-- --locked/);
  assert.match(recipe, /node scripts\/ci\/check-radio-mutations\.mjs target\/ci-radio-mutations\/mutants\.out/);
});

function verdict(overrides = {}) {
  const env = { PATH: process.env.PATH, ...Object.fromEntries(bindings.map(({ variable }) => [variable, 'success'])), ...overrides };
  const result = spawnSync('bash', ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script], {
    env, encoding: 'utf8', timeout: 5000,
  });
  assert.ifError(result.error);
  assert.equal(result.signal, null, 'verdict must exit rather than time out');
  return result.status;
}

test('aggregate accepts all lanes succeeding', () => {
  assert.equal(verdict(), 0);
});

for (const { variable, job } of bindings) {
  for (const state of ['failure', 'cancelled', 'skipped', '']) {
    test(`aggregate rejects ${job}=${state || 'missing'}`, () => {
      assert.notEqual(verdict({ [variable]: state }), 0);
    });
  }
}
