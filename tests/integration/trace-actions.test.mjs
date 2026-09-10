import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { passed, run, workspace } from './support.mjs';

test('a traced script records public calls once and retains their source locations', async () => {
  const cwd = await workspace({
    'ferridriver.toml': '[mcp.browser]\nheadless = true\nbackend = "cdp-pipe"\n',
    'script.ts': `await context.tracing.start({ snapshots: true });
      await page.setContent('<h1>hello</h1><button id="b">go</button>');
      await page.title();
      await page.content();
      await page.evaluate('() => document.title');
      await page.$('h1');
      await page.locator('#b').click();
      await page.mouse.move(5, 5);
      await page.mouse.click(5, 5);
      await page.keyboard.press('Tab');
      await page.keyboard.type('abc');
      await page.setViewportSize({ width: 900, height: 700 });
      await page.waitForLoadState('load');
      await page.screenshot();
      await context.addCookies([{ name: 'a', value: 'b', url: 'https://example.com' }]);
      await context.cookies();
      await context.setOffline(false);
      await context.route('**/never', route => route.continue());
      await context.unroute('**/never');
      await context.tracing.stop({ path: 'trace.zip' });
      'done';`,
  });
  passed(await run(['run', '--no-inherit', '--instance', 'default', 'script.ts'], { cwd }));
  const result = await run(['trace', 'show', 'trace.zip', '--no-inherit', '--json'], { cwd });
  passed(result);
  const actions = JSON.parse(result.stdout).contexts[0].actions;
  const titles = actions.map(action => action.title);
  for (const title of [
    'page.setContent', 'page.title', 'page.content', 'page.evaluate', 'page.$',
    'locator.click', 'mouse.move', 'mouse.click', 'keyboard.press', 'keyboard.type',
    'page.setViewportSize', 'page.waitForLoadState', 'page.screenshot',
    'browserContext.addCookies', 'browserContext.cookies', 'browserContext.setOffline',
    'browserContext.route', 'browserContext.unroute',
  ]) assert.ok(titles.includes(title), `missing ${title}: ${JSON.stringify(titles)}`);
  for (const title of ['page.setContent', 'page.waitForLoadState']) {
    assert.equal(titles.filter(actual => actual === title).length, 1, `${title} must not include internal calls`);
  }
  const located = actions.filter(action => action.title?.startsWith('page.') && action.stack?.[0]?.file?.endsWith('script.ts'));
  assert.ok(located.length >= 5, JSON.stringify(actions));
});
