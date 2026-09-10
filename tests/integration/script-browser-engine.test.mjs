import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { observation, runtimeProbe } from './support.mjs';
import { dataUrl } from './mcp-client.mjs';

async function execute(scripts) {
  const { results } = await runtimeProbe([{ op: 'browser-engine', scripts }]);
  return observation(results[0]).map(result => {
    assert.equal(result.status, 'ok', JSON.stringify(result));
    return result.value;
  });
}

const navigate = html => `await page.goto(${JSON.stringify(dataUrl(html))});`;

test('script browser engine preserves page bindings and host variables across executions', async () => {
  const argument = 'prompt-injection"; drop table; --';
  const values = await execute([
    { source: `${navigate('<title>Hello</title><body>World</body>')} return { title: await page.title(), url: await page.url() };` },
    { source: "return await page.evaluate('1 + 2');" },
    { source: `${navigate('<button id="b" onclick="this.textContent=\'clicked\'">Go</button>')}
      await page.locator('#b').click(); return await page.evaluate("document.getElementById('b').textContent");` },
    { source: `${navigate('<input id="i" type="text">')} const loc = page.locator('#i'); await loc.fill('hi there'); return await loc.inputValue();` },
    { source: `${navigate('<div id="shown">x</div><div id="hidden" style="display:none">y</div>')}
      return { v: await page.isVisible('#shown'), h: await page.isHidden('#hidden') };` },
    { source: `${navigate('<button>a</button><button>b</button><button>c</button>')} return await page.getByRole('button').count();` },
    { source: `${navigate('<button>alpha</button><button>beta</button><button>gamma</button>')} return await page.getByRole('button').nth(1).textContent();` },
    { source: "vars.set('checkpoint', 'first'); return null;" },
    { source: "return vars.get('checkpoint');" },
    { source: `${navigate('<input id="i" type="text">')} await page.fill('#i', args[0]); return await page.inputValue('#i');`, args: [argument] },
  ]);
  assert.ok(values[0].title.includes('Hello'));
  assert.ok(values[0].url.startsWith('data:'));
  assert.equal(values[1], 3);
  assert.ok(values[2].includes('clicked'));
  assert.equal(values[3], 'hi there');
  assert.deepEqual(values[4], { v: true, h: true });
  assert.equal(values[5], 3);
  assert.equal(values[6], 'beta');
  assert.equal(values[7], null);
  assert.equal(values[8], 'first');
  assert.equal(values[9], argument);
});

test('script browser engine accepts navigation options and closes the actual page', async () => {
  const values = await execute([
    { source: `await page.goto(${JSON.stringify(dataUrl('<title>opts</title><body>ready</body>'))},
      { waitUntil: 'domcontentloaded', referer: 'https://ref.example.com/', timeout: 10000 }); return await page.title();` },
    { source: "page.setDefaultTimeout(5000); page.setDefaultNavigationTimeout(10000); return 'ok';" },
    { source: "await page.goto('data:text/html,about'); return await page.isClosed();" },
    { source: "await page.close({ reason: 'script finished' }); return await page.isClosed();" },
  ]);
  assert.deepEqual(values, ['opts', 'ok', false, true]);
});
