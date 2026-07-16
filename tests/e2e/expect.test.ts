// Ported from crates/ferridriver-cli/tests/backends_support/expect.rs —
// web-first matchers, Jest value matchers, poll/toPass retry semantics.
// Test titles mirror the original Rust fn names for grep-ability.

import { test, describe, expect } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

describe('expect', () => {
  test('to_be_visible', async ({ page }) => {
    await page.goto(dataUrl("<button id='b'>hi</button><span id='hidden' style='display:none'>x</span>"));
    await expect(page.locator('#b')).toBeVisible();
    await expect(page.locator('#hidden')).not.toBeVisible();
  });

  test('to_have_text', async ({ page }) => {
    await page.goto(dataUrl('<h1>Hello World</h1>'));
    await expect(page.locator('h1')).toHaveText('Hello World');
    await expect(page.locator('h1')).toHaveText(/^Hello/);
  });

  test('to_contain_text', async ({ page }) => {
    await page.goto(dataUrl("<p id='msg'>The quick brown fox</p>"));
    await expect(page.locator('#msg')).toContainText('quick brown');
  });

  test('to_have_count', async ({ page }) => {
    await page.goto(dataUrl('<ul><li>a</li><li>b</li><li>c</li></ul>'));
    await expect(page.locator('li')).toHaveCount(3);
  });

  test('to_have_attribute', async ({ page }) => {
    await page.goto(dataUrl("<a id='lnk' href='https://example.com' data-x>link</a>"));
    await expect(page.locator('#lnk')).toHaveAttribute('href', 'https://example.com');
    await expect(page.locator('#lnk')).toHaveAttribute('data-x');
  });

  test('to_have_value', async ({ page }) => {
    await page.goto(dataUrl("<input id='inp' value='hello' />"));
    await expect(page.locator('#inp')).toHaveValue('hello');
  });

  test('page_title_and_url', async ({ page }) => {
    await page.goto(dataUrl('<title>My Page</title><h1>x</h1>'));
    await expect(page).toHaveTitle('My Page');
    await expect(page).toHaveURL(/^data:/);
  });

  test('value_matchers_in_script', async () => {
    expect(2 + 2).toBe(4);
    expect({ a: 1, b: 2 }).toEqual({ a: 1, b: 2 });
    expect([1, 2, 3]).toContain(2);
    expect({ id: 7 }).toEqual({ id: expect.any(Number) });
    expect({ a: 1, b: 2, c: 3 }).toEqual(expect.objectContaining({ a: 1 }));
  });

  test('to_throw_in_script', async () => {
    await expect(() => {
      throw new Error('boom: bad');
    }).toThrow('bad');
    await expect(() => 42).not.toThrow();
  });

  test('failure_throws', async () => {
    // A failing assertion must throw a JS error — not silently pass.
    let threw = false;
    try {
      expect(1).toBe(2);
    } catch {
      threw = true;
    }
    expect(threw).toBe(true);
  });

  test('poll_with_browser', async ({ page }) => {
    // Counter rises with each call; toEqual(3) becomes true on attempt 3.
    await page.goto(dataUrl("<div id='counter'>0</div>"));
    await page.evaluate('window.__attempt = 0');
    await expect
      .poll(async () => page.evaluate('window.__attempt = (window.__attempt||0)+1'), { timeout: 3000 })
      .toEqual(3);
  });

  test('inline_timeout_option', async ({ page }) => {
    // The inline `{ timeout }` matcher option must bound the retry loop:
    // a 400ms timeout on a never-appearing element fails well under the
    // 5s default.
    await page.goto(dataUrl('<h1>x</h1>'));
    const t = Date.now();
    let failed = false;
    try {
      await expect(page.locator('#never')).toBeVisible({ timeout: 400 });
    } catch {
      failed = true;
    }
    const elapsed = Date.now() - t;
    expect(failed).toBe(true);
    expect(elapsed).toBeGreaterThanOrEqual(300);
    expect(elapsed).toBeLessThan(3000);
  });

  test('to_pass_retries', async () => {
    // toPass retries the callback until it stops throwing; the third
    // attempt succeeds, so exactly 3 attempts must be observed.
    let attempts = 0;
    await expect(async () => {
      attempts += 1;
      if (attempts < 3) {
        throw new Error('not yet');
      }
    }).toPass({ intervals: [50], timeout: 5000 });
    expect(attempts).toBe(3);
  });

  test('to_pass_timeout_and_intervals', async () => {
    // An always-failing callback must time out on the toPass deadline,
    // keep the last error message, and honor the custom 50ms interval
    // schedule (~8 attempts in 400ms, far more than the default's 3).
    const t = Date.now();
    let attempts = 0;
    let msg = '';
    try {
      await expect(async () => {
        attempts += 1;
        throw new Error('always fails');
      }).toPass({ intervals: [50], timeout: 400 });
    } catch (e) {
      msg = String((e as Error).message);
    }
    expect(msg).toContain('always fails');
    expect(attempts).toBeGreaterThanOrEqual(5);
    expect(Date.now() - t).toBeLessThan(3000);
  });

  test('not_to_pass', async () => {
    // `.not.toPass` succeeds as soon as the callback throws.
    await expect(async () => {
      throw new Error('nope');
    }).not.toPass({ timeout: 2000 });
  });

  test('boolean_state_options', async ({ page }) => {
    // Playwright lowers `visible: false` to the hidden assertion,
    // `enabled: false` to disabled, `checked: false` to unchecked, and
    // `editable: false` to readonly (NOT plain negation — a disabled
    // input is neither editable nor readonly).
    await page.goto(
      dataUrl(
        "<span id='gone' style='display:none'>x</span>" +
          "<button id='btn' disabled>b</button>" +
          "<input id='cb' type='checkbox' />" +
          "<input id='ro' readonly value='r' />" +
          "<input id='rw' value='w' />",
      ),
    );
    await expect(page.locator('#gone')).toBeVisible({ visible: false });
    await expect(page.locator('#btn')).toBeEnabled({ enabled: false });
    await expect(page.locator('#cb')).toBeChecked({ checked: false });
    await expect(page.locator('#ro')).toBeEditable({ editable: false });
    let rwReadonly = false;
    try {
      await expect(page.locator('#rw')).toBeEditable({ editable: false, timeout: 400 });
      rwReadonly = true;
    } catch {
      // writable input is not readonly — expected failure
    }
    expect(rwReadonly).toBe(false);
  });

  test('text_match_options', async ({ page }) => {
    // ignoreCase folds both sides; useInnerText reads rendered text
    // (display:none children are excluded, unlike textContent).
    await page.goto(dataUrl("<div id='d'>Hello<span style='display:none'>ZZZ</span></div>"));
    await expect(page.locator('#d')).toHaveText('hellozzz', { ignoreCase: true });
    await expect(page.locator('#d')).toHaveText('Hello', { useInnerText: true });
    await expect(page.locator('#d')).toContainText('HELLO', { ignoreCase: true });
    let exactFailed = false;
    try {
      await expect(page.locator('#d')).toHaveText('Hello', { timeout: 400 });
    } catch {
      exactFailed = true;
    }
    // textContent includes the hidden span, so the plain match must fail.
    expect(exactFailed).toBe(true);
  });

  test('to_have_attribute_overloads', async ({ page }) => {
    // Playwright overloads: (name, value, options?) and (name, options?).
    // An options bag in the second slot is the presence check; ignoreCase
    // applies to the value comparison.
    await page.goto(dataUrl("<a id='lnk' href='HTTPS://EXAMPLE.COM' data-x>link</a>"));
    await expect(page.locator('#lnk')).toHaveAttribute('data-x', { timeout: 2000 });
    await expect(page.locator('#lnk')).toHaveAttribute('href', 'https://example.com', { ignoreCase: true });
    let caseFailed = false;
    try {
      await expect(page.locator('#lnk')).toHaveAttribute('href', 'https://example.com', { timeout: 400 });
    } catch {
      caseFailed = true;
    }
    expect(caseFailed).toBe(true);
  });

  test('new_locator_matchers', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<input id='focus-me' />" +
          "<div id='classy' class='alpha beta'>c</div>" +
          "<div id='styled' style='color: rgb(255, 0, 0)'>s</div>" +
          "<button id='role-btn'>r</button>" +
          "<select id='sel' multiple><option value='a' selected>a</option><option value='b' selected>b</option></select>",
      ),
    );
    await page.locator('#focus-me').focus();
    await expect(page.locator('#focus-me')).toBeFocused();
    await expect(page.locator('#classy')).toHaveClass('alpha beta');
    await expect(page.locator('#classy')).toContainClass('beta');
    await expect(page.locator('#styled')).toHaveCSS('color', 'rgb(255, 0, 0)');
    await expect(page.locator('#classy')).toHaveId('classy');
    await expect(page.locator('#role-btn')).toHaveRole('button');
    await expect(page.locator('#focus-me')).toHaveJSProperty('type', 'text');
    await expect(page.locator('#classy')).toBeInViewport();
    await expect(page.locator('#sel')).toHaveValues(['a', 'b']);
  });

  test('to_have_url_ignore_case', async ({ page }) => {
    await page.goto(dataUrl('<h1>x</h1>'));
    await expect(page).toHaveURL(/^DATA:/i);
    await expect(page).toHaveURL(/^DATA:/, { ignoreCase: true });
    let strictFailed = false;
    try {
      await expect(page).toHaveURL(/^DATA:/, { timeout: 400 });
    } catch {
      strictFailed = true;
    }
    expect(strictFailed).toBe(true);
  });

  test('poll_intervals_option', async () => {
    // A 50ms interval schedule reaches attempt 4 well inside 3s; the
    // default schedule (100/250/500/1000) would take ~1.85s.
    const t = Date.now();
    let n = 0;
    await expect
      .poll(
        () => {
          n += 1;
          return n;
        },
        { intervals: [50], timeout: 3000 },
      )
      .toEqual(4);
    expect(Date.now() - t).toBeLessThan(1500);
  });
});

describe('locator handler', () => {
  // A `#target` button whose click sets `window.__clicked`, fully covered
  // by a fixed `#overlay` that intercepts pointer events — the click can
  // only land once a handler dismisses the overlay.
  const OVERLAY_FIXTURE =
    "<button id='target' onclick='window.__clicked=true'>Click me</button>" +
    "<div id='overlay' style='position:fixed;inset:0;z-index:9999;background:rgba(0,0,0,0.5)'>blocking</div>";

  test('handler dismisses overlay during click actionability', async ({ page }) => {
    await page.goto(dataUrl(OVERLAY_FIXTURE));
    let handlerRuns = 0;
    page.addLocatorHandler(page.locator('#overlay'), async () => {
      handlerRuns += 1;
      await page.evaluate("document.getElementById('overlay').remove()");
    });
    await page.click('#target', { timeout: 10000 });
    expect(handlerRuns).toBe(1);
    expect(await page.evaluate('window.__clicked === true')).toBe(true);
  });

  test('removeLocatorHandler stops the handler from firing', async ({ page }) => {
    await page.goto(dataUrl(OVERLAY_FIXTURE));
    let handlerRuns = 0;
    page.addLocatorHandler(page.locator('#overlay'), async () => {
      handlerRuns += 1;
      await page.evaluate("document.getElementById('overlay').remove()");
    });
    page.removeLocatorHandler(page.locator('#overlay'));
    // With the handler removed the overlay stays: the checkpoint must not
    // invoke it (runs stays 0) and the hit-target check blocks the click —
    // it times out instead of landing through the overlay.
    let msg = '';
    try {
      await page.click('#target', { timeout: 2000 });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    expect(msg).toContain('Timeout');
    expect(handlerRuns).toBe(0);
    expect(await page.evaluate('window.__clicked === true')).toBe(false);
    expect(await page.evaluate("document.getElementById('overlay') !== null")).toBe(true);
  });

  test('times option limits handler invocations', async ({ page }) => {
    await page.goto(dataUrl(OVERLAY_FIXTURE));
    let handlerRuns = 0;
    page.addLocatorHandler(
      page.locator('#overlay'),
      async () => {
        handlerRuns += 1;
        await page.evaluate("document.getElementById('overlay').remove()");
      },
      { times: 1 },
    );
    await page.click('#target', { timeout: 10000 });
    expect(handlerRuns).toBe(1);
    // Re-add the overlay and click again: the handler is exhausted
    // (times: 1 consumed), so it must NOT fire a second time, and the
    // hit-target check blocks the click (timeout, no landing through
    // the overlay).
    await page.evaluate('window.__clicked = false');
    await page.evaluate(
      "const d = document.createElement('div'); d.id = 'overlay'; " +
        "d.style.cssText = 'position:fixed;inset:0;z-index:9999;background:rgba(0,0,0,0.5)'; " +
        'document.body.appendChild(d)',
    );
    let msg = '';
    try {
      await page.click('#target', { timeout: 2000 });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    expect(msg).toContain('Timeout');
    expect(handlerRuns).toBe(1);
    expect(await page.evaluate('window.__clicked === true')).toBe(false);
    expect(await page.evaluate("document.getElementById('overlay') !== null")).toBe(true);
  });
});
