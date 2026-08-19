// The fixture tuple's `{ timeout, title, box }`.
//
// `box` here is NOT `test.step`'s: a step's `box` re-attributes an error
// to the step's call site, while a fixture's decides whether the fixture
// appears as a step at all and under what grouping.

import { test as base, describe, expect } from '@ferridriver/test';

const test = base.extend<{
  named: string;
  hidden: string;
  grouped: string;
}>({
  named: [
    async ({}, use) => {
      await use('named');
    },
    { title: 'sign in' },
  ],
  hidden: [
    async ({}, use) => {
      await use('hidden');
    },
    { box: 'self' },
  ],
  grouped: [
    async ({}, use) => {
      await use('grouped');
    },
    { box: true },
  ],
});

// A fixture whose setup outlives its own timeout: the fixture server
// holds this route open far past the 250ms budget.
const slow = base.extend<{ slowOne: string }>({
  slowOne: [
    async ({ page, baseURL }, use) => {
      await page.goto(`${baseURL}/fx/slow?ms=8000`);
      await use('never');
    },
    { timeout: 250 },
  ],
});

describe('fixture options', () => {
  // The step a titled fixture opens is `Fixture "sign in"`, but a
  // fixture step is not visible from a spec: the JSON reporter shows
  // only `test.step` categories, as Playwright's does
  // (`reporters/json.ts:200`). The title rule is pinned by
  // `fixture_step_title`'s unit test; what a spec can see is that the
  // fixture resolved under it.
  test('a titled fixture still resolves', async ({ named }) => {
    expect(named).toBe('named');
  });

  test('box: self resolves the fixture without opening a step', async ({ hidden }) => {
    expect(hidden).toBe('hidden');
  });

  test('box: true still resolves the fixture', async ({ grouped }) => {
    expect(grouped).toBe('grouped');
  });

});

describe('a fixture timeout', () => {
  // The failure happens in SETUP, before any body runs, so the expected
  // outcome has to be declared at describe scope — a `fail()` inside the
  // body would never be reached.
  slow.fail();

  slow('fails the setup, naming the fixture', async ({ slowOne }) => {
    expect(slowOne).toBe('never');
  });
});
