import { test, expect } from '@ferridriver/test';

test('text snapshot', async () => {
  await expect('version two').toMatchSnapshot('content');
});
