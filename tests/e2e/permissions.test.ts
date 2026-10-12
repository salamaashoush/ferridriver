// Playwright tests/library/permissions.spec.ts "should reset permissions":
// a grant shows in the Permissions API, and clearPermissions takes the
// page back to what it reported before the grant.
import { test, expect } from '@ferridriver/test';
import type { Page } from '@ferridriver/test';

const geolocation = (page: Page) =>
  page.evaluate(() => navigator.permissions.query({ name: 'geolocation' }).then(status => status.state));

test('clearPermissions takes back what grantPermissions gave', async ({ page, context }) => {
  await page.goto('/fx/landed');
  const before = await geolocation(page);
  expect(before).not.toBe('granted');

  await context.grantPermissions(['geolocation']);
  expect(await geolocation(page)).toBe('granted');

  await context.clearPermissions();
  expect(await geolocation(page)).toBe(before);
});
