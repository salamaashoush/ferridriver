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

// "should grant geolocation permission when origin is listed" and
// "should not grant other permissions when origin is not listed".
test('a grant for one origin leaves the others alone', async ({ page, context }) => {
  await page.goto('http://localhost:47831/fx/landed');
  const before = await geolocation(page);
  await context.grantPermissions(['geolocation'], { origin: 'http://127.0.0.1:47831/any/path' });
  expect(await geolocation(page)).toBe(before);
  await page.goto('http://127.0.0.1:47831/fx/landed');
  expect(await geolocation(page)).toBe('granted');
  await context.clearPermissions();
});

// "should accumulate when adding": a second grant adds to the first.
test('grants add up', async ({ page, context }) => {
  await page.goto('/fx/landed');
  await context.grantPermissions(['geolocation']);
  await context.grantPermissions(['notifications']);
  expect(await geolocation(page)).toBe('granted');
  const notifications = await page.evaluate(() =>
    navigator.permissions.query({ name: 'notifications' }).then(status => status.state));
  expect(notifications).toBe('granted');
  await context.clearPermissions();
});

// "should fail when bad permission is given".
test('an unknown permission is refused', async ({ page, context, browserName }) => {
  await page.goto('/fx/landed');
  const error = await context.grantPermissions(['foo'], { origin: page.url() }).then(() => null, (e: Error) => e);
  expect(error!.message).toContain(browserName === 'firefox' ? 'invalid argument' : 'Unknown permission: foo');
});
