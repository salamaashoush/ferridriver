import assert from 'node:assert/strict';
import { test, totp } from '@ferridriver/test';
import { totp as frameworkTotp } from 'ferridriver';

test('totp is the same function from the test module and the framework module', async () => {
  assert.equal(totp, frameworkTotp);
  assert.equal(frameworkTotp('GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ', { timestamp: 59, digits: 8 }), '94287082');
});
