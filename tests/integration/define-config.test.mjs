import assert from 'node:assert/strict';
import { test, defineConfig } from '@ferridriver/test';
import { defineConfig as viaPlaywright } from '@playwright/test';
import { defineConfig as viaBare } from 'playwright/test';

const eq = (actual, expected, message) => assert.equal(JSON.stringify(actual), JSON.stringify(expected), message);
const ok = (condition, message) => assert.ok(condition, message);

test('a later config overrides an earlier one key by key', async () => {
  const merged = defineConfig(
        { timeout: 1000, retries: 1, workers: 4 },
        { timeout: 2000, retries: 3 },
        { retries: 9 },
      );
      eq(merged.timeout, 2000, 'the middle config wins over the first');
      eq(merged.retries, 9, 'the last config wins over both');
      eq(merged.workers, 4, 'a key nobody overrode survives');
});

test('use expect and build merge one level deep', async () => {
  const merged = defineConfig(
        { use: { locale: 'de-DE', headless: true }, expect: { timeout: 5000 }, build: { external: ['a'] } },
        { use: { locale: 'fr-FR' }, expect: { toHaveScreenshot: { maxDiffPixels: 3 } } },
      );
      eq(merged.use.locale, 'fr-FR', 'the incoming use key wins');
      eq(merged.use.headless, true, 'a use key only the first config set survives');
      eq(merged.expect.timeout, 5000, 'expect merges rather than being replaced');
      eq(merged.expect.toHaveScreenshot.maxDiffPixels, 3, 'the incoming expect key lands');
      eq(merged.build.external, ['a'], 'build merges the same way');
});

test('the merged blocks exist even when neither side had them', async () => {
  const merged = defineConfig({ timeout: 1 }, { retries: 2 });
      eq(merged.use, {}, 'use is created empty');
      eq(merged.expect, {}, 'expect is created empty');
      eq(merged.webServer, [], 'webServer is created empty');
      ok(merged.build === undefined, 'no build block is invented');
});

test('an explicit undefined erases the key beneath it', async () => {
  const merged = defineConfig(
        { use: { storageState: 'state.json' }, globalSetup: './setup.ts' },
        { use: { storageState: undefined }, globalSetup: undefined },
      );
      ok('storageState' in merged.use, 'the key is present');
      ok(merged.use.storageState === undefined, 'and its value is the override');
      ok(merged.globalSetup === undefined, 'a top-level key clears the same way');
});

test('web server normalizes each side then concatenates', async () => {
  const one = defineConfig(
        { webServer: { command: 'a' } },
        { webServer: [{ command: 'b' }, { command: 'c' }] },
      );
      eq(one.webServer.map(w => w.command), ['a', 'b', 'c'], 'a lone object is normalized, then concatenated');

      const two = defineConfig({ webServer: { command: 'a' } }, { timeout: 1 });
      eq(two.webServer.map(w => w.command), ['a'], 'an absent incoming side contributes nothing');

      const three = defineConfig({ timeout: 1 }, { webServer: { command: 'b' } });
      eq(three.webServer.map(w => w.command), ['b'], 'an absent outgoing side contributes nothing');
});

test('projects merge by name and new names are appended', async () => {
  const merged = defineConfig(
        { projects: [
          { name: 'chromium', retries: 1, use: { locale: 'de-DE', headless: true } },
          { name: 'firefox', retries: 2 },
        ] },
        { projects: [
          { name: 'firefox', retries: 5, use: { locale: 'fr-FR' } },
          { name: 'webkit', retries: 7 },
        ] },
      );
      eq(merged.projects.map(p => p.name), ['chromium', 'firefox', 'webkit'],
        'matched names keep their position and a new name is appended');
      eq(merged.projects[1].retries, 5, 'the override wins for a matched project');
      eq(merged.projects[1].use.locale, 'fr-FR', 'its use block merges rather than replacing');
      eq(merged.projects[0].use.locale, 'de-DE', 'an unmatched project is untouched');
      eq(merged.projects[2].retries, 7, 'the appended project keeps its own values');
});

test('a project nothing overrode keeps its identity', async () => {
  const chromium = { name: 'chromium', retries: 1 };
      const merged = defineConfig({ projects: [chromium] }, { projects: [{ name: 'webkit' }] });
      ok(merged.projects[0] === chromium, 'the same object comes out');
      ok(!('use' in merged.projects[0]), 'an absent use block is not created');
      ok('use' in defineConfig({ projects: [{ name: 'a' }] }, { projects: [{ name: 'a' }] }).projects[0],
        'a project that WAS overridden always has one');
});

test('neither side declaring projects leaves the key alone', async () => {
  const merged = defineConfig({ timeout: 1 }, { retries: 2 });
      ok(!('projects' in merged), 'no projects key is invented');

      const kept = defineConfig({ projects: [{ name: 'a' }] }, { retries: 2 });
      eq(kept.projects.map(p => p.name), ['a'], 'an outgoing list survives an incoming config that has none');
});

test('one argument is returned as it was passed', async () => {
  const config = { timeout: 1 };
      ok(defineConfig(config) === config, 'a single config is not copied');
      ok(!('use' in config), 'and nothing is added to it');
});

test('calling it with nothing is a type error', async () => {
  let threw = null;
      try { defineConfig(); } catch (e) { threw = e; }
      ok(threw !== null, 'defineConfig() with no arguments throws');
      eq(threw.name, 'TypeError', 'and it is a TypeError');
});

test('the playwright specifier serves the same function', async () => {
  if (viaPlaywright !== defineConfig || viaBare !== defineConfig)
    throw new Error('every specifier must answer with the same defineConfig');
});
