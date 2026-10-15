// Playwright's reads (textContent, getAttribute, isChecked and the rest)
// wait for their element like an action does, and fail with a timeout
// rather than answering for an element that is not there yet. Ported
// from Playwright's locator-convenience.spec.ts and
// page-wait-for-selector-1.spec.ts.
import { test, expect } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';

const failure = (work: Promise<unknown>) => work.then(
  value => { throw new Error(`expected a rejection, got ${JSON.stringify(value)}`); },
  (e: Error) => e,
);

async function addLater(page: import('@ferridriver/test').Page, html: string, ms = 300) {
  await page.evaluate(([markup, delay]: [string, number]) => {
    setTimeout(() => document.body.insertAdjacentHTML('beforeend', markup), delay);
  }, [html, ms] as [string, number]);
}

test.describe('locator reads wait for their element', () => {
  test('every read answers for an element added later', async ({ page }) => {
    await page.setContent('<main></main>');
    await addLater(page, `
      <div id="late" title="later" class="x">late <b>text</b></div>
      <input id="field" value="typed">
      <input id="box" type="checkbox" checked>
      <button id="off" disabled>off</button>`);
    const [text, inner, html, attribute, value, checked, enabled, disabled, editable] = await Promise.all([
      page.locator('#late').textContent(),
      page.locator('#late').innerText(),
      page.locator('#late').innerHTML(),
      page.locator('#late').getAttribute('title'),
      page.locator('#field').inputValue(),
      page.locator('#box').isChecked(),
      page.locator('#off').isEnabled(),
      page.locator('#off').isDisabled(),
      page.locator('#field').isEditable(),
    ]);
    expect(text).toBe('late text');
    expect(inner).toBe('late text');
    expect(html).toBe('late <b>text</b>');
    expect(attribute).toBe('later');
    expect(value).toBe('typed');
    expect(checked).toBe(true);
    expect(enabled).toBe(false);
    expect(disabled).toBe(true);
    expect(editable).toBe(true);
  });

  test('a missing element times out with the call log', async ({ page }) => {
    await page.setContent('<main></main>');
    const started = Date.now();
    const error = await failure(page.locator('#none').textContent({ timeout: 300 }));
    expect(error.name).toBe('TimeoutError');
    expect(error.message).toContain('Timeout 300ms exceeded');
    expect(error.message).toContain(`waiting for locator('#none')`);
    expect(Date.now() - started).toBeGreaterThanOrEqual(250);

    for (const read of [
      () => page.locator('#none').getAttribute('id', { timeout: 100 }),
      () => page.locator('#none').inputValue({ timeout: 100 }),
      () => page.locator('#none').isChecked({ timeout: 100 }),
      () => page.locator('#none').isEnabled({ timeout: 100 }),
      () => page.locator('#none').innerHTML({ timeout: 100 }),
    ]) {
      expect((await failure(read())).name).toBe('TimeoutError');
    }
  });

  test('the page default timeout bounds a read without its own', async ({ page }) => {
    await page.setContent('<main></main>');
    page.setDefaultTimeout(200);
    const error = await failure(page.locator('#none').innerText());
    expect(error.message).toContain('Timeout 200ms exceeded');
  });

  test('a missing attribute is null at once, not a wait', async ({ page }) => {
    await page.setContent('<div id="here"></div>');
    const started = Date.now();
    expect(await page.locator('#here').getAttribute('title', { timeout: 5000 })).toBeNull();
    expect(await page.locator('#here').textContent({ timeout: 5000 })).toBe('');
    expect(Date.now() - started).toBeLessThan(2000);
  });

  test('isVisible and isHidden answer without waiting', async ({ page }) => {
    await page.setContent('<main></main>');
    const started = Date.now();
    expect(await page.locator('#none').isVisible({ timeout: 5000 })).toBe(false);
    expect(await page.locator('#none').isHidden({ timeout: 5000 })).toBe(true);
    expect(await page.isVisible('#none', { timeout: 5000 })).toBe(false);
    expect(Date.now() - started).toBeLessThan(2000);
  });

  test('a read the element rejects fails at once', async ({ page }) => {
    await page.setContent(`
      <div id="plain">plain</div>
      <label>Name <input id="named" value="from label"></label>
      <svg><text id="svg">vector</text></svg>`);
    const started = Date.now();
    expect((await failure(page.locator('#plain').isChecked())).message).toContain('Not a checkbox or radio button');
    expect((await failure(page.locator('#plain').inputValue())).message)
      .toContain('Node is not an <input>, <textarea> or <select> element');
    expect((await failure(page.locator('#svg').innerText())).message).toContain('Node is not an HTMLElement');
    expect(Date.now() - started).toBeLessThan(5000);
    expect(await page.locator('label').inputValue()).toBe('from label');
  });

  test('a locator read is strict', async ({ page }) => {
    await page.setContent('<ul><li>one</li><li>two</li></ul>');
    expect((await failure(page.locator('li').textContent())).message).toContain('strict mode violation');
    expect(await page.locator('li').first().textContent()).toBe('one');
  });

  test('a signal stops a waiting read', async ({ page }) => {
    await page.setContent('<main></main>');
    const controller = new AbortController();
    const read = failure(page.locator('#none').textContent({ signal: controller.signal, timeout: 0 }));
    setTimeout(() => controller.abort('stop reading'), 200);
    const error = await read;
    expect(error.name).toBe('AbortError');
    expect(error.message).toContain('stop reading');
  });
});

test.describe('page and frame reads', () => {
  test('page reads wait, take the first match, and honour strict', async ({ page }) => {
    await page.setContent('<ul><li>one</li><li>two</li></ul>');
    expect(await page.textContent('li')).toBe('one');
    expect((await failure(page.textContent('li', { strict: true }))).message).toContain('strict mode violation');
    expect((await failure(page.isVisible('li', { strict: true }))).message).toContain('strict mode violation');
    expect((await failure(page.getAttribute('#none', 'id', { timeout: 150 }))).name).toBe('TimeoutError');

    await addLater(page, '<p id="late" data-k="v">later</p>');
    expect(await page.getAttribute('#late', 'data-k')).toBe('v');
    expect(await page.innerHTML('#late')).toBe('later');
  });

  test('frame reads run in their own frame', async ({ page }) => {
    await page.goto(`${ORIGIN}/frames/one-frame.html`);
    const frame = page.frame('child')!;
    await frame.waitForLoadState();
    expect(await frame.getAttribute('input, button', 'id')).toBe('frame-input');
    expect((await failure(frame.getAttribute('input, button', 'id', { strict: true }))).message)
      .toContain('strict mode violation');
    expect(await frame.isEnabled('#frame-button')).toBe(true);
    expect((await failure(page.mainFrame().textContent('#frame-button', { timeout: 150 }))).name).toBe('TimeoutError');
  });
});
