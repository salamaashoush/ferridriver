import {mkdir, readdir, readFile, writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {test, expect} from '@ferridriver/test';
import { runOnceAcrossProjects } from './helpers/projects';

const preferences = {
  'general.useragent.override': 'sashoush-pref-probe',
  'dom.maxHardwareConcurrency': 1,
  'dom.webnotifications.enabled': false,
};

async function observe(page: any) {
  await page.route('http://127.0.0.1:47839/**', async (route: any) => {
    await route.fulfill({contentType: 'text/html', body: '<!doctype html><title>Firefox preferences</title>'});
  });
  await page.goto('http://127.0.0.1:47839/');
  return page.evaluate(() => ({agent: navigator.userAgent, cores: navigator.hardwareConcurrency, notifications: typeof Notification}));
}

test('Firefox launch applies string, number and boolean preferences without leaking to a fresh profile', async () => {
  runOnceAcrossProjects();
  // Each launches Firefox two or three times, about 4s apiece on 8 cores.
  test.slow();
  const browser = await firefox().launch({headless: true, firefoxUserPrefs: preferences});
  try {
    expect(await observe(await browser.newPage())).toEqual({agent: 'sashoush-pref-probe', cores: 1, notifications: 'undefined'});
  } finally {
    await browser.close();
  }
  const fresh = await firefox().launch({headless: true});
  try {
    const normal = await observe(await fresh.newPage());
    expect(normal.agent).not.toBe('sashoush-pref-probe');
    expect(normal.notifications).toBe('function');
  } finally {
    await fresh.close();
  }
});

test('a Firefox launch keeps its per-user data out of the home directory', async () => {
  runOnceAcrossProjects();
  test.slow();
  // Firefox writes profiles.ini, crash reports and pending pings under its
  // per-user data root (~/Library/Application Support/Firefox, ~/.mozilla)
  // before it reads --profile. A launch gets its own root, so the user's
  // Firefox data is untouched and a protected one does not stop startup.
  const home = test.info().outputPath('home');
  await mkdir(home, {recursive: true});
  const browser = await firefox().launch({headless: true, env: {HOME: home}});
  try {
    const page = await browser.newPage();
    await page.setContent('<h1>launched</h1>');
    expect(await page.locator('h1').textContent()).toBe('launched');
  } finally {
    await browser.close();
  }
  expect(await readdir(home, {recursive: true})).toEqual([]);
});

test('Firefox persistent preferences preserve caller content and reject a second live profile owner', async () => {
  runOnceAcrossProjects();
  // Each launches Firefox two or three times, about 4s apiece on 8 cores.
  test.slow();
  const profile = test.info().outputPath('firefox-profile');
  await mkdir(profile, {recursive: true});
  const prefsFile = join(profile, 'user.js');
  const caller = '// sashoush profile\nuser_pref("general.useragent.override", "sashoush-saved-profile");\n';
  await writeFile(prefsFile, caller);
  const context = await firefox().launchPersistentContext(profile, {headless: true, firefoxUserPrefs: preferences});
  try {
    expect(await observe(await context.newPage())).toEqual({agent: 'sashoush-pref-probe', cores: 1, notifications: 'undefined'});
    const contents = await readFile(prefsFile, 'utf8');
    expect(contents).toContain(caller);
    await expect(firefox().launchPersistentContext(profile, {headless: true, firefoxUserPrefs: {'general.useragent.override': 'wrong-owner'}})).rejects.toThrow(/already in use|legacy.*lock/);
    expect(await readFile(prefsFile, 'utf8')).toBe(contents);
    expect((await observe(await context.newPage())).agent).toBe('sashoush-pref-probe');
  } finally {
    await context.close();
  }
  const reopened = await firefox().launchPersistentContext(profile, {headless: true});
  try {
    expect((await observe(await reopened.newPage())).agent).toBe('sashoush-saved-profile');
    expect(await readFile(prefsFile, 'utf8')).toContain(caller);
  } finally {
    await reopened.close();
  }
});

test('Firefox launch rejects non-scalar preferences through native scripting', async () => {
  runOnceAcrossProjects();
  for (const invalid of [null, [], {}, 1.5, 2147483648, -2147483649, 'sashoush\0pref']) {
    await expect(firefox().launch({headless: true, firefoxUserPrefs: {invalid: invalid as any}})).rejects.toThrow(/firefoxUserPrefs/);
  }
});
