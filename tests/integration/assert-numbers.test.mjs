import assert from 'node:assert/strict';
import { isDeepStrictEqual } from 'node:util';
import { test } from '@ferridriver/test';

test('assertions compare numeric values independently of the runtime number representation', () => {
  const parsed = JSON.parse('{"count":1,"values":[1,0]}');
  assert.equal(parsed.count, 1);
  assert.deepEqual(parsed, { count: 1, values: [1, 0] });
  assert.equal(isDeepStrictEqual(parsed, { count: 1, values: [1, 0] }), true);
  assert.equal(NaN, NaN);
  assert.notEqual(0, -0);
  assert.notDeepEqual({ value: 0 }, { value: -0 });
  assert.notEqual(parsed.count, '1');
});
