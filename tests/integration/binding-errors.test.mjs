import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { script } from './support.mjs';

test('a binding rejects a bad argument with a whole TypeError', async () => {
  const behavior = 'sashoush'.repeat(40);
  const { value } = await script(`
    const browser = await chromium().launch({ headless: true });
    try {
      const page = await browser.newPage();
      await page.unrouteAll({ behavior: ${JSON.stringify(behavior)} });
      return null;
    } catch (error) {
      return { typeError: error instanceof TypeError, name: error.name, message: error.message };
    } finally {
      await browser.close();
    }`);
  assert.equal(value.typeError, true, JSON.stringify(value));
  assert.equal(value.name, 'TypeError');
  assert.equal(value.message,
    `unrouteAll options: invalid behavior "${behavior}" (expected 'wait', 'ignoreErrors', or 'default')`);
});
