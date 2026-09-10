import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { dataUrl } from './mcp-client.mjs';
import { observation, runtimeProbe } from './support.mjs';

const fixture = dataUrl(`<!doctype html><html><head><title>Surface</title></head><body>
<h1 id="hdr">Surface Fixture</h1>
<button id="btn" data-testid="go" aria-label="Go Button">Click Me</button>
<input id="txt" type="text" placeholder="type here" value="seed">
<input id="ro" type="text" value="locked" readonly><input id="chk" type="checkbox">
<select id="sel"><option value="a">A</option><option value="b">B</option></select>
<a id="lnk" href="#frag" title="Home Link">home</a><img id="pic" alt="A Picture" src="data:image/gif;base64,R0lGODlhAQABAAAAACw=">
<label for="lblin">My Label</label><input id="lblin"><ul id="list"><li>one</li><li>two</li><li>three</li></ul>
<div id="deep" style="height:3000px">tail</div><iframe id="ifr" srcdoc="<button id='ib'>InnerBtn</button><p>frame text</p>"></iframe>
<script>document.getElementById('btn').onclick=()=>document.getElementById('btn').textContent='Clicked';
document.getElementById('btn').addEventListener('custom-evt',()=>document.getElementById('hdr').textContent='Dispatched');</script>
</body></html>`);

async function execute(sources) {
  const { results } = await runtimeProbe([{ op: 'browser-engine', scripts: sources }]);
  return observation(results[0]).map(result => {
    assert.equal(result.status, 'ok', JSON.stringify(result));
    return result.value;
  });
}

function everyTrue(label, value) {
  assert.equal(typeof value, 'object', `${label} must return an object`);
  for (const [key, result] of Object.entries(value)) assert.equal(result, true, `${label}.${key}`);
}

test('native JS runner covers the script binding surface with real assertions', async () => {
  const values = await execute([
    { source: `await page.addInitScript(() => { window.__init = 7; }); const response = await page.goto(${JSON.stringify(fixture)}); vars.set('runs', '1'); return { title: await page.title() === 'Surface', url: (await page.url()).startsWith('data:'), init: await page.evaluate(() => window.__init) === 7, status: !response || response.status() === 200 || response.status() === 0 };` },
    { source: `return { role: await page.getByRole('button', { name: 'Go Button' }).count() === 1, text: await page.getByText('Click Me').count() >= 1, label: await page.getByLabel('My Label').count() === 1, placeholder: await page.getByPlaceholder('type here').count() === 1, alt: await page.getByAltText('A Picture').count() === 1, title: await page.getByTitle('Home Link').count() === 1, testId: await page.getByTestId('go').count() === 1, innerText: (await page.locator('#hdr').innerText()).includes('Surface'), html: (await page.locator('#list').innerHTML()).includes('<li>'), attribute: await page.locator('#lnk').getAttribute('title') === 'Home Link' };` },
    { source: `const li = page.locator('#list li'); return { count: await li.count() === 3, first: await li.first().textContent() === 'one', last: await li.last().textContent() === 'three', nth: await li.nth(1).textContent() === 'two', allText: (await li.allTextContents()).length === 3, filter: await li.filter({ hasText: 'two' }).count() === 1, attached: await page.locator('#hdr').isAttached(), absent: !(await page.locator('#nope').isAttached()), editable: await page.locator('#txt').isEditable(), readonly: !(await page.locator('#ro').isEditable()), visible: await page.locator('#hdr').isVisible() };` },
    { source: `const text = page.locator('#txt'); await text.fill('hello world'); const filled = await text.inputValue(); await text.clear(); const cleared = await text.inputValue(); const checkbox = page.locator('#chk'); await checkbox.check(); const checked = await checkbox.isChecked(); await checkbox.uncheck(); const unchecked = !(await checkbox.isChecked()); await page.locator('#sel').selectOption('b'); return { filled: filled === 'hello world', cleared: cleared === '', checked, unchecked, selected: await page.locator('#sel').inputValue() === 'b' };` },
    { source: `await page.locator('#txt').focus(); await page.keyboard.type('ab'); await page.keyboard.press('Backspace'); await page.keyboard.insertText('Z'); await page.keyboard.down('Shift'); await page.keyboard.up('Shift'); await page.locator('#btn').click(); await page.mouse.move(5, 5); await page.mouse.move(20, 20); await page.mouse.wheel(0, 200); return { typed: await page.locator('#txt').inputValue() === 'aZ', clicked: await page.locator('#btn').textContent() === 'Clicked', scrolled: (await page.evaluate(() => window.scrollY)) >= 0 };` },
    { source: `const heading = await page.locator('#hdr').elementHandle(); const box = await heading.boundingBox(); const list = await page.locator('#list').elementHandle(); const one = await list.$eval('li', el => el.textContent); const three = await list.$$eval('li', els => els.length); const handle = await page.evaluateHandle(() => ({ a: 1, b: [2, 3] })); const prop = await (await handle.getProperty('a')).jsonValue(); const props = await handle.getProperties(); const json = await handle.jsonValue(); return { tag: await heading.evaluate(el => el.tagName) === 'H1', box: box.width > 0 && box.height > 0, eval: one === 'one', subEval: three === 3, property: prop === 1, properties: Object.hasOwn(props, 'a'), json: json.b[1] === 3 };` },
    { source: `await page.locator('#btn').dispatchEvent('custom-evt'); const frames = await page.frames(); const frameLocator = page.frameLocator('#ifr'); return { dispatched: await page.locator('#hdr').textContent() === 'Dispatched', frameCount: frames.length >= 2, innerButton: await frameLocator.locator('#ib').textContent() === 'InnerBtn', frameText: await frameLocator.getByText('frame text').count() === 1, and: await page.locator('#list li').and(page.getByText('two')).count() === 1, or: await page.locator('#hdr').or(page.locator('#btn')).count() === 2 };` },
    { source: `await page.exposeFunction('addOne', n => n + 1); await page.exposeFunction('sum', (a, b) => a + b); return { exposed: await page.evaluate(async () => window.addOne(41)) === 42, spread: await page.evaluate(async () => window.sum(3, 4)) === 7 };` },
    { source: `await page.setViewportSize({ width: 800, height: 600 }); const dimensions = await page.evaluate(() => [innerWidth, innerHeight]); await page.emulateMedia({ colorScheme: 'dark' }); const screenshot = await page.screenshot(); return { viewport: dimensions[0] === 800 && dimensions[1] === 600, screenshot: (screenshot?.length || screenshot?.byteLength || 0) > 0 };` },
    { source: `const url = new URL('https://a.test:8443/p?x=1#h'); const encoded = new TextEncoder().encode('hi'); const decoded = new TextDecoder().decode(encoded); vars.set('checkpoint', 'v1'); return { url: url.port === '8443' && url.hash === '#h', encoding: encoded.length === 2 && decoded === 'hi', base64: atob(btoa('xy')) === 'xy', process: typeof process.platform === 'string' && typeof process.versions.quickjs === 'string', vars: vars.get('checkpoint') === 'v1', repl: vars.get('runs') === '1' };` },
    { source: `vars.set('runs', '2'); await page.goto(${JSON.stringify(fixture)}); return { runs: vars.get('runs') === '2', vars: vars.get('checkpoint') === 'v1', init: await page.evaluate(() => window.__init) === 7, locatorEvaluate: await page.locator('#hdr').evaluate((el, suffix) => el.tagName + suffix, '-X') === 'H1-X', evaluateAll: await page.locator('#list li').evaluateAll((els, multiplier) => els.length * multiplier, 10) === 30 };` },
    { source: `await page.setContent('<input id="i"><div id="h" style="display:none">x</div>'); await page.locator('#i').pressSequentially('hello', { delay: 0 }); await page.locator('#i').focus(); const focused = await page.evaluate(() => document.activeElement.id); await page.locator('#i').blur(); await page.locator('#h').waitFor({ state: 'hidden', timeout: 2000 }); await page.waitForLoadState('load'); return { typed: await page.locator('#i').inputValue() === 'hello', focused: focused === 'i', blurred: await page.evaluate(() => document.activeElement.id) !== 'i', title: await page.title() === '' };` },
    { source: `await page.setContent('<div id="v">ready</div>'); await page.evaluate(() => { setTimeout(() => { window.__ready = 7; }, 0); }); const ready = await page.waitForFunction(() => window.__ready || false, null, { timeout: 5000 }); const shot = await page.locator('#v').screenshot(); return { poll: await ready.jsonValue() === 7, screenshot: (shot?.length || shot?.byteLength || 0) > 0, content: (await page.content()).includes('id="v"') };` },
    { source: `const special = await page.evaluate(() => ({ n: NaN, i: Infinity, date: new Date('2024-01-02T03:04:05Z'), re: /abc/i, bi: 9007199254740993n, a: [1, undefined, 3] })); const live = await page.evaluateHandle(() => 42); const before = live.isDisposed(); await live.dispose(); const after = live.isDisposed(); return { nan: Number.isNaN(special.n), infinity: special.i === Infinity, date: special.date instanceof Date, regex: special.re instanceof RegExp && special.re.flags === 'i', bigint: typeof special.bi === 'bigint', array: special.a[1] === undefined || special.a[1] === null, dispose: !before && after };` },
  ]);
  values.forEach((value, index) => everyTrue(`surface-${index + 1}`, value));
});
