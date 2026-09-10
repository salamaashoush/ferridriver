import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { McpClient, dataUrl, ok } from './mcp-client.mjs';

for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: screenshot artifacts resolve to PNG resources and are listed`, async () => {
    const client = await McpClient.launch(backend);
    try {
      ok(await client.call('navigate', { url: dataUrl('<h1>Shot</h1>') }));
      await client.script("await page.waitForSelector('h1'); return true;");
      const response = ok(await client.call('screenshot'));
      const link = response.result.content.find(block => block.type === 'resource_link');
      assert.ok(link, JSON.stringify(response));
      const image = response.result.content.find(block => block.type === 'image');
      assert.ok(image);
      assert.match(link.uri, /^artifact:\/\/screenshots\//);
      assert.equal(link.mimeType, 'image/png');
      const read = ok(await client.request('resources/read', { uri: link.uri }));
      assert.match(read.result.contents[0].blob, /^iVBOR/);
      assert.equal(read.result.contents[0].blob, image.data);
      assert.equal(link.size, atob(image.data).length);
      const listed = ok(await client.request('resources/list', {}));
      assert.ok(listed.result.resources.some(resource => resource.uri === link.uri));
    } finally { await client.close(); }
  });

  test(`${backend}: navigation progress is correlated and finishes at its total`, async () => {
    const client = await McpClient.launch(backend);
    try {
      const progress = [];
      const token = `navigation-${backend}`;
      ok(await client.request('tools/call', {
        name: 'navigate', arguments: { url: dataUrl('<h1>P</h1>') },
        _meta: { progressToken: token },
      }, progress));
      assert.ok(progress.length >= 2, JSON.stringify(progress));
      for (const beat of progress) assert.equal(beat.progressToken, token);
      const last = progress.at(-1);
      assert.equal(typeof last.progress, 'number');
      assert.equal(last.progress, last.total);
    } finally { await client.close(); }
  });

  test(`${backend}: BDD progress counts both completed scenarios`, async () => {
    const client = await McpClient.launch(backend);
    try {
      const progress = [];
      const token = `bdd-${backend}`;
      const gherkin = `Feature: Progress
  Scenario: one
    Given I navigate to "data:text/html,<h1>One</h1>"
    Then "h1" should contain text "One"
  Scenario: two
    Given I navigate to "data:text/html,<h1>Two</h1>"
    Then "h1" should contain text "Two"
`;
      ok(await client.request('tools/call', {
        name: 'run_bdd', arguments: { gherkin }, _meta: { progressToken: token },
      }, progress));
      assert.ok(progress.length >= 2, JSON.stringify(progress));
      for (const beat of progress) assert.equal(beat.progressToken, token);
      assert.equal(progress.at(-1).total, 2);
      assert.equal(progress.at(-1).progress, 2);
    } finally { await client.close(); }
  });
}
