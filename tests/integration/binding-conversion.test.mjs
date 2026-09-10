import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

const array = (...a) => ({ id: 1, a });
const object = entries => ({ id: 1, o: Object.entries(entries).map(([k, v]) => ({ k, v })) });

function normalize(value) {
  if (value && typeof value === 'object') {
    if (value.a) return { id: 0, a: value.a.map(normalize) };
    if (value.o) return { id: 0, o: value.o.map(({ k, v }) => ({ k, v: normalize(v) })) };
  }
  return value;
}

test('binding conversion round trips nested values without creating remote handles', async () => {
  const cases = [true, false, 42, -1.5, 'hello', { v: 'null' }, array(1, 'two', false),
    object({ a: 1, b: 'x', c: array(true, { v: 'null' }) }), array(array(array(object({ deep: 7 }))))];
  const { results } = await runtimeProbe(cases.map(value => ({ op: 'convert-value', value })));
  for (let index = 0; index < cases.length; index++) {
    const result = observation(results[index]);
    assert.deepEqual(normalize(result.serialized), normalize(cases[index]));
    assert.deepEqual(result.handles, []);
  }
});

test('binding conversion preserves identity when two array slots reference the same object', async () => {
  const { results } = await runtimeProbe([{
    op: 'convert-value', value: { id: 100, a: [object({ k: 9 }), { ref: 1 }] },
    expression: 'Array.isArray(__v) && __v.length === 2 && __v[0] === __v[1] && __v[0].k === 9',
  }]);
  assert.equal(observation(results[0]).probe, 'true');
});

test('binding conversion restores native dates, regular expressions, bigints, URLs and typed arrays', async () => {
  const cases = [
    [{ d: '2020-01-02T03:04:05.000Z' }, '__v instanceof Date && __v.toISOString()', '2020-01-02T03:04:05.000Z'],
    [{ r: { p: 'ab+c', f: 'i' } }, "(__v instanceof RegExp) + '|' + __v.source + '|' + __v.flags", 'true|ab+c|i'],
    [{ bi: '123456789012345' }, "typeof __v === 'bigint' && (__v === 123456789012345n)", 'true'],
    [{ u: 'https://example.com/p?q=1' }, '__v instanceof URL && __v.href', 'https://example.com/p?q=1'],
    [{ ta: { k: 'ui8', b: 'AQID' } }, "(__v instanceof Uint8Array) + '|' + __v.length + '|' + __v[2]", 'true|3|3'],
  ];
  const { results } = await runtimeProbe(cases.map(([value, expression]) => ({ op: 'convert-value', value, expression })));
  for (let index = 0; index < cases.length; index++) assert.equal(observation(results[index]).probe, cases[index][2]);
});

test('binding conversion preserves special numbers, including the sign of zero', async () => {
  const cases = [
    ['NaN', 'Number.isNaN(__v)'], ['Infinity', '__v === Infinity'], ['-Infinity', '__v === -Infinity'],
    ['-0', '__v === 0 && Object.is(__v, -0)'], ['undefined', '__v === undefined'],
  ];
  const { results } = await runtimeProbe(cases.map(([v, expression]) => ({ op: 'convert-value', value: { v }, expression })));
  for (let index = 0; index < cases.length; index++) {
    const result = observation(results[index]);
    assert.equal(result.probe, 'true', cases[index][0]);
    assert.deepEqual(result.serialized, { v: cases[index][0] });
  }
});
