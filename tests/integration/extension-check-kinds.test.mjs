import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

async function check(name, entries, files) {
  const cwd = await workspace({ ...files, 'package.json': JSON.stringify({
    name: `@acme/${name}`, type: 'module', ferridriver: { entries },
  }) });
  const result = await run(['ext', 'check', '.', '--json', '--no-typecheck', '--no-inherit'], { cwd });
  passed(result);
  return JSON.parse(result.stdout);
}

function kinds(report, host) {
  const counts = {};
  for (const entry of report.hosts[host].entries) {
    for (const [kind, count] of Object.entries(entry.kinds)) counts[kind] = (counts[kind] ?? 0) + count;
  }
  return counts;
}

test('fixture-only extensions report their fixtures without inventing a tool count', async () => {
  const report = await check('fixtures', ['./fixtures.ts'], {
    'fixtures.ts': "defineFixtures({ acmeUser: async ({}, use) => { await use('sashoush'); } });",
  });
  assert.equal(report.ok, true);
  const registered = kinds(report, 'test');
  assert.ok(registered.fixtures >= 1);
  assert.ok(!Object.hasOwn(registered, 'tools'));
});

test('host branches report exactly the registrations that execute in that host', async () => {
  const report = await check('branch', ['./plug.ts'], {
    'plug.ts': `if (ferridriver.host === 'bdd') {
      Given('a cart with {int} items', async () => {});
    } else if (ferridriver.host === 'mcp') {
      defineTool({ name: 'acme_ping', description: 'ping', handler: async () => 'pong' });
    }`,
  });
  const bdd = kinds(report, 'bdd');
  const mcp = kinds(report, 'mcp');
  assert.equal(bdd.steps, 1);
  assert.ok(!Object.hasOwn(bdd, 'tools'));
  assert.equal(mcp.tools, 1);
  assert.ok(!Object.hasOwn(mcp, 'steps'));
});

test('host-restricted entries remain absent from other hosts while shared entries load', async () => {
  const report = await check('narrowed', [{ path: './mcp.ts', hosts: ['mcp'] }, './shared.ts'], {
    'mcp.ts': "defineTool({ name: 'acme_only', description: 'x', handler: async () => 'y' });",
    'shared.ts': 'defineFixtures({ acmeShared: async ({}, use) => { await use(1); } });',
  });
  const paths = host => report.hosts[host].entries.flatMap(entry => entry.files);
  assert.ok(paths('mcp').some(path => path.endsWith('mcp.ts')));
  assert.ok(paths('mcp').some(path => path.endsWith('shared.ts')));
  assert.ok(!paths('test').some(path => path.endsWith('mcp.ts')));
  assert.ok(paths('test').some(path => path.endsWith('shared.ts')));
});
