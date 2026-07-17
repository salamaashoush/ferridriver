// Ported from crates/ferridriver-cli/tests/backends_support/
// {script_locators,accessible_description,aria_snapshot}.rs — locator
// chains, getBy* engines, frame accessors, waits, uploads, filter(has),
// normalize/highlight and the accessible-description surface. Test
// titles mirror the original Rust fn names.

import { test, describe, expect } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

describe('locators', () => {
  test('script_frame_sync_accessors', async ({ page }) => {
    // Playwright-parity sync frame accessors: name/url/isMainFrame/
    // parentFrame/childFrames/isDetached are all sync (no await).
    await page.goto(
      dataUrl(
        "<h1>Parent</h1><iframe name='alpha' srcdoc='<p>A</p>'></iframe><iframe name='beta' srcdoc='<p>B</p>'></iframe>",
      ),
    );
    await page.waitForSelector("iframe[name='alpha']");
    await page.waitForSelector("iframe[name='beta']");
    const main = page.mainFrame();
    expect(main.isMainFrame()).toBe(true);
    expect(main.parentFrame() == null).toBe(true);
    expect(main.isDetached()).toBe(false);
    const kidNames = main
      .childFrames()
      .map((f) => f.name())
      .sort();
    expect(kidNames).toContain('alpha');
    expect(kidNames).toContain('beta');
    const alpha = page.frame('alpha');
    expect(alpha).not.toBeNull();
    expect(alpha?.name()).toBe('alpha');
    expect(alpha?.isMainFrame()).toBe(false);
    expect(alpha?.parentFrame()?.isMainFrame()).toBe(true);
    expect(page.frames().length).toBeGreaterThanOrEqual(3);
  });

  test('script_frame_selector_union', async ({ page }) => {
    await page.goto(dataUrl("<iframe name='target' src='about:blank'></iframe>"));
    await page.waitForSelector("iframe[name='target']");
    expect(page.frame('target')?.name()).toBe('target');
    expect(page.frame({ name: 'target' })?.name()).toBe('target');
    expect(page.frame({}) == null).toBe(true);
  });

  test('script_wait_for_selector', async ({ page }) => {
    await page.goto(dataUrl("<div id='target'>here</div>"));
    await page.waitForSelector('#target');
  });

  test('script_frame_wait_for_selector_handle', async ({ page }) => {
    // Frame.waitForSelector returns the matched ElementHandle for
    // state 'attached' | 'visible' (default) and null for
    // hidden/detached (client/frame.ts:217).
    await page.goto(dataUrl("<div id='t'>payload-text</div><div id='hid' style='display:none'>x</div>"));
    const main = page.mainFrame();
    const h = await main.waitForSelector('#t');
    expect(h).not.toBeNull();
    expect(await h?.textContent()).toBe('payload-text');
    const hidden = await main.waitForSelector('#hid', { state: 'hidden' });
    expect(hidden == null).toBe(true);
  });

  test('script_frame_wait_for_selector_in_child', async ({ page }) => {
    // waitForSelector resolves inside a child frame and returns that
    // frame's element (not the parent's).
    await page.goto(dataUrl("<iframe name='child' srcdoc=\"<div id='inner'>inner-payload</div>\"></iframe>"));
    await page.waitForSelector("iframe[name='child']");
    const frame = page.frame('child');
    expect(frame).not.toBeNull();
    const h = await frame!.waitForSelector('#inner');
    expect(await h?.textContent()).toBe('inner-payload');
  });

  test('script_wait_for_text', async ({ page }) => {
    await page.goto(
      dataUrl("<body></body><script>setTimeout(function(){document.body.innerHTML='<p>findme</p>'}, 100)</script>"),
    );
    await page.waitForSelector('p');
    expect(await page.textContent('p')).toBe('findme');
  });

  test('script_auto_wait_visibility', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<button style='display:none' id='b' onclick=\"this.textContent='ok'\">Go</button>" +
          "<script>setTimeout(function(){document.getElementById('b').style.display=''},500)</script>",
      ),
    );
    await page.click('#b');
    expect(await page.textContent('#b')).toBe('ok');
  });

  test('script_locator_role', async ({ page }) => {
    await page.goto(dataUrl('<button>Save</button><button disabled>Delete</button>'));
    await page.getByRole('button').first().click();
    expect(await page.getByRole('button').count()).toBe(2);
  });

  test('script_locator_label', async ({ page }) => {
    await page.goto(dataUrl("<label for='e'>Email Address</label><input id='e' type='email'>"));
    await page.getByLabel('Email Address').fill('test@test.com');
    expect(await page.inputValue('#e')).toBe('test@test.com');
  });

  test('script_locator_placeholder', async ({ page }) => {
    await page.goto(dataUrl("<input placeholder='Enter your name' id='n'>"));
    await page.getByPlaceholder('Enter your name').fill('Alice');
    expect(await page.inputValue('#n')).toBe('Alice');
  });

  test('script_locator_text', async ({ page }) => {
    await page.goto(dataUrl('<button>First</button><button>Second</button><button>Third</button>'));
    expect(await page.getByText('Second').textContent()).toBe('Second');
  });

  test('script_locator_nth', async ({ page }) => {
    await page.goto(dataUrl('<button>alpha</button><button>beta</button><button>gamma</button>'));
    expect(await page.getByRole('button').nth(1).textContent()).toBe('beta');
  });

  test('script_locator_all_text', async ({ page }) => {
    await page.goto(dataUrl('<li>a</li><li>b</li><li>c</li>'));
    expect(await page.locator('li').allTextContents()).toEqual(['a', 'b', 'c']);
  });

  test('script_selector_chain', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<div class='a'><button onclick=\"this.textContent='clicked'\">Yes</button></div>" +
          "<div class='b'><button>No</button></div>",
      ),
    );
    await page.locator('.a').locator('button').click();
    expect(await page.locator('.a button').textContent()).toBe('clicked');
  });

  test('script_upload_file', async ({ page }) => {
    // A real on-disk file (written into the test's outputDir through the
    // sandboxed fs global) exercises the path variant of setInputFiles.
    await page.goto(dataUrl("<input type='file' id='f'>"));
    const path = test.info().outputPath('ferridriver_test_upload.txt');
    await fs.writeFile(path, 'test file content');
    await page.setInputFiles('#f', [path]);
    expect(await page.evaluate("document.getElementById('f').files.length")).toBe(1);
    expect(await page.evaluate("document.getElementById('f').files[0].name")).toBe('ferridriver_test_upload.txt');
    expect(await page.evaluate("document.getElementById('f').files[0].size")).toBe(17);
  });

  test('script_set_input_files_hidden_remounting', async ({ page }) => {
    // A hidden, framework-managed file input that re-mounts every 25ms
    // during page setup (the app.acme.com Sign builder shape). The
    // locator funnel re-resolves through the selector engine
    // immediately before each set; the page-side change event reporting
    // the file name proves the set landed on a live node AND fired
    // events.
    await page.goto(
      dataUrl(
        "<div id='mount' data-test-id='file-upload-desktop'></div>" +
          '<script>' +
          "window.uploaded='';" +
          'var flips=0;' +
          'function remake(){' +
          'if(window.uploaded){return;}' +
          "var mount=document.getElementById('mount');" +
          "mount.innerHTML='';" +
          "var input=document.createElement('input');" +
          "input.type='file';" +
          "input.style.display='none';" +
          "input.addEventListener('change',function(){window.uploaded=input.files[0]?input.files[0].name:'';});" +
          'mount.appendChild(input);' +
          'if(++flips<40){setTimeout(remake,25);}' +
          '}' +
          'remake();' +
          '</script>',
      ),
    );
    await page.setInputFiles("[data-test-id='file-upload-desktop'] input[type=file]", {
      name: 'hello.txt',
      mimeType: 'text/plain',
      buffer: new Uint8Array([104, 105]),
    });
    let name = '';
    for (let i = 0; i < 200 && !name; i++) {
      name = (await page.evaluate('window.uploaded')) as string;
    }
    expect(name).toBe('hello.txt');
  });

  test('script_set_input_files_in_iframe', async ({ page }) => {
    // File input inside an iframe, addressed through a frameLocator
    // chain — must resolve against the iframe's own document.
    await page.goto(dataUrl("<iframe id='fr' srcdoc='<input type=file id=up>'></iframe>"));
    await page.frameLocator('#fr').locator('#up').setInputFiles({
      name: 'inner.txt',
      mimeType: 'text/plain',
      buffer: new Uint8Array([105]),
    });
    expect(
      await page.evaluate("document.getElementById('fr').contentDocument.getElementById('up').files[0].name"),
    ).toBe('inner.txt');
  });

  test('script_set_input_files_engine_selector_payload', async ({ page }) => {
    // Engine selector (getByTestId) + payload + page-side content
    // read-back (on WebKit that requires the browser-session
    // Playwright.grantFileReadAccess pairing).
    await page.goto(dataUrl("<input type='file' data-testid='uploader'>"));
    await page.getByTestId('uploader').setInputFiles({
      name: 'engine.txt',
      mimeType: 'text/plain',
      buffer: new Uint8Array([111, 107]),
    });
    const f = (await page.evaluate(
      "(async () => { const f = document.querySelector('input').files[0]; return f ? { name: f.name, content: await f.text() } : null; })()",
    )) as { name: string; content: string } | null;
    expect(f?.name).toBe('engine.txt');
    expect(f?.content).toBe('ok');
  });

  test('script_locator_normalize', async ({ page }) => {
    // locator.normalize() returns a NEW locator whose selector is the
    // canonical recorder form (injected.generateSelectorSimple) — it
    // differs from the input, still resolves to the same single
    // element, and prefers the data-testid attribute.
    await page.goto(
      dataUrl("<button data-testid='save-btn' onclick=\"this.dataset.hit='1'\">Save</button><button>Cancel</button>"),
    );
    const orig = page.getByText('Save');
    const norm = await orig.normalize();
    expect(norm.selector).not.toBe(orig.selector);
    expect(norm.selector).toContain('save-btn');
    expect(await norm.count()).toBe(1);
    await norm.click();
    expect(await page.evaluate("document.querySelector('[data-testid=save-btn]').dataset.hit")).toBe('1');
  });

  test('script_locator_highlight', async ({ page }) => {
    // highlight() installs the Playwright glass-pane overlay
    // (<x-pw-glass>); dispose()/hideHighlight() tear it down. The
    // overlay's presence is the real effect of the call.
    await page.goto(dataUrl("<button id='b'>Target</button>"));
    const loc = page.locator('#b');
    const glassCount = () => page.evaluate("document.querySelectorAll('x-pw-glass').length");
    expect(await glassCount()).toBe(0);
    const disp = await loc.highlight({ style: { outlineColor: 'red', zIndex: 7 } });
    expect(await glassCount()).toBe(1);
    await disp.dispose();
    expect(await glassCount()).toBe(0);
    await loc.highlight();
    expect(await glassCount()).toBe(1);
    await loc.hideHighlight();
    expect(await glassCount()).toBe(0);
  });

  test('script_locator_napi_parity', async ({ page }) => {
    // Locator.selector / isStrict / setStrict / selectText / rightClick
    // / boundingBox — the QuickJS binding mirrors the NAPI surface.
    await page.goto(
      dataUrl(
        "<button id='b' oncontextmenu=\"this.dataset.rc='1';return false\">Target</button>" +
          "<input id='inp' value='select me'>",
      ),
    );
    const b = page.locator('#b');
    expect(b.selector).toBe('#b');
    expect(b.isStrict).toBe(true);
    expect(b.setStrict(false).isStrict).toBe(false);
    const box = await b.boundingBox();
    expect(box != null && box.width > 0 && box.height > 0).toBe(true);
    await b.rightClick();
    expect(await page.evaluate("document.getElementById('b').dataset.rc")).toBe('1');
    await page.locator('#inp').selectText();
    const selText = (await page.evaluate(
      "String(window.getSelection ? document.getSelection().toString() : '') || (document.activeElement && document.activeElement.id)",
    )) as string;
    expect(selText.includes('select me') || selText === 'inp').toBe(true);
  });

  test('script_frame_locator_enter_frame_reads', async ({ page }) => {
    // frameLocator enter-frame hops must resolve through the READ/WAIT
    // paths, not just the action funnel. srcdoc AND data: URL child
    // frames have no name/url a frame-cache heuristic could match — the
    // deterministic content-frame path is the only thing that resolves
    // them.
    const variants = [
      "<iframe srcdoc='<button id=c>child</button>'></iframe>",
      "<iframe src='data:text/html,<button id=c>child</button>'></iframe>",
    ];
    for (const html of variants) {
      await page.goto(dataUrl(html));
      const inner = page.frameLocator('iframe').locator('#c');
      await inner.waitFor({ timeout: 10000 });
      expect(await inner.isVisible()).toBe(true);
      expect(await inner.isAttached()).toBe(true);
      expect(await inner.innerText()).toBe('child');
      expect(await inner.count()).toBe(1);
    }
  });

  test('script_locator_all', async ({ page }) => {
    // locator.all() returns one Locator per matching element, each
    // resolving its OWN element.
    await page.goto(dataUrl('<ul><li>one</li><li>two</li><li>three</li></ul>'));
    const items = await page.locator('li').all();
    expect(items.length).toBe(3);
    const texts = [];
    for (const it of items) {
      texts.push(((await it.textContent()) ?? '').trim());
    }
    expect(texts).toEqual(['one', 'two', 'three']);
  });

  test('script_locator_wait_for_function', async ({ page }) => {
    // locator.waitForFunction polls the element-scoped predicate; a
    // page-side setTimeout flips the text after the first poll, so a
    // pass proves the loop re-polled.
    await page.goto(dataUrl("<div id='t'>pending</div>"));
    const el = page.locator('#t');
    await page.evaluate("setTimeout(() => { document.getElementById('t').textContent = 'ready'; }, 60)");
    await el.waitForFunction((node: Element) => node.textContent === 'ready');
    expect(((await el.textContent()) ?? '').trim()).toBe('ready');
  });

  test('script_locator_filter_has', async ({ page }) => {
    // filter({ has / hasNot }) round-trips a JSON-encoded inner
    // selector through internal:has=; XPath inner locators additionally
    // exercise the relative-.// rewrite of the injected XPathEngine.
    await page.goto(
      dataUrl(
        "<div class='card'><span class='tag'>Signature</span></div>" +
          "<div class='card'><span class='tag'>Date</span></div>",
      ),
    );
    expect(await page.locator('.card').filter({ has: page.locator('.tag') }).count()).toBe(2);
    expect(
      await page
        .locator("//*[contains(@class,'card')]")
        .filter({ has: page.locator("//*[contains(@class,'tag') and contains(.,'Signature')]") })
        .count(),
    ).toBe(1);
    expect(
      await page
        .locator('.card')
        .filter({ hasNot: page.locator("//*[contains(.,'Signature')]") })
        .count(),
    ).toBe(1);
    expect(await page.locator('css=.card >> has=.tag').count()).toBe(2);
  });

  test('script_locator_filter_text_regex', async ({ page }) => {
    // filter({ hasText / hasNotText }) accepts string | RegExp like
    // Playwright — strings are case-insensitive substrings ("quoted"i),
    // RegExp serializes as /source/flags for the injected text engine.
    await page.goto(
      dataUrl(
        "<div class='row'>alpha Change</div>" +
          "<div class='row'>beta keep</div>" +
          "<div class='row'>gamma CHANGED</div>",
      ),
    );
    expect(await page.locator('.row').filter({ hasText: /change/i }).count()).toBe(2);
    expect(await page.locator('.row').filter({ hasText: /^beta keep$/ }).count()).toBe(1);
    expect(await page.locator('.row').filter({ hasNotText: /change/i }).count()).toBe(1);
    expect(await page.locator('.row').filter({ hasText: 'ALPHA' }).count()).toBe(1);
    expect(
      await page
        .locator('.row')
        .filter({ hasText: /change/i })
        .filter({ hasNotText: /gamma/ })
        .count(),
    ).toBe(1);
    // Same union on the locator(selector, options) form.
    expect(await page.locator('.row', { hasText: /keep/i }).count()).toBe(1);
  });

  test('locator_description_getter', async ({ page }) => {
    // locator.describe(x).description() round-trips (Playwright 1.58);
    // a plain locator has no description.
    await page.goto(dataUrl('<button id=go>Go</button>'));
    expect(page.locator('#go').describe('the go button').description()).toBe('the go button');
    expect(page.locator('#go').description() == null).toBe(true);
  });

  test('get_by_role_description', async ({ page }) => {
    // getByRole with a description matcher (Playwright 1.60) selects
    // only the element whose accessible description matches.
    await page.goto(
      dataUrl(
        "<button aria-description='primary action'>Save</button>" +
          "<button aria-description='secondary action'>Cancel</button>",
      ),
    );
    const primary = page.getByRole('button', { description: 'primary action' });
    expect(await primary.count()).toBe(1);
    expect(await primary.textContent()).toBe('Save');
    expect(await page.getByRole('button', { description: 'secondary action' }).textContent()).toBe('Cancel');
  });

  test('aria_snapshot_boxes', async ({ page }) => {
    // ariaSnapshot({ boxes: true }) appends [box=x,y,w,h] annotations;
    // the default snapshot does not (Playwright 1.60).
    await page.goto(dataUrl('<button>Boxed</button>'));
    const withBoxes = await page.locator('body').ariaSnapshot({ boxes: true });
    const without = await page.locator('body').ariaSnapshot();
    expect(withBoxes).toContain('[box=');
    expect(without).not.toContain('[box=');
  });
});
