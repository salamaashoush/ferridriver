import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

async function invoke(extension, sources) {
  const { results } = await runtimeProbe([
    { op: 'load-extensions', entries: ['extension.ts'] },
    ...sources.map(source => ({ op: 'run-session', source })),
  ], { 'extension.ts': extension });
  assert.deepEqual(observation(results[0]), { count: 1, failures: [] });
  return results.slice(1).map(observation);
}

function value(result) {
  assert.equal(result.status, 'ok', JSON.stringify(result));
  return result.value;
}

function error(result) {
  assert.equal(result.status, 'error', JSON.stringify(result));
  return result.error.message;
}

test('extension commands pass shell metacharacters literally in argv form', async () => {
  const literal = '$(touch /tmp/ferri_pwned); a && b';
  const [result] = await invoke(`
    defineTool({ name: 't', allow: { commands: { e: { run: ['echo', '\${m}'] } } },
      handler: async ({ args, commands }) => commands.run('e', { m: args.m }) });
  `, [`return await tools.t({ m: ${JSON.stringify(literal)} });`]);
  assert.equal(value(result), literal);
});

test('extension commands decode text, JSON and nonempty lines', async () => {
  const results = await invoke(String.raw`
    defineTool({ name: 'j', allow: { commands: { c: { run: "printf '{\"a\":1}'", output: 'json' } } },
      handler: async ({ commands }) => commands.run('c') });
    defineTool({ name: 'l', allow: { commands: { c: { run: "printf 'a\nb\n\nc\n'", output: 'lines' } } },
      handler: async ({ commands }) => commands.run('c') });
    defineTool({ name: 'x', allow: { commands: { c: 'echo hi' } },
      handler: async ({ commands }) => commands.run('c') });
  `, ['return await tools.j();', 'return await tools.l();', 'return await tools.x();']);
  assert.deepEqual(results.map(value), [{ a: 1 }, ['a', 'b', 'c'], 'hi']);
});

test('extension commands reject missing placeholders and undeclared command names', async () => {
  const results = await invoke(`
    defineTool({ name: 'missing', allow: { commands: { c: 'echo \${name}' } },
      handler: async ({ commands }) => commands.run('c', {}) });
    defineTool({ name: 'denied', allow: { commands: { allowed: 'echo ok' } },
      handler: async ({ commands }) => commands.run('other') });
  `, ['return await tools.missing();', 'return await tools.denied();']);
  assert.match(error(results[0]), /\$\{name\}/);
  assert.match(error(results[1]), /not in the commands allow-list/);
});

test('extension command timeouts terminate a blocked process within two seconds', async () => {
  const started = performance.now();
  const [result] = await invoke(`
    defineTool({ name: 't', allow: { commands: { slow: {
      run: 'mkfifo hold; exec 3<> hold; read signal <&3', timeoutMs: 150
    } } }, handler: async ({ commands }) => commands.run('slow') });
  `, ['return await tools.t();']);
  assert.match(error(result), /timed out after 150ms/);
  assert.ok(performance.now() - started < 2000);
});

test('extension commands reject persistent specs in run and one-shot specs in start', async () => {
  const results = await invoke(`
    defineTool({ name: 'p', allow: { commands: { srv: { run: 'cat', persistent: true } } },
      handler: async ({ commands }) => commands.run('srv') });
    defineTool({ name: 'o', allow: { commands: { one: 'echo hi' } },
      handler: async ({ commands }) => commands.start('one') });
  `, ['return await tools.p();', 'return await tools.o();']);
  assert.match(error(results[0]), /persistent/);
  assert.match(error(results[1]), /not declared `persistent`/);
});

test('extension persistent commands retain output across calls and remove their record on stop', async () => {
  const results = await invoke(`
    const spec = { run: 'echo up; mkfifo hold; exec 3<> hold; read signal <&3', persistent: true };
    defineTool({ name: 'srv', allow: { commands: { s: spec } }, handler: async ({ args, commands }) => {
      if (args.op === 'start') return await commands.start('s');
      if (args.op === 'status') {
        await commands.waitForOutput('s', 'up');
        return commands.status('s');
      }
      if (args.op === 'stop') { await commands.stop('s'); return 'stopped'; }
    }});
  `, [
    "return await tools.srv({ op: 'start' });",
    "return await tools.srv({ op: 'status' });",
    "return await tools.srv({ op: 'stop' });",
    "return await tools.srv({ op: 'status' });",
  ]);
  assert.ok(value(results[0]).pid > 0);
  assert.equal(value(results[1]).running, true);
  assert.match(value(results[1]).stdout, /up/);
  assert.equal(value(results[2]), 'stopped');
  assert.match(error(results[3]), /no persistent process/);
});
