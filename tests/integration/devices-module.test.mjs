import assert from 'node:assert/strict';
import { test, devices } from '@ferridriver/test';
import { devices as playwrightDevices } from '@playwright/test';
import { devices as driverDevices } from 'ferridriver';

test('device descriptors carry every viewport and browser option needed by a spread', async () => {
  const device = devices['iPhone 15'];
  assert.ok(device);
  assert.ok(device.userAgent.includes('iPhone'));
  assert.deepEqual(device.viewport, { width: 393, height: 659 });
  assert.deepEqual(device.screen, { width: 393, height: 852 });
  assert.equal(device.deviceScaleFactor, 3);
  assert.equal(device.isMobile, true);
  assert.equal(device.hasTouch, true);
  assert.equal(device.defaultBrowserType, 'webkit');
  const spread = { ...device, hasTouch: false };
  assert.equal(spread.userAgent, device.userAgent);
  assert.equal(spread.hasTouch, false);
});

test('all device module specifiers expose the same object including require', async () => {
  assert.equal(devices, playwrightDevices);
  assert.equal(devices, driverDevices);
  assert.equal(devices, require('@playwright/test').devices);
});

test('the entire vendored device registry is exposed without fallback devices', async () => {
  assert.equal(Object.keys(devices).length, 207);
  assert.equal(devices['Nokia 3310'], undefined);
  assert.equal(devices['Desktop Chrome'].defaultBrowserType, 'chromium');
  assert.equal(devices['Desktop Safari'].defaultBrowserType, 'webkit');
  assert.equal(devices['Desktop Firefox'].defaultBrowserType, 'firefox');
});
