import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { passed, repo, run, workspace } from './support.mjs';

async function script(source, files = {}, args = []) {
  const cwd = await workspace(files);
  const result = await run(['run', '--no-inherit', '--json', ...args, '-e', source], { cwd });
  passed(result);
  const response = JSON.parse(result.stdout);
  assert.equal(response.status, 'ok', result.text);
  return response.value;
}

function config(name) {
  return { 'ferridriver.toml': `[[sidecars]]\nname = "${name}"\ncommand = [${JSON.stringify(join(repo, 'target/debug/sidecar_echo'))}]\n` };
}

test('a sidecar declared in configuration is reachable from a script', async () => {
  const value = await script(`const sc = await sidecars.connect('echo');
    try { return await sc.send('ping'); } finally { await sc.close(); }`, config('echo'));
  assert.equal(value.ok, true);
});

test('an extension exchanges commands and pushed events with its declared sidecar', async () => {
  const value = await script(`const ping = await tools['gateway.ping']();
    const echoed = await tools['gateway.call']({ method: 'echo', params: { n: 7 } });
    const evt = await tools['gateway.roundtripEvent']({ event: 'tick', params: { event: 'tick', payload: { hits: 3 } } });
    await tools['gateway.close']();
    return { ping, echoed, evt };`, config('gateway'), [
    '--extension', join(repo, 'crates/ferridriver-script/tests/fixtures/sidecar_gateway.ts'),
  ]);
  assert.equal(value.ping.ok, true);
  assert.equal(value.ping.name, 'gateway');
  assert.deepEqual(value.echoed, { n: 7 });
  assert.deepEqual(value.evt, { hits: 3 });
});

test('a script rejects a sidecar that was not declared', async () => {
  const value = await script(`try { await sidecars.connect('echo'); return 'no-throw'; }
    catch (error) { return String(error.message || error); }`);
  assert.match(value, /unknown sidecar/);
});
