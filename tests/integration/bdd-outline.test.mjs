import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import cases from './fixtures/outline-cases.mjs';
import { observation, runtimeProbe } from './support.mjs';

const titles = [
  ['Example #1', 'Example #2', 'Example #3'], ['visiting a', 'visiting b'], ['user sashoush is 36'],
  ['hitting a (1)'], ['tagged a'], ['row 1: a'], ['a and <missing>'], ['Check h1', 'Check p'], null, ['Visit the page'],
];
for (const [index, fixture] of cases.entries()) {
  test(`BDD outlines: ${fixture.name.replaceAll('_', ' ')}`, async () => {
    const { results } = await runtimeProbe([{ op: 'expand-feature', source: fixture.source, titleFormat: fixture.titleFormat }]);
    const scenarios = observation(results[0]);
    if (titles[index]) assert.deepEqual(scenarios.map(scenario => scenario.name), titles[index]);
    if (index === 0) assert.deepEqual(scenarios[0].describePath, ['Visit']);
    if (index === 2) assert.deepEqual(scenarios[0].describePath, ['user <name> is <age>']);
    if (index === 7) {
      assert.deepEqual(scenarios[0].describePath, ['Structure', 'Check <thing>']);
      for (const tag of ['@rule', '@feature', '@scenario']) assert.ok(scenarios[0].tags.includes(tag));
      assert.equal(scenarios[0].steps.length, 3);
      assert.deepEqual(scenarios[0].steps.map(step => step.text), ['a page', 'a rule background', 'h1 is visible']);
      assert.equal(scenarios[0].source.ruleName, 'Structure');
    }
    if (index === 8) {
      const source = scenarios[0].source;
      assert.equal(source.featureKeyword, 'Feature');
      assert.equal(source.featureName, 'Sites');
      assert.ok(source.featureDescription.includes("The feature's description."));
      assert.equal(source.featureLine, 2);
      assert.equal(source.scenarioKeyword, 'Scenario Outline');
      assert.ok(source.scenarioDescription.includes("The scenario's description."));
      assert.deepEqual(source.tags.map(tag => tag.name), ['@one', '@two', '@three', '@four']);
      assert.ok(source.tags.every(tag => tag.line > 0));
      assert.equal(source.tags[0].line, source.tags[1].line);
      assert.equal(scenarios[1].source.scenarioLine, source.scenarioLine + 1);
    }
    if (index === 9) {
      assert.deepEqual(scenarios[0].describePath, []);
      assert.equal(scenarios[0].source.scenarioKeyword, 'Scenario');
    }
  });
}
