import assert from 'node:assert/strict';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { workspace } from './support.mjs';
import { McpClient, payload } from './mcp-client.mjs';

test('MCP BDD captures passing and failing scenarios and sees the script session page', async () => {
  const client = await McpClient.launch();
  try {
    const good = payload(await client.call('run_bdd', { gherkin: `Feature: Smoke
  Scenario: data URL renders
    Given I navigate to "data:text/html,<h1>Hello BDD</h1>"
    Then "h1" should contain text "Hello BDD"
` }));
    assert.equal(good.status, 'passed');
    assert.equal(good.passed, 1);
    assert.equal(good.failed, 0);
    const bad = payload(await client.call('run_bdd', { gherkin: `Feature: Smoke
  Scenario: wrong text
    Given I navigate to "data:text/html,<h1>Hello</h1>"
    Then "h1" should contain text "Goodbye"
` }));
    assert.equal(bad.status, 'failed');
    assert.equal(bad.failed, 1);
    await client.script("await page.goto('data:text/html,<h1>SharedSession</h1>');");
    const shared = payload(await client.call('run_bdd', { gherkin: `Feature: Shared session
  Scenario: BDD sees the run_script page
    Then "h1" should contain text "SharedSession"
` }));
    assert.equal(shared.status, 'passed');
    assert.equal(shared.passed, 1);
  } finally { await client.close(); }
});

test('MCP BDD loads custom step files and retains their module state across runs', async () => {
  const root = await workspace({
    'steps.js': `Given('I am on a temp blank page', async world => { await world.page.goto('about:blank'); });`,
    'custom.feature': `Feature: MCP JS steps
  Scenario: custom js step
    Given I am on a temp blank page
    Then the URL should contain "about:blank"
`,
    'counter.js': `let counter = 0;
When('I increment the counter', async () => { counter += 1; });
Then('the counter should be {string}', async (_world, n) => {
  if (String(counter) !== n) throw new Error('counter ' + counter + ' != ' + n);
});`,
  });
  const client = await McpClient.launch();
  try {
    const custom = payload(await client.call('run_bdd', {
      features: [join(root, 'custom.feature')], steps: [join(root, 'steps.js')],
    }));
    assert.equal(custom.status, 'passed');
    assert.equal(custom.passed, 1);
    assert.equal(custom.failed, 0);
    for (const count of [1, 2]) {
      const result = payload(await client.call('run_bdd', {
        steps: [join(root, 'counter.js')], gherkin: `Feature: Reuse
  Scenario: count
    When I increment the counter
    Then the counter should be "${count}"
`,
      }));
      assert.equal(result.status, 'passed', JSON.stringify(result));
    }
  } finally { await client.close(); }
});

test('MCP BDD caches reordered step sets and rebuilds when steps or world parameters change', async () => {
  const root = await workspace({
    'increment.js': `globalThis.counter = 0;
When('I increment the counter', async () => { globalThis.counter += 1; });`,
    'assert.js': `Then('the counter should be {int}', async (_world, count) => {
  if (globalThis.counter !== count) throw new Error('counter ' + globalThis.counter + ' != ' + count);
});
Then('the world value should be {int}', async function (_world, value) {
  if (this.parameters.value !== value) throw new Error('world value ' + JSON.stringify(this.parameters) + ' != ' + value);
  if (this.parameters.fraction !== 0.25 || this.parameters.large !== 1e30) throw new Error('numeric parameters changed');
});`,
    'different.js': `globalThis.counter = 0;
When('I increment the counter', async () => { globalThis.counter += 10; });`,
  });
  const client = await McpClient.launch();
  try {
    for (const [steps, value, count] of [
      [['increment.js', 'assert.js'], 1, 1],
      [['assert.js', 'increment.js'], 1, 2],
      [['assert.js', 'increment.js'], 2, 1],
      [['different.js', 'assert.js'], 2, 10],
    ]) {
      const result = payload(await client.call('run_bdd', {
        steps: steps.map(step => join(root, step)), world_parameters: { value, fraction: 0.25, large: 1e30 },
        gherkin: `Feature: Cached engine
  Scenario: counters and parameters
    When I increment the counter
    Then the counter should be ${count}
    And the world value should be ${value}
`,
      }));
      assert.equal(result.status, 'passed', JSON.stringify(result));
    }
  } finally { await client.close(); }
});
