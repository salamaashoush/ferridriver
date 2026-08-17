// `testInfo.snapshotPath()` and the matchers resolve through ONE
// template (Playwright's `_resolveSnapshotPaths` / `_applyPathTemplate`,
// worker/testInfo.ts:560-642). The point of the port is that the path a
// matcher writes and the path a spec can ask for are the same string —
// so the two file-backed cases below assert against what actually
// landed on disk, contents included, rather than against a formatting
// rule.
//
// Baselines are NOT committed: a missing one is written by the first
// call, and a following run compares against it. Both orders have to
// pass, which is why nothing here asserts a file is absent.

import { test, describe, expect } from '@ferridriver/test';

describe('testInfo.snapshotPath', () => {
  test('names the exact file toHaveScreenshot writes', async ({ page }, testInfo) => {
    const declared = testInfo.snapshotPath('shot.png', { kind: 'screenshot' });

    await page.setContent('<div style="width:40px;height:40px;background:#123456"></div>');
    await expect(page.locator('div')).toHaveScreenshot('shot.png');

    // The matcher wrote (or matched) a PNG at exactly the path the
    // resolver reported — the agreement this whole phase exists for.
    expect(await fs.exists(declared)).toBe(true);
    const baseline = await fs.readFileBytes(declared);
    expect(baseline.slice(0, 4)).toEqual([0x89, 0x50, 0x4e, 0x47]);
    // And it is THIS element's image, not a leftover: the same capture
    // compared equal, byte for byte.
    const fresh = await page.locator('div').screenshot();
    expect(Array.from(fresh)).toEqual(baseline);
  });

  test('a project qualifies the baseline it owns', async ({}, testInfo) => {
    // Four backend projects run this file; the legacy template's
    // `{-projectName}` is what keeps their baselines apart.
    const declared = testInfo.snapshotPath('shot.png', { kind: 'screenshot' });
    expect(declared).toContain(`shot-${testInfo.project.name}.png`);
  });

  test('puts the baseline in the legacy -snapshots directory beside the spec', async ({}, testInfo) => {
    const declared = testInfo.snapshotPath('a.png', { kind: 'screenshot' });
    expect(declared).toContain('snapshot-path.test.ts-snapshots');
    // Absolute, because a template resolves against the config dir.
    expect(declared.startsWith('/')).toBe(true);
    // Relative to testDir, so the absolute spec path is not glued on.
    expect(declared).not.toContain('/tests/e2e/e2e/');
  });

  test('a kind decides the default extension of an unnamed snapshot', async ({}, testInfo) => {
    // Playwright's anonymous form: the title path plus a running index,
    // with the kind's own extension. An empty name is the same form —
    // upstream tests the name for JS falsiness.
    expect(testInfo.snapshotPath().endsWith('.txt')).toBe(true);
    expect(testInfo.snapshotPath('', { kind: 'screenshot' }).endsWith('.png')).toBe(true);
    expect(testInfo.snapshotPath('', { kind: 'aria' }).endsWith('.aria.yml')).toBe(true);
    // The name comes from the test title, not the file name.
    expect(testInfo.snapshotPath()).toContain('a-kind-decides-the-default-extension');
  });

  test('reading a path never consumes an index', async ({}, testInfo) => {
    // Two reads of the anonymous path agree; a matcher would advance it.
    expect(testInfo.snapshotPath()).toBe(testInfo.snapshotPath());
  });

  test('several segments are joined as a path', async ({}, testInfo) => {
    const nested = testInfo.snapshotPath('nested', 'deep', 'b.png');
    // The project still qualifies the leaf, so the assertion is on the
    // directory part and the extension.
    expect(nested).toContain('nested/deep/b');
    expect(nested.endsWith('.png')).toBe(true);
  });

  test('an unknown kind is refused by name', async ({}, testInfo) => {
    expect(() => testInfo.snapshotPath('x.png', { kind: 'nonsense' as 'snapshot' })).toThrow(
      /unknown kind "nonsense"/,
    );
  });

  test('toMatchSnapshot writes where snapshotPath says', async ({}, testInfo) => {
    const declared = testInfo.snapshotPath('text.txt');
    await expect('hello snapshot').toMatchSnapshot('text.txt');
    expect(await fs.exists(declared)).toBe(true);
    expect((await fs.readFile(declared)).trim()).toBe('hello snapshot');
  });
});
