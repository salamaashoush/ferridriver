import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';

test('exception matchers accept predicates and preserve constructor checks', async () => {
  const error = new TypeError('sashoush');
  const fail = () => { throw error; };
  assert.throws(fail, received => received === error);
  assert.throws(fail, function (received) { return received === error; });
  assert.throws(fail, TypeError);
  const mismatch = { code: 'ERR_ASSERTION' };
  assert.throws(() => assert.throws(fail, RangeError), mismatch);
  assert.throws(() => assert.throws(fail, () => false), mismatch);
  assert.throws(() => assert.throws(fail, () => 'truthy'), mismatch);
  await assert.rejects(Promise.reject(error), received => received === error);
  await assert.rejects(assert.rejects(Promise.reject(error), () => false), mismatch);
});
