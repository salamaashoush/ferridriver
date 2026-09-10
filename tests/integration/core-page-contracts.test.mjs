import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { dataUrl } from '../e2e/helpers/html';
import { observation, runtimeProbe } from './support.mjs';

const visit = (page, html) => page.goto(dataUrl(html));

test('core Rust callbacks preserve return values, navigation lifetime, and removal', async () => {
  const navigate = { action: 'navigate', url: dataUrl('<body></body>') };
  const { results } = await runtimeProbe([{ op: 'browser-lifecycle', actions: [
    { action: 'new-context' }, { action: 'new-page' },
    { action: 'expose-function', name: 'double', callback: 'double' }, navigate,
    { action: 'evaluate', expression: '(async () => String(await window.double(21)))()' },
    { action: 'expose-function', name: 'greet', callback: 'greet' },
    { action: 'evaluate', expression: "(async () => await window.greet('Rust'))()" }, navigate,
    { action: 'evaluate', expression: '(async () => String(await window.double(5)))()' },
    { action: 'expose-function', name: 'add', callback: 'add' },
    { action: 'evaluate', expression: '(async () => String(await window.add(3,4)))()' },
    { action: 'remove-exposed-function', name: 'double' },
    { action: 'evaluate', expression: 'typeof window.double' },
  ] }]);
  const observed = observation(results[0]);
  assert.equal(observed[4], '42');
  assert.equal(observed[6], 'Hello, Rust!');
  assert.equal(observed[8], '10');
  assert.equal(observed[10], '7');
  assert.equal(observed[12], 'undefined');
});

test('core page storage import and export retain the Playwright storage structure', async () => {
  const { results } = await runtimeProbe([{ op: 'browser-lifecycle', actions: [
    { action: 'new-context' }, { action: 'new-page' },
    { action: 'navigate', url: dataUrl('<body>storage</body>') }, { action: 'storage-state' },
    { action: 'set-storage-state', state: { cookies: [{ name: 'test', value: 'val123', domain: 'localhost',
      path: '/', secure: false, httpOnly: false }], origins: [] } }, { action: 'storage-state' },
  ] }]);
  const observed = observation(results[0]);
  for (const state of [observed[3], observed[5]]) {
    assert.ok(Array.isArray(state.cookies));
    assert.ok(Array.isArray(state.origins));
  }
});

test('core page navigation, evaluation, selectors, and form actions', async ({ page }) => {
  await visit(page, '<title>Hello</title><body>World</body>');
  assert.ok((await page.title()).includes('Hello'));
  assert.ok(page.url().startsWith('data:'));
  assert.equal(await page.evaluate('1 + 2'), 3);
  assert.ok((await page.evaluate("'hello'")).includes('hello'));
  await visit(page, `<button id='b' onclick="this.textContent='clicked'">Go</button>`);
  await page.locator('#b').click();
  assert.ok((await page.locator('#b').textContent()).includes('clicked'));
  await visit(page, "<input id='i' type='text'>");
  await page.locator('#i').fill('hello');
  assert.ok((await page.locator('#i').inputValue()).includes('hello'));
  await visit(page, '<button>Save</button><button>Cancel</button>');
  assert.equal(await page.getByRole('button').count(), 2);
  await visit(page, '<p>Hello World</p><p>Goodbye</p>');
  assert.equal(await page.getByText('Hello').count(), 1);
  await visit(page, "<label for='e'>Email</label><input id='e' type='email'>");
  await page.getByLabel('Email').fill('acme@example.com');
  assert.equal(await page.locator('#e').inputValue(), 'acme@example.com');
  await visit(page, "<div data-testid='card'>Content</div>");
  assert.ok((await page.getByTestId('card').textContent()).includes('Content'));
  await visit(page, "<div class='a'><span>Inside A</span></div><div class='b'><span>Inside B</span></div>");
  assert.ok((await page.locator('css=.a').locator('css=span').textContent()).includes('Inside A'));
  await visit(page, '<ul><li>A</li><li>B</li><li>C</li></ul>');
  assert.ok((await page.locator('css=li').first().textContent()).includes('A'));
  assert.ok((await page.locator('css=li').last().textContent()).includes('C'));
  assert.ok((await page.locator('css=li').nth(1).textContent()).includes('B'));
});

test('core locator state and content accessors reflect the document', async ({ page }) => {
  await visit(page, "<div id='v'>visible</div><div id='h' style='display:none'>hidden</div>");
  assert.equal(await page.locator('#v').isVisible(), true);
  assert.equal(await page.locator('#h').isHidden(), true);
  await visit(page, "<input id='e'><input id='d' disabled>");
  assert.equal(await page.locator('#e').isEnabled(), true);
  assert.equal(await page.locator('#d').isDisabled(), true);
  assert.equal(await page.locator('#e').isEditable(), true);
  assert.equal(await page.locator('#d').isEditable(), false);
  await visit(page, "<input type='checkbox' id='c'>");
  assert.equal(await page.locator('#c').isChecked(), false);
  await page.locator('#c').check();
  assert.equal(await page.locator('#c').isChecked(), true);
  await page.locator('#c').uncheck();
  assert.equal(await page.locator('#c').isChecked(), false);
  await visit(page, "<div id='d'><b>Bold</b> text</div>");
  assert.ok((await page.locator('#d').innerHTML()).includes('<b>'));
  assert.ok((await page.locator('#d').innerText()).includes('Bold'));
  await visit(page, '<h1>Title</h1><p>Body text</p>');
  assert.ok((await page.content()).includes('Title'));
  assert.ok((await page.markdown()).includes('# Title'));
  await visit(page, '<ul><li>Alpha</li><li>Beta</li><li>Gamma</li></ul>');
  const texts = await page.locator('css=li').allTextContents();
  assert.equal(texts.length, 3);
  assert.ok(texts[0].includes('Alpha'));
  assert.ok(texts[2].includes('Gamma'));
  assert.equal(await page.locator('css=li').count(), 3);
});

test('core page waiters observe delayed selector and function results', async ({ page }) => {
  await visit(page, `<div id='d'></div><script>setTimeout(() => {document.getElementById('d').innerHTML='<span id="s">loaded</span>'},200)</script>`);
  await page.waitForSelector('#s', { timeout: 5000 });
  await visit(page, '<script>setTimeout(() => {window.ready=true},200)</script>');
  const expression = await page.waitForFunction('window.ready', undefined, { timeout: 5000 });
  assert.equal(await expression.jsonValue(), true);
  await expression.dispose();
  await visit(page, '<script>setTimeout(() => {window.counter=7},150)</script>');
  const argument = await page.waitForFunction(min => (window.counter || 0) >= min, 5, { polling: 25, timeout: 5000 });
  assert.equal(await argument.jsonValue(), true);
  await argument.dispose();
});

test('core page screenshots, focus, selection, filters, and viewport changes', async ({ page }) => {
  await visit(page, '<h1>Screenshot</h1>');
  const png = await page.screenshot();
  assert.ok(png.length > 100);
  assert.deepEqual(Array.from(png.slice(0, 4)), [0x89, 0x50, 0x4e, 0x47]);
  await visit(page, `<h1 id='h'>0</h1><button id='b' onclick="document.getElementById('h').textContent=Number(document.getElementById('h').textContent)+1">+</button>`);
  await page.locator('#b').dblclick();
  assert.ok((await page.locator('#h').textContent()).includes('2'));
  await visit(page, "<input id='i'>");
  await page.locator('#i').focus();
  assert.ok((await page.evaluate("document.activeElement?.id||''")).includes('i'));
  await page.locator('#i').blur();
  assert.ok(!(await page.evaluate("document.activeElement?.tagName||''")).includes('INPUT'));
  await visit(page, "<select id='s'><option value='a'>Apple</option><option value='b'>Banana</option></select>");
  await page.locator('#s').selectOption({ label: 'Banana' });
  assert.ok((await page.locator('#s').inputValue()).includes('b'));
  await assert.rejects(() => page.locator('#s').click());
  await visit(page, '<div><p>Keep</p></div><div><p>Remove</p></div>');
  assert.equal(await page.locator('css=div').filter({ hasText: 'Keep' }).count(), 1);
  assert.equal(await page.locator('css=div').filter({ hasText: /^keep$/i }).count(), 1);
  assert.equal(await page.locator('css=div').filter({ hasNotText: /remove/i }).count(), 1);
  const dimensions = '<body><script>document.title=innerWidth+"x"+innerHeight</script></body>';
  await visit(page, dimensions);
  const [width, height] = (await page.title()).split('x').map(Number);
  assert.ok(width > 0 && height > 0);
  await page.setViewportSize({ width: 1024, height: 768 });
  await visit(page, dimensions);
  const resized = await page.title();
  assert.ok(resized.includes('1024') && resized.includes('768'));
  await page.setViewportSize({ width: 375, height: 812 });
  await visit(page, dimensions);
  assert.ok((await page.title()).includes('375'));
  await page.setViewportSize({ width, height });
});

test('core AI snapshots retain roles, metadata, depth limits, changes, and node refs', async ({ page }) => {
  await visit(page, "<h1>Hello World</h1><button>Submit</button><a href='#'>Link</a>");
  const initial = await page.snapshotForAI();
  for (const text of ['### Page', 'heading', 'Hello World', 'button', 'Submit', 'link']) assert.ok(initial.full.includes(text), text);
  assert.ok(initial.incremental == null);
  assert.ok(Object.keys(initial.refMap).length > 0);
  await visit(page, '<title>Test Title</title><body>Content</body>');
  const metadata = await page.snapshotForAI();
  assert.ok(metadata.full.includes('Title: Test Title'));
  assert.ok(metadata.full.includes('URL: data:'));
  await visit(page, "<div><ul><li><a href='#'>Deep Link</a></li></ul></div>");
  const deep = await page.snapshotForAI();
  const shallow = await page.snapshotForAI({ depth: 2 });
  assert.ok(deep.full.length >= shallow.full.length);
  await visit(page, '<h1>V1</h1><button>Click</button>');
  const first = await page.snapshotForAI({ track: 't1' });
  assert.ok(first.full.includes('V1'));
  assert.ok(first.incremental == null);
  await visit(page, '<h1>V2</h1><button>Click</button>');
  const changed = await page.snapshotForAI({ track: 't1' });
  assert.ok(changed.full.includes('V2'));
  assert.equal(typeof changed.incremental, 'string');
  assert.ok(changed.incremental.includes('V2'));
  assert.ok((await page.snapshotForAI({ track: 't1' })).incremental == null);
  await visit(page, "<button id='b1'>Save</button><a href='#'>Help</a>");
  const refs = await page.snapshotForAI();
  assert.ok(refs.full.includes('[ref='));
  for (const [label, node] of Object.entries(refs.refMap)) {
    assert.ok(label.startsWith('e'));
    assert.ok(node > 0);
  }
});

test('core init scripts persist across navigation and dispose independently', async ({ page }) => {
  const first = await page.addInitScript("window.__test_init='injected'");
  for (let i = 0; i < 2; i++) {
    await visit(page, "<script>document.title=window.__test_init||'missing'</script>");
    assert.equal(await page.title(), 'injected');
  }
  await page.addInitScript("window.__test_init2='second'");
  await visit(page, "<script>document.title=(window.__test_init||'')+':'+(window.__test_init2||'')</script>");
  assert.equal(await page.title(), 'injected:second');
  for (let i = 0; i < 2; i++) {
    await first.dispose();
    await visit(page, "<script>document.title=(window.__test_init||'gone')+':'+(window.__test_init2||'')</script>");
    assert.equal(await page.title(), 'gone:second');
  }
});

test('core dialogs dismiss by default and accept listener responses', async ({ page }) => {
  await visit(page, "<script>alert('hello');document.title='after_alert'</script>");
  assert.equal(await page.title(), 'after_alert');
  const confirm = "<script>document.title=confirm('sure?')?'yes':'no'</script>";
  await visit(page, confirm);
  assert.equal(await page.title(), 'no');
  await visit(page, "<script>document.title=prompt('name?','default')||'null'</script>");
  assert.equal(await page.title(), 'null');
  page.on('dialog', dialog => dialog.accept());
  await visit(page, confirm);
  assert.equal(await page.title(), 'yes');
  page.removeAllListeners('dialog');
  page.on('dialog', dialog => dialog.accept(dialog.type() === 'prompt' ? 'custom_answer' : undefined));
  await visit(page, "<script>document.title=prompt('name?')||'null'</script>");
  assert.equal(await page.title(), 'custom_answer');
});

test('core inline script and style tags change the rendered page', async ({ page }) => {
  await visit(page, '<body></body>');
  await page.addScriptTag({ content: "document.title='injected'" });
  assert.equal(await page.title(), 'injected');
  await visit(page, "<div id='box'>text</div>");
  await page.addStyleTag({ content: '#box { color: red }' });
  assert.equal(await page.evaluate("getComputedStyle(document.getElementById('box')).color"), 'rgb(255, 0, 0)');
});

test('core load-state waiters resolve only after the requested lifecycle', async ({ page }) => {
  for (const loadState of ['load', 'domcontentloaded', undefined, 'networkidle']) {
    await visit(page, '<body>content</body>');
    await page.waitForLoadState(loadState);
    const state = await page.evaluate('document.readyState');
    if (loadState === 'domcontentloaded') assert.ok(state === 'interactive' || state === 'complete');
    else assert.equal(state, 'complete');
  }
});

test('core locator evaluation serializes elements, arrays, and computed sizes', async ({ page }) => {
  await visit(page, "<ul><li class='item'>Alpha</li><li class='item'>Beta</li><li class='item'>Gamma</li></ul><h1 id='title'>Hello</h1>");
  assert.equal(await page.locator('#title').evaluate(el => el.tagName), 'H1');
  assert.equal(await page.locator('#title').evaluate(el => el.textContent), 'Hello');
  assert.equal(await page.locator('css=.item').evaluateAll(elements => elements.length), 3);
  assert.deepEqual(await page.locator('css=.item').evaluateAll(elements => elements.map(el => el.textContent)), ['Alpha', 'Beta', 'Gamma']);
  const rect = await page.locator('#title').evaluate(el => ({ w: el.offsetWidth, h: el.offsetHeight }));
  assert.ok(rect && rect.w > 0);
});

test('core checkbox setting is idempotent and selection and tap produce input events', async ({ page }) => {
  await visit(page, "<input id='cb' type='checkbox'><input id='inp' type='text' value='select me'>");
  assert.equal(await page.locator('#cb').isChecked(), false);
  for (const checked of [true, false, true, true]) {
    await page.locator('#cb').setChecked(checked);
    assert.equal(await page.locator('#cb').isChecked(), checked);
  }
  await page.locator('#inp').selectText();
  assert.equal(await page.evaluate('window.getSelection().toString()'), 'select me');
  await visit(page, `<button id='btn'>tap me</button><script>var b=document.getElementById('btn');b.addEventListener('touchend',function(){this.textContent='tapped'});b.addEventListener('pointerup',function(e){if(e.pointerType==='touch')this.textContent='tapped'})</script>`);
  await page.locator('#btn').tap();
  assert.equal(await page.locator('#btn').textContent(), 'tapped');
});

test('core page close is observable and idempotent', async ({ context }) => {
  const page = await context.newPage();
  await page.goto('about:blank');
  assert.equal(page.isClosed(), false);
  await page.close();
  assert.equal(page.isClosed(), true);
  await page.close();
  assert.equal(page.isClosed(), true);
});

test('core locator unions and intersections preserve element identity', async ({ page }) => {
  await visit(page, "<button id='a'>Alpha</button><span id='b'>Beta</span><div id='c'>Gamma</div>");
  const combined = page.locator('#a').or(page.locator('#b'));
  assert.equal(await combined.count(), 2);
  assert.equal(await combined.first().textContent(), 'Alpha');
  await visit(page, "<p class='a b'>Both</p><p class='a'>A only</p><p class='b'>B only</p>");
  const intersection = page.locator('css=.a').and(page.locator('css=.b'));
  assert.equal(await intersection.count(), 1);
  assert.equal(await intersection.textContent(), 'Both');
});

test('core routes fulfill, dispose, abort, and unroute requests', async ({ page }) => {
  await page.route('**/mock-page', route => route.fulfill({ status: 200, contentType: 'text/html',
    body: '<html><head><title>Mocked</title></head><body>This page is mocked</body></html>' }));
  await page.goto('http://mock.test/mock-page');
  assert.equal(await page.title(), 'Mocked');
  assert.ok((await page.evaluate('document.body.textContent')).includes('This page is mocked'));
  const api = await page.route('**/api/data', route => route.fulfill({ status: 200,
    contentType: 'application/json', body: '{"mocked":true,"value":42}' }));
  assert.ok((await page.evaluate("(async () => (await fetch('/api/data')).text())()")).includes('"mocked":true'));
  await api.dispose();
  const after = await page.evaluate("(async () => {try { return 'status:'+(await fetch('/api/data')).status; } catch { return 'error'; }})()");
  assert.ok(!after.includes('status:200'));
  await page.route('**/blocked', route => route.abort('blockedbyclient'));
  const blocked = await page.evaluate("(async () => {try {await fetch('/blocked');return 'ok';}catch(e){return 'error:'+e.message;}})()");
  assert.ok(blocked.startsWith('error:'));
  await page.unroute('**/api/data');
});

test('core browser status, context menus, attachment, and viewport accessors', async () => {
  const browser = await chromium().launch({ headless: true });
  try {
    const page = await browser.newPage();
    assert.equal(browser.isConnected(), true);
    assert.ok(browser.version().length > 0);
    assert.ok((await browser.contexts()).length > 0);
    await visit(page, `<div id='target' oncontextmenu="document.title='context_menu';return false" style='padding:20px'>Right click me</div>`);
    await page.locator('#target').rightClick();
    assert.equal(await page.title(), 'context_menu');
    await visit(page, "<div id='exists'>here</div>");
    assert.equal(await page.locator('#exists').isAttached(), true);
    assert.equal(await page.locator('#gone').isAttached(), false);
    await page.goto(dataUrl('<title>Opts</title><body>content</body>'), { waitUntil: 'domcontentloaded', timeout: 10000 });
    assert.equal(await page.title(), 'Opts');
    await page.setViewportSize({ width: 640, height: 480 });
    assert.deepEqual(page.viewportSize(), { width: 640, height: 480 });
    const second = await browser.newPage();
    await second.goto('about:blank');
    assert.equal(second.isClosed(), false);
    await second.close();
    assert.equal(second.isClosed(), true);
  } finally { await browser.close(); }
  assert.equal(browser.isConnected(), false);
});

test('core console storms deliver all 3000 messages and leave commands responsive', async ({ page }) => {
  test.setTimeout(20_000);
  await visit(page, '<title>storm</title>');
  const messages = [];
  let complete;
  const received = new Promise(resolve => { complete = resolve; });
  page.on('console', message => {
    messages.push(message.text());
    if (messages.length === 3000) complete();
  });
  await page.evaluate("for(let i=0;i<3000;i++)console.log('storm-'+i)");
  await received;
  assert.equal(messages.length, 3000);
  assert.deepEqual(messages, Array.from({ length: 3000 }, (_, i) => `storm-${i}`));
  assert.equal(await page.title(), 'storm');
});
