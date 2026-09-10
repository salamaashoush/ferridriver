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

test('sidecar binding connect send close from js', async () => {
  const value = await script(`
    const sc = await sidecars.connect('echo');
    const ping = await sc.send('ping');
    const echoed = await sc.send('echo', { hello: 'world' });
    await sc.close();
    return { ok: ping.ok === true, echoed };
  `, config('echo'));
  assert.deepEqual(value, { ok: true, echoed: { hello: 'world' } });
});

test('sidecar binding on delivers pushed events to js', async () => {
  const value = await script(`
    const sc = await sidecars.connect('echo');
    let resolve;
    const got = new Promise((r) => { resolve = r; });
    sc.on('evt', (p) => resolve(p));
    await sc.send('emit', { event: 'evt', payload: { hi: 5 } });
    const payload = await got;
    await sc.close();
    return payload;
  `, config('echo'));
  assert.deepEqual(value, { hi: 5 });
});

test('sidecar binding once resolves with next event', async () => {
  const value = await script(`
    const sc = await sidecars.connect('echo');
    const p = sc.once('done');
    await sc.send('emit', { event: 'done', payload: { v: 'ok' } });
    const out = await p;
    await sc.close();
    return out;
  `, config('echo'));
  assert.deepEqual(value, { v: 'ok' });
});

test('sidecar binding unsubscribe and off stop delivery', async () => {
  const value = await script(`
    const sc = await sidecars.connect('echo');
    let count = 0;
    const off = sc.on('evt', () => { count++; });
    off();
    sc.on('evt', () => { count++; });
    sc.off('evt');
    let openGate;
    const gate = new Promise((r) => { openGate = r; });
    sc.on('gate', () => openGate());
    await sc.send('emit', { event: 'evt', payload: {} });
    await sc.send('emit', { event: 'gate', payload: {} });
    await gate;
    await sc.close();
    return count;
  `, config('echo'));
  assert.deepEqual(value, 0);
});

test('sidecar binding unknown sidecar name rejects', async () => {
  const value = await script(`
    try { await sidecars.connect('does-not-exist'); return 'no-throw'; }
    catch (e) { return String(e.message || e); }
  `, config('echo'));
  assert.ok(value.includes('unknown sidecar'));
});

test('sidecar binding send many from js returns results array', async () => {
  const value = await script(`
    const sc = await sidecars.connect('echo');
    const out = await sc.sendMany([
      { method: 'echo', params: { a: 1 } },
      { method: 'ping' },
      { method: 'echo', params: 'three' },
    ]);
    await sc.close();
    return out;
  `, config('echo'));
  assert.deepEqual(value, [{ a: 1 }, { ok: true }, 'three']);
});

test('sidecar binding send many from js rejects on a remote error', async () => {
  const value = await script(`
    const sc = await sidecars.connect('echo');
    let msg = 'no-throw';
    try { await sc.sendMany([{ method: 'ping' }, { method: '__nope__' }]); }
    catch (e) { msg = String(e.message || e); }
    await sc.close();
    return msg;
  `, config('echo'));
  assert.ok(value.includes('unknown method'));
});

test('sidecar binding connect after close respawns instead of returning the corpse', async () => {
  const value = await script(`
    const first = await sidecars.connect('echo');
    await first.send('ping');
    await first.close();
    const second = await sidecars.connect('echo');
    const ping = await second.send('ping');
    await second.close();
    return { ok: ping.ok === true };
  `, config('echo'));
  assert.deepEqual(value, { ok: true });
});
