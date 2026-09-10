import assert from 'node:assert/strict';
import { readFile, readdir, stat } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { test } from '@ferridriver/test';
import { observation, repo, runtimeProbe, script } from './support.mjs';

const read = path => readFile(join(repo, path), 'utf8');
async function contract() {
  const { results } = await runtimeProbe([{ op: 'runtime-contract' }]);
  return observation(results[0]);
}

test('every declared extension contribution point exists in the live script runtime', async () => {
  const { contributionPoints } = await contract();
  const result = await script(`return ${JSON.stringify(contributionPoints)}.filter(name => typeof globalThis[name] !== 'function')`);
  assert.deepEqual(result.value, []);
});

test('every runtime contribution point is documented', async () => {
  const [{ contributionPoints }, docs] = await Promise.all([contract(), read('docs/extensions.md')]);
  assert.deepEqual(contributionPoints.filter(name => !docs.includes('`' + name + '`') && !docs.includes(name + '(')), []);
});

test('both extension guides describe every runtime host', async () => {
  const { hosts } = await contract();
  for (const page of ['docs/extensions.md', 'site/docs/scripting/extensions.md']) {
    const docs = await read(page);
    for (const host of hosts) assert.ok(docs.includes(`"${host}"`) || docs.includes('`ferridriver ' + host + '`'), `${page}: ${host}`);
  }
});

test('literal extension global installations are included in the public contract', async () => {
  const { contributionPoints } = await contract();
  const installed = new Set();
  for (const name of ['registry', 'bdd', 'test']) {
    const source = await read(`crates/ferridriver-script/src/bindings/${name}.rs`);
    for (const match of source.matchAll(/(?:globals\(\)|\bg)\.set\("([A-Za-z_$][A-Za-z0-9_$]*)"/g)) installed.add(match[1]);
  }
  assert.deepEqual([...installed].filter(name => !contributionPoints.includes(name)), []);
});

async function markdown(dir) {
  const result = [];
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) result.push(...await markdown(path));
    else if (path.endsWith('.md')) result.push(path);
  }
  return result;
}
async function exists(path) {
  try { await stat(path); return true; }
  catch (error) { if (error.code === 'ENOENT' || error.code === 'ENOTDIR') return false; throw error; }
}

test('internal documentation links resolve to repository files', async () => {
  const files = (await Promise.all(['docs', 'site/docs'].map(dir => markdown(join(repo, dir))))).flat();
  const dangling = [];
  for (const file of files) {
    const source = await readFile(file, 'utf8');
    const targets = new Set([...source.matchAll(/\]\(([^)]*)\)/g)].map(match => match[1].trim().split(/\s/)[0].split('#')[0]));
    for (const target of targets) {
      if (!target || target.startsWith('http') || target.startsWith('mailto:') || target.startsWith('/') || !target.includes('.')) continue;
      if (!await exists(join(dirname(file), target)) && !await exists(join(repo, target))) dangling.push(`${file} -> ${target}`);
    }
  }
  assert.deepEqual(dangling, []);
});
