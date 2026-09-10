import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';

test('headless WebMCP facade uses one target session across commands', async () => {
  const { results } = await runtimeProbe([{
    op: 'browser-engine',
    scripts: [{ source: `
      await page.goto('data:text/html,<title>WebMCP</title>');
      const enabled = await page.webMcp.enable();
      let missingTool = '';
      try { await page.webMcp.invokeTool('missing-tool', { value: 1 }); }
      catch (error) { missingTool = String(error); }
      const disabled = await page.webMcp.disable();
      return {
        methods: ['enable', 'disable', 'invokeTool', 'cancelInvocation']
          .every(name => typeof page.webMcp[name] === 'function'),
        enabled: enabled && typeof enabled === 'object',
        missingTool: missingTool.includes('Tool not found'),
        disabled: disabled && typeof disabled === 'object',
      };
    ` }],
  }]);
  const value = observation(results[0])[0].value;
  assert.deepEqual(value, { methods: true, enabled: true, missingTool: true, disabled: true });
});
