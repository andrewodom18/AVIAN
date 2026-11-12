import test from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:net";
import { mkdtemp, writeFile, readFile, rm, symlink, access } from "node:fs/promises";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { runPreflight, isolatedEnvironment, validatePorts } from "../../scripts/Run-ArcEmulatorPreflight.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
async function ports() {
  const servers = [];
  try {
    for (let i = 0; i < 3; i++) {
      const s = createServer();
      await new Promise((resolve, reject) => { s.once("error", reject); s.listen(0, "127.0.0.1", resolve); });
      servers.push(s);
    }
    return servers.map((s) => s.address().port);
  } finally { await Promise.all(servers.map((s) => new Promise((resolve) => s.close(resolve)))); }
}
async function fixture(t, mode = "ready") {
  const directory = await mkdtemp(path.join(os.tmpdir(), "avian-preflight-test-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const script = path.join(directory, "bridge-fixture.mjs");
  await writeFile(script, `
import { createServer } from 'node:http';
const mode = ${JSON.stringify(mode)};
const args = process.argv.slice(2);
const arg = (key) => args[args.indexOf(key) + 1];
if (!args.includes('--mock') || arg('--http-bind') !== '127.0.0.1'
  || arg('--tcp-bind') !== '127.0.0.1' || process.env.ARC_RADIO_MUTATIONS_ENABLED !== 'false'
  || process.env.RADIO_MANAGEMENT_API_KEY_FILE || process.env.ZENOH_CONFIG || process.env.OPENAI_API_KEY) process.exit(7);
if (mode === 'exit') process.exit(6);
if (mode === 'hang') setInterval(() => {}, 1000);
else {
 const token = process.env.RADIO_MANAGEMENT_API_TOKEN;
 process.stdout.write(token.slice(0, 17));
 setTimeout(() => process.stdout.write(token.slice(17) + '\\n'), 10);
 createServer(async (req, res) => {
   if (mode === 'redirect') { res.writeHead(302, { location: 'http://192.0.2.1/' }); res.end(); return; }
   if (mode === 'write' && req.url === '/api/radio/networks') {
     await fetch(arg('--radio-management-api-url') + '/api/radio/apply', { method: 'POST',
       headers: { authorization: 'Bearer ' + token, 'content-type': 'application/json' },
       body: JSON.stringify({ mac: '00:1e:3f:20:9a:10', desired: { network_id: { value: 'CHANGED' } } }) });
   }
   res.writeHead(req.url === '/api/radio/networks' && mode === 'blocked' ? 503 : 200, {'content-type': 'application/json'});
   res.end(JSON.stringify(req.url === '/api/health' ? {status: 'ok'} : mode === 'malformed' ? {networks: {}} : {networks: []}));
 }).listen(Number(arg('--http-port')), '127.0.0.1');
}
`);
  const [bridgePort, commandPort, emulatorPort] = await ports();
  return { arcRoot: root, bridgeExe: process.execPath, bridgePrefixArgs: [script], outputRoot: directory,
    bridgePort, commandPort, emulatorPort, timeoutMs: 3000 };
}

test("only distinct explicit unprivileged ports and isolated environment are allowed", () => {
  for (const p of [[1023, 2000, 3000], [2000, 2000, 3000], [2000, 3000, NaN], [2000, 3000, 65536]]) {
    assert.throws(() => validatePorts(p));
  }
  const env = isolatedEnvironment('/disposable');
  assert.equal(env.HOME, '/disposable');
  for (const key of ['RADIO_MANAGEMENT_API_KEY_FILE', 'ZENOH_CONFIG', 'HTTP_PROXY', 'NODE_OPTIONS', 'OPENAI_API_KEY']) assert.equal(env[key], undefined);
});

test("real emulator plus fixture bridge proves preflight, redaction and owned cleanup only", async (t) => {
  const options = await fixture(t);
  const { directory, report } = await runPreflight(options);
  assert.equal(report.verdict, 'PASS');
  assert.equal(report.fixture_bridge, true);
  assert.equal(report.workflow_qualified, false);
  assert.deepEqual(report.ledger.before.body.events, []);
  assert.deepEqual(report.ledger.after.body.events, []);
  assert.equal(report.processes.length, 2);
  assert.ok(report.processes.every((p) => p.exited));
  assert.match(await readFile(path.join(directory, 'bridge.log'), 'utf8'), /\[REDACTED\]/);
  assert.deepEqual(JSON.parse(await readFile(path.join(directory, 'report.json'), 'utf8')), report);
  for (const port of [options.bridgePort, options.emulatorPort]) {
    const s = createServer();
    await new Promise((resolve, reject) => { s.once('error', reject); s.listen(port, '127.0.0.1', resolve); });
    await new Promise((resolve) => s.close(resolve));
  }
});

for (const mode of ['blocked', 'malformed']) test(`${mode} radio control cannot pass`, async (t) => {
  const { report } = await runPreflight(await fixture(t, mode));
  assert.equal(report.verdict, 'BLOCKED');
  assert.ok(report.processes.every((p) => p.exited));
});

for (const mode of ['exit', 'hang', 'redirect']) test(`${mode} bridge cannot pass and is cleaned up`, async (t) => {
  const options = await fixture(t, mode);
  const { report } = await runPreflight({ ...options, timeoutMs: 700 });
  assert.equal(report.verdict, 'FAIL');
  assert.ok(report.processes.every((p) => p.exited));
});

test("occupied port fails before launch without stopping its owner", async (t) => {
  const options = await fixture(t);
  const s = createServer();
  await new Promise((resolve) => s.listen(options.bridgePort, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => s.close(resolve)));
  const { report } = await runPreflight(options);
  assert.equal(report.verdict, 'FAIL');
  assert.deepEqual(report.processes, []);
  assert.equal(s.listening, true);
});

test("cancellation saves incomplete evidence as failure and terminates children", async (t) => {
  const options = await fixture(t, 'hang');
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 200);
  const { report } = await runPreflight({ ...options, signal: controller.signal });
  clearTimeout(timer);
  assert.equal(report.verdict, 'FAIL');
  assert.ok(report.processes.every((p) => p.exited));
});

test("output symlinks into source are rejected before creating evidence", async (t) => {
  const options = await fixture(t);
  const link = path.join(options.outputRoot, 'source');
  await symlink(root, link, process.platform === 'win32' ? 'junction' : 'dir');
  const forbidden = path.join(link, 'forbidden-preflight-output');
  await assert.rejects(runPreflight({ ...options, outputRoot: forbidden }), /outside source/);
  await assert.rejects(access(forbidden));
});

test("a mutation during read-only readiness fails ledger acceptance", async (t) => {
  const { report } = await runPreflight(await fixture(t, 'write'));
  assert.equal(report.verdict, 'FAIL');
  assert.ok(report.ledger.after.body.events.length > 0);
  assert.match(report.steps.at(-1).detail, /mutation ledger/);
});
