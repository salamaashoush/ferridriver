import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

const feature = `Feature: Checkout
  @smoke
  Scenario: a smoke scenario
    Given something
  @wip @smoke
  Scenario: a smoke scenario that is still in progress
    Given something
  @regression
  Scenario: a regression scenario
    Given something
`;
async function plan(config = {}, files = {}) {
  const { results } = await runtimeProbe([{ op: 'bdd-plan', config: { features: ['*.feature'], ...config } }], {
    'checkout.feature': feature, ...files,
  });
  return results[0];
}

test('BDD planning keeps every scenario without a tag expression', async () => {
  const result = observation(await plan());
  assert.equal(result.totalTests, 3);
  assert.deepEqual(result.projects, {});
});

test('BDD planning selects exactly the scenarios matching a tag expression', async () => {
  assert.deepEqual(observation(await plan({ tags: '@smoke and not @wip' })).names, ['a smoke scenario']);
});

test('an invalid BDD tag expression fails planning', async () => {
  const result = await plan({ tags: '@smoke and' });
  assert.match(result.error, /invalid tag expression/);
});

test('project-specific tags select disjoint corpora from shared features', async () => {
  const result = observation(await plan({ projects: [
    { name: 'smoke', tags: '@smoke and not @wip' }, { name: 'regression', tags: '@regression' },
  ] }));
  assert.deepEqual(result.projects.smoke, ['a smoke scenario']);
  assert.deepEqual(result.projects.regression, ['a regression scenario']);
  assert.ok(result.projects.smoke.every(name => !result.projects.regression.includes(name)));
});

test('projects without BDD overrides reuse the shared plan', async () => {
  const result = observation(await plan({ projects: [{ name: 'chromium' }, { name: 'firefox' }] }));
  assert.equal(result.totalTests, 3);
  assert.deepEqual(result.projects, {});
});

test('a BDD project can select its own feature files', async () => {
  const result = observation(await plan({ projects: [{ name: 'other', features: ['other.feature'] }] }, {
    'other.feature': 'Feature: Other\n\n  Scenario: elsewhere\n    Given something\n',
  }));
  assert.deepEqual(result.projects.other, ['elsewhere']);
});
