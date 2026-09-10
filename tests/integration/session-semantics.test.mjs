import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';
import { scripts } from './fixtures/session-scripts.mjs';

async function execute(name, timeoutMs) {
  const { results } = await runtimeProbe(scripts[name].flatMap(source => [
    { op: 'execute-script', source, timeoutMs }, { op: 'session-state' },
  ]));
  return scripts[name].map((_, i) => ({ result: observation(results[i * 2]), ...observation(results[i * 2 + 1]) }));
}

function success(call) {
  assert.equal(call.result.status, 'ok', JSON.stringify(call));
  assert.equal(call.poisoned, false);
  return call.result.value;
}

test('session semantics keep global functions across executions', async () => {
  const [first, second] = await execute('globals_persist_across_executions');
  success(first);
  assert.equal(success(second), 42);
});

test('session semantics keep lexical declarations local to each call', async () => {
  const [first, second] = await execute('let_const_inside_call_do_not_persist');
  assert.equal(success(first), 5);
  assert.equal(success(second), 'undefined');
});

test('session semantics preserve state after ordinary exceptions without poisoning', async () => {
  const [seed, thrown, after] = await execute('plain_throw_does_not_poison_and_state_survives');
  success(seed);
  assert.equal(thrown.result.status, 'error');
  assert.equal(thrown.result.error.kind, 'runtime');
  assert.ok(thrown.result.error.message.includes('boom'));
  assert.equal(thrown.poisoned, false);
  assert.equal(success(after), 'alive');
});

for (const name of ['timeout_poisons_the_session', 'native_await_park_hits_the_backstop_and_poisons']) {
  test(`session semantics ${name.replaceAll('_', ' ')}`, async () => {
    const [call] = await execute(name, 150);
    assert.equal(call.result.status, 'error');
    assert.equal(call.result.error.kind, 'timeout');
    assert.equal(call.poisoned, true);
  });
}

test('session semantics clear completed deadlines before later VM event-loop entry', async () => {
  const { results } = await runtimeProbe([
    { op: 'execute-script', source: scripts.finished_call_deadline_does_not_halt_later_vm_entry[0], timeoutMs: 100 },
    { op: 'session-state' },
    { op: 'enter-vm', idleMs: 250, source: 'let s = 0; for (let i = 0; i < 1e6; i++) s += i; s' },
  ]);
  success({ result: observation(results[0]), ...observation(results[1]) });
  assert.equal(observation(results[2]), 499999500000);
});

test('session semantics refresh framework bindings against the durable store', async () => {
  const [first, second] = await execute('framework_globals_refresh_each_call');
  success(first);
  assert.equal(success(second), 'v');
});

test('session semantics retain timer handles for cancellation by a later call', async () => {
  const [first, second] = await execute('timer_handle_persists_and_clears_across_calls');
  success(first);
  assert.equal(success(second), false);
});

for (const [name, expected] of [
  ['set_timeout_resolves_inside_execute', 7],
  ['url_and_search_params_work', ['1', '2,3']],
  ['web_polyfills_text_codec_base64_microtask', { len: 5, dec: 'hi€', b64: 'aGk=', round: 'xy', mt: 1 }],
  ['assertion_failures_throw_a_named_assertion_error', { name: 'AssertionError', isError: true }],
  ['set_timeout_passes_extra_args_and_clear_tolerates_garbage', 'xy'],
  ['url_search_params_binding_is_live_in_both_directions', {
    afterAppend: ['https://ex.com/p?a=1&b=2', '?a=1&b=2'], afterSearchSet: ['3', false, 1], sameObject: true, afterHref: '4',
  }],
  ['text_codecs_cover_utf16_and_the_stream_forms', { utf16: 'hi', text: 'hi€', encoding: 'utf-8' }],
  ['node_url_module_serves_the_path_and_host_helpers', {
    path: '/tmp/a b.txt', href: 'file:///tmp/a%20b.txt', ascii: 'xn--bcher-kva.de', unicode: 'bücher.de',
    sameClass: true, port: 8443, search: '?b=1',
  }],
  ['url_search_params_node_semantics', ['null=', 'a=1+2&b=%C3%A9', 'é', 1, 0, 'a=1&a=0&b=2']],
]) {
  test(`session semantics ${name.replaceAll('_', ' ')}`, async () => {
    const [call] = await execute(name);
    assert.deepEqual(success(call), expected);
  });
}

test('session semantics expose native URL components and search parameters', async () => {
  const [call] = await execute('native_url_class_parses_and_exposes_search_params');
  const value = success(call);
  for (const [key, expected] of Object.entries({
    host: 'ex.com:8443', hostname: 'ex.com', port: '8443', proto: 'https:', path: '/a/b', search: '?x=1&y=2',
    hash: '#frag', origin: 'https://ex.com:8443', sp: '2', str: 'https://ex.com:8443/a/b?x=1&y=2#frag',
  })) assert.equal(value[key], expected, key);
});

test('session semantics capture console objects and arrays in Node format', async () => {
  const [call] = await execute('console_uses_node_style_formatter_and_is_captured');
  success(call);
  const messages = call.result.console.map(entry => entry.message);
  assert.equal(messages.length, 2);
  assert.ok(messages[0].startsWith('n = 42 '));
  assert.ok(messages[0].includes('a: 1'));
  assert.ok(messages[1].includes("[ 'x', 'y' ]"));
});

test('session semantics retain printf, Map, Set and RegExp console rendering', async () => {
  const [call] = await execute('console_printf_and_inspect_rendering');
  success(call);
  assert.deepEqual(call.result.console.map(entry => entry.message), [
    'sashoush scored 97% extra', "[ 'x', 1 ]", "Map(1) { 'a' => 1 }", 'Set(2) { 1, 2 }', '/ab+c/gi',
  ]);
});
