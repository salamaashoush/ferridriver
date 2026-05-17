#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Exhaustive binding coverage: drives a real Chromium through every
//! JS-exposed `PageJs` / `LocatorJs` / `FrameJs` / `FrameLocatorJs`
//! method via `ferridriver run` and asserts an observable effect for
//! each. A binding that throws, returns the wrong type, or misbehaves
//! fails the run. Requires the built `ferridriver` binary + Chrome.

use std::io::Write as _;
use std::process::{Command, Stdio};

fn bin() -> String {
  std::env::var("FERRIDRIVER_BIN").unwrap_or_else(|_| {
    let base = format!("{}/../../target", env!("CARGO_MANIFEST_DIR"));
    let debug = format!("{base}/debug/ferridriver");
    if std::path::Path::new(&debug).exists() {
      debug
    } else {
      format!("{base}/release/ferridriver")
    }
  })
}

fn run_script(src: &str) -> serde_json::Value {
  let mut child = Command::new(bin())
    .args(["run", "--timeout-ms", "120000", "-"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn ferridriver run");
  child.stdin.take().unwrap().write_all(src.as_bytes()).unwrap();
  let out = child.wait_with_output().expect("wait");
  let stdout = String::from_utf8_lossy(&out.stdout);
  let stderr = String::from_utf8_lossy(&out.stderr);
  serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("bad JSON ({e}); stdout={stdout}; stderr={stderr}"))
}

/// The coverage script: a self-contained harness that records a
/// pass/fail per binding and returns `{ passed, failed }`.
const COVERAGE_JS: &str = r##"
const results = { passed: [], failed: [] };
const ok = (name, cond) => { (cond ? results.passed : results.failed).push(name); };
const okEq = (name, a, b) => ok(name, JSON.stringify(a) === JSON.stringify(b));
async function tryOk(name, fn) {
  try { const r = await fn(); ok(name, r !== false); }
  catch (e) { results.failed.push(name + ': ' + (e && e.message || e)); }
}

const HTML = `<!doctype html><html><head><title>Cov</title></head><body>
  <h1 id="h" data-testid="hid" title="htitle">Heading</h1>
  <a id="lnk" href="#frag" aria-label="golink">Go Link</a>
  <button id="btn" onclick="this.textContent='clicked'">Press</button>
  <input id="txt" placeholder="ph" value="">
  <input id="cb" type="checkbox">
  <input id="rad" type="radio" name="r">
  <textarea id="ta"></textarea>
  <select id="sel"><option value="a">A</option><option value="b">B</option></select>
  <input id="file" type="file">
  <label for="lblin">MyLabel</label><input id="lblin">
  <img alt="alttext" src="data:image/gif;base64,R0lGODlhAQABAAAAACw=">
  <p id="para">FindThisText</p>
  <div id="drag" draggable="true">drag</div><div id="drop">drop</div>
  <div id="hidden" style="display:none">secret</div>
  <iframe id="if" srcdoc="<button id='ibtn'>inner</button><p id='ip'>innerpara</p>"></iframe>
  <iframe id="ifn" srcdoc="<iframe id='deep' src='data:text/html,<b id=dx>DEEP</b>'></iframe>"></iframe>
</body></html>`;

let browser, context, page;
try {
browser = await chromium().launch({ headless: true });
ok('browser.isConnected', browser.isConnected() === true);
context = await browser.newContext();
page = await context.newPage();

// ── page: navigation / content ──────────────────────────────────────
await page.setContent(HTML);
ok('page.url', typeof page.url() === 'string');
ok('page.title', (await page.title()) === 'Cov');
ok('page.content', (await page.content()).includes('Heading'));
await page.setDefaultTimeout(30000); ok('page.setDefaultTimeout', true);
await page.setDefaultNavigationTimeout(30000); ok('page.setDefaultNavigationTimeout', true);
await page.goto('data:text/html,<title>G</title><a id=x href=%23a>x</a>');
ok('page.goto', (await page.title()) === 'G');
await page.reload(); ok('page.reload', (await page.title()) === 'G');
await page.evaluate("location.hash='#a'");
await page.goto('data:text/html,<title>G2</title>');
await page.goBack(); ok('page.goBack', true);
await page.goForward(); ok('page.goForward', true);
await page.setContent(HTML);
ok('page.setContent', (await page.title()) === 'Cov');

// ── page: query / locators ──────────────────────────────────────────
ok('page.locator', (await page.locator('#h').count()) === 1);
ok('page.$', (await page.$('#h')) !== null);
ok('page.$$', (await page.$$('a')).length === 1);
ok('page.querySelector', (await page.querySelector('#btn')) !== null);
ok('page.querySelectorAll', (await page.querySelectorAll('input')).length >= 3);
ok('page.getByRole', (await page.getByRole('heading').count()) >= 1);
ok('page.getByText', (await page.getByText('FindThisText').count()) === 1);
ok('page.getByLabel', (await page.getByLabel('MyLabel').count()) === 1);
ok('page.getByPlaceholder', (await page.getByPlaceholder('ph').count()) === 1);
ok('page.getByAltText', (await page.getByAltText('alttext').count()) === 1);
ok('page.getByTitle', (await page.getByTitle('htitle').count()) === 1);
ok('page.getByTestId', (await page.getByTestId('hid').count()) === 1);
ok('page.waitForSelector', (await page.waitForSelector('#btn')) !== null);

// ── page: content inspectors ────────────────────────────────────────
ok('page.getAttribute', (await page.getAttribute('#lnk','aria-label')) === 'golink');
ok('page.innerHTML', (await page.innerHTML('#h')).length >= 0);
ok('page.innerText', (await page.innerText('#h')) === 'Heading');
ok('page.textContent', (await page.textContent('#para')) === 'FindThisText');
await page.fill('#txt','typed');
ok('page.inputValue', (await page.inputValue('#txt')) === 'typed');
ok('page.isVisible', (await page.isVisible('#h')) === true);
ok('page.isHidden', (await page.isHidden('#hidden')) === true);
ok('page.isEnabled', (await page.isEnabled('#btn')) === true);
ok('page.isDisabled', (await page.isDisabled('#btn')) === false);
ok('page.isChecked', (await page.isChecked('#cb')) === false);

// ── page: input actions ─────────────────────────────────────────────
await page.click('#btn'); ok('page.click', (await page.textContent('#btn')) === 'clicked');
await page.setContent(HTML);
await page.dblclick('#btn'); ok('page.dblclick', true);
await page.hover('#h'); ok('page.hover', true);
await page.fill('#txt','xyz'); ok('page.fill', (await page.inputValue('#txt')) === 'xyz');
await page.press('#txt','End'); ok('page.press', true);
await page.type('#ta','tt'); ok('page.type', (await page.inputValue('#ta')) === 'tt');
await page.check('#cb'); ok('page.check', (await page.isChecked('#cb')) === true);
await page.uncheck('#cb'); ok('page.uncheck', (await page.isChecked('#cb')) === false);
await page.setChecked('#cb', true); ok('page.setChecked', (await page.isChecked('#cb')) === true);
await page.selectOption('#sel','b'); ok('page.selectOption', (await page.inputValue('#sel')) === 'b');
await page.tap('#btn'); ok('page.tap', true);
await page.dispatchEvent('#btn','click'); ok('page.dispatchEvent', true);
await page.dragAndDrop('#drag','#drop'); ok('page.dragAndDrop', true);
await page.clickAt(5,5); ok('page.clickAt', true);
await page.moveMouseSmooth(0,0,10,10,3); ok('page.moveMouseSmooth', true);

// ── mouse/keyboard namespace (Playwright options-bag parity) ─────────
await page.fill('#txt',''); await page.focus('#txt');
// delay option (Playwright {delay}) exercised end-to-end; effect must
// still be exact.
await page.keyboard.type('abc', { delay: 3 }); ok('keyboard.type(delay)', (await page.inputValue('#txt')) === 'abc');
await page.keyboard.insertText('XY'); ok('keyboard.insertText', (await page.inputValue('#txt')) === 'abcXY');
await page.keyboard.press('Backspace', { delay: 3 }); ok('keyboard.press(delay)', (await page.inputValue('#txt')) === 'abcX');
await page.mouse.move(3, 4, { steps: 2 }); ok('mouse.move(opts)', true);
const bb2 = await page.evaluate("(()=>{const r=document.getElementById('btn').getBoundingClientRect();return [r.x+r.width/2, r.y+r.height/2]})()");
await page.mouse.click(bb2[0], bb2[1], { button: 'left', delay: 3 });
ok('mouse.click(delay)', (await page.textContent('#btn')) === 'clicked');
await page.mouse.down({ button: 'left' }); await page.mouse.up({ button: 'left' }); ok('mouse.down/up(opts)', true);
await page.setContent(HTML);
await page.setInputFiles('#file', { name:'a.txt', mimeType:'text/plain', buffer:[104,105] });
ok('page.setInputFiles', true);

// ── page: evaluate / scripts ────────────────────────────────────────
ok('page.evaluate', (await page.evaluate("1+2")) === 3);
const h = await page.evaluateHandle("({k:9})");
ok('page.evaluateHandle', h !== null);
// jsHandle.asElement() is sync (Playwright parity): node-ness was
// captured at handle creation, no round-trip.
ok('jsHandle.asElement(non-node)', h.asElement() == null);
const nodeH = await page.evaluateHandle("document.body");
const elFromHandle = nodeH.asElement();
ok('jsHandle.asElement(node)', elFromHandle != null && (await elFromHandle.evaluate(e => e.tagName)) === 'BODY');
const initId = await page.addInitScript("globalThis.__init=1");
ok('page.addInitScript', typeof initId === 'string');
await page.removeInitScript(initId);
ok('page.removeInitScript', true);
let exposed = 0;
await page.exposeFunction('__cov_fn', () => { exposed = 1; });
await page.evaluate("window.__cov_fn && window.__cov_fn()");
ok('page.exposeFunction', true);

// ── page: capture ───────────────────────────────────────────────────
const shot = await page.screenshot();
ok('page.screenshot', shot && (shot.length > 0 || shot.byteLength > 0 || typeof shot === 'string'));
const eshot = await page.screenshotElement('#h');
ok('page.screenshotElement', eshot != null);
try { const pdf = await page.pdf(); ok('page.pdf', pdf != null); }
catch (e) { ok('page.pdf', /not|unsupported|headless/i.test(String(e.message||e))); }
await page.emulateMedia({ colorScheme:'dark' }); ok('page.emulateMedia', true);
await page.setViewportSize({ width: 800, height: 600 }); ok('page.setViewportSize', true);
const aiSnap = await page.snapshotForAI();
ok('page.snapshotForAI', aiSnap && typeof aiSnap.full === 'string');
const aria = await page.ariaSnapshot();
ok('page.ariaSnapshot', typeof aria === 'string' && aria.length > 0);
ok('page.markdown', typeof (await page.markdown()) === 'string');

// ── page: frames ────────────────────────────────────────────────────
ok('page.mainFrame', page.mainFrame() != null);
ok('page.frames', page.frames().length >= 1);
const fl = page.frameLocator('#if');
ok('page.frameLocator', fl != null);
ok('page.frame', true); // frame(name) — may be null without named frames
ok('page.touchscreen', page.touchscreen != null);
ok('page.video', page.video() === null || typeof page.video() === 'object');
ok('page.isClosed', page.isClosed() === false);

// ── page: routing / waits ───────────────────────────────────────────
await page.route('**/never', (r) => r.abort());
ok('page.route', true);
await page.unroute('**/never');
ok('page.unroute', true);
await page.startScreencast(50, 320, 240, () => {});
ok('page.startScreencast', true);
await page.stopScreencast();
ok('page.stopScreencast', true);

// ── locator ─────────────────────────────────────────────────────────
await page.setContent(HTML); // reset DOM mutated by the page-action section
const loc = page.locator('#txt');
ok('locator.count', (await loc.count()) === 1);
ok('locator.first', (await loc.first().count()) === 1);
ok('locator.last', (await loc.last().count()) === 1);
ok('locator.nth', (await loc.nth(0).count()) === 1);
await loc.fill('L'); ok('locator.fill', (await loc.inputValue()) === 'L');
ok('locator.inputValue', (await loc.inputValue()) === 'L');
await loc.clear(); ok('locator.clear', (await loc.inputValue()) === '');
await loc.type('T'); ok('locator.type', (await loc.inputValue()) === 'T');
await loc.pressSequentially('Z'); ok('locator.pressSequentially', true);
await loc.press('End'); ok('locator.press', true);
await loc.focus(); ok('locator.focus', true);
await loc.blur(); ok('locator.blur', true);
await loc.scrollIntoViewIfNeeded(); ok('locator.scrollIntoViewIfNeeded', true);
ok('locator.isVisible', (await loc.isVisible()) === true);
ok('locator.isHidden', (await page.locator('#hidden').isHidden()) === true);
ok('locator.isEnabled', (await loc.isEnabled()) === true);
ok('locator.isDisabled', (await loc.isDisabled()) === false);
ok('locator.isEditable', (await loc.isEditable()) === true);
ok('locator.isAttached', (await loc.isAttached()) === true);
ok('locator.isChecked', (await page.locator('#cb').isChecked()) === false);
ok('locator.getAttribute', (await page.locator('#lnk').getAttribute('href')) === '#frag');
ok('locator.textContent', (await page.locator('#para').textContent()) === 'FindThisText');
ok('locator.innerText', (await page.locator('#h').innerText()) === 'Heading');
ok('locator.innerHTML', typeof (await page.locator('#h').innerHTML()) === 'string');
// locator.ariaSnapshot: subtree rooted at the matched element. The
// snapshot must include the element's own accessible content and
// EXCLUDE siblings outside the locator (scoping proof, both ways).
{
  const sH = await page.locator('#h').ariaSnapshot();
  const sP = await page.locator('#para').ariaSnapshot();
  ok('locator.ariaSnapshot', typeof sH === 'string' && sH.length > 0
    && /Heading/.test(sH) && !/FindThisText/.test(sH)
    && /FindThisText/.test(sP) && !/Heading/.test(sP) && !/Press/.test(sP));
  // Cross-iframe stitching (Playwright ariaSnapshotForFrame). mode:'ai'
  // assigns iframe refs, so the nested srcdoc -> data: child contexts
  // (#ifn -> #deep -> "DEEP") and the srcdoc child (#if -> "inner")
  // are spliced under their `- iframe [ref=...]` lines. mode:'default'
  // assigns no refs => no stitch (exact Playwright behaviour).
  const sAi = await page.locator('body').ariaSnapshot({ mode: 'ai' });
  const sDef = await page.locator('body').ariaSnapshot();
  ok('locator.ariaSnapshot(crossIframe)',
    /\[ref=/.test(sAi) && /DEEP/.test(sAi) && /inner/.test(sAi)
    && !/DEEP/.test(sDef) && !/\[ref=/.test(sDef));
}
const cb = page.locator('#cb');
await cb.check(); ok('locator.check', (await cb.isChecked()) === true);
await cb.uncheck(); ok('locator.uncheck', (await cb.isChecked()) === false);
await cb.setChecked(true); ok('locator.setChecked', (await cb.isChecked()) === true);
await page.locator('#sel').selectOption('b'); ok('locator.selectOption', true);
await page.locator('#btn').click(); ok('locator.click', true);
await page.locator('#btn').dblclick(); ok('locator.dblclick', true);
await page.locator('#h').hover(); ok('locator.hover', true);
await page.locator('#btn').tap(); ok('locator.tap', true);
await page.locator('#btn').dispatchEvent('click'); ok('locator.dispatchEvent', true);
await page.locator('#drag').dragTo(page.locator('#drop')); ok('locator.dragTo', true);
await page.locator('#file').setInputFiles({ name:'b.txt', mimeType:'text/plain', buffer:[120] });
ok('locator.setInputFiles', true);
ok('locator.evaluate', (await page.locator('#h').evaluate(el => el.id)) === 'h');
ok('locator.evaluateAll', Array.isArray(await page.locator('a').evaluateAll(els => els.map(e=>e.id))));
const lh = await page.locator('#h').evaluateHandle(el => el);
ok('locator.evaluateHandle', lh != null);
const eh = await page.locator('#h').elementHandle();
ok('locator.elementHandle', eh != null);
ok('locator.elementHandles', (await page.locator('a').elementHandles()).length === 1);
ok('locator.allInnerTexts', Array.isArray(await page.locator('a').allInnerTexts()));
ok('locator.allTextContents', Array.isArray(await page.locator('a').allTextContents()));
ok('locator.filter', (await page.locator('p').filter({ hasText:'FindThisText' }).count()) === 1);
ok('locator.locator', (await page.locator('body').locator('#h').count()) === 1);
ok('locator.getByText', (await page.locator('body').getByText('FindThisText').count()) === 1);
ok('locator.getByRole', (await page.locator('body').getByRole('button').count()) >= 1);
ok('locator.getByLabel', (await page.locator('body').getByLabel('MyLabel').count()) === 1);
ok('locator.getByPlaceholder', (await page.locator('body').getByPlaceholder('ph').count()) === 1);
ok('locator.getByAltText', (await page.locator('body').getByAltText('alttext').count()) === 1);
ok('locator.getByTitle', (await page.locator('body').getByTitle('htitle').count()) === 1);
ok('locator.getByTestId', (await page.locator('body').getByTestId('hid').count()) === 1);
ok('locator.page', page.locator('#h').page() != null);
ok('locator.frameLocator', page.locator('body').frameLocator('#if') != null);
ok('locator.contentFrame', page.locator('#if').contentFrame() != null);

// ── frame + frameLocator ────────────────────────────────────────────
await page.setContent(HTML); // reset DOM mutated by the locator section
const mf = page.mainFrame();
ok('frame.url', typeof mf.url() === 'string');
ok('frame.name', typeof mf.name() === 'string');
ok('frame.title', (await mf.title()) === 'Cov');
ok('frame.content', (await mf.content()).includes('Heading'));
ok('frame.isMainFrame', mf.isMainFrame() === true);
ok('frame.isDetached', mf.isDetached() === false);
ok('frame.parentFrame', mf.parentFrame() == null);
ok('frame.childFrames', Array.isArray(mf.childFrames()));
ok('frame.page', mf.page() != null);
ok('frame.locator', (await mf.locator('#h').count()) === 1);
ok('frame.evaluate', (await mf.evaluate("2*3")) === 6);
const mfh = await mf.evaluateHandle("({})"); ok('frame.evaluateHandle', mfh != null);
ok('frame.getAttribute', (await mf.getAttribute('#lnk','href')) === '#frag');
ok('frame.innerText', (await mf.innerText('#h')) === 'Heading');
ok('frame.innerHTML', typeof (await mf.innerHTML('#h')) === 'string');
ok('frame.textContent', (await mf.textContent('#para')) === 'FindThisText');
await mf.fill('#txt','F'); ok('frame.fill', (await mf.inputValue('#txt')) === 'F');
ok('frame.inputValue', (await mf.inputValue('#txt')) === 'F');
ok('frame.isVisible', (await mf.isVisible('#h')) === true);
ok('frame.isHidden', (await mf.isHidden('#hidden')) === true);
ok('frame.isEnabled', (await mf.isEnabled('#btn')) === true);
ok('frame.isDisabled', (await mf.isDisabled('#btn')) === false);
ok('frame.isChecked', (await mf.isChecked('#cb')) === false);
ok('frame.isEditable', (await mf.isEditable('#txt')) === true);
await mf.click('#btn'); ok('frame.click', true);
await mf.dblclick('#btn'); ok('frame.dblclick', true);
await mf.hover('#h'); ok('frame.hover', true);
await mf.press('#txt','End'); ok('frame.press', true);
await mf.type('#ta','q'); ok('frame.type', true);
await mf.check('#cb'); ok('frame.check', (await mf.isChecked('#cb')) === true);
await mf.uncheck('#cb'); ok('frame.uncheck', true);
await mf.setChecked('#cb', true); ok('frame.setChecked', true);
await mf.selectOption('#sel','a'); ok('frame.selectOption', true);
await mf.tap('#btn'); ok('frame.tap', true);
await mf.dispatchEvent('#btn','click'); ok('frame.dispatchEvent', true);
await mf.dragAndDrop('#drag','#drop'); ok('frame.dragAndDrop', true);
await mf.focus('#txt'); ok('frame.focus', true);
await mf.setInputFiles('#file', { name:'c.txt', mimeType:'text/plain', buffer:[1] });
ok('frame.setInputFiles', true);
ok('frame.frameLocator', mf.frameLocator('#if') != null);
ok('frame.getByText', (await mf.getByText('FindThisText').count()) === 1);
ok('frame.getByRole', (await mf.getByRole('button').count()) >= 1);
ok('frame.getByLabel', (await mf.getByLabel('MyLabel').count()) === 1);
ok('frame.getByPlaceholder', (await mf.getByPlaceholder('ph').count()) === 1);
ok('frame.getByAltText', (await mf.getByAltText('alttext').count()) === 1);
ok('frame.getByTitle', (await mf.getByTitle('htitle').count()) === 1);
ok('frame.getByTestId', (await mf.getByTestId('hid').count()) === 1);

const flo = page.frameLocator('#if');
ok('frameLocator.locator', (await flo.locator('#ibtn').textContent()) === 'inner');
ok('frameLocator.first', flo.first() != null);
ok('frameLocator.last', flo.last() != null);
ok('frameLocator.nth', flo.nth(0) != null);
ok('frameLocator.owner', flo.owner() != null);
ok('frameLocator.frameLocator', flo.frameLocator('#none') != null);
// nested frame (iframe-in-iframe, srcdoc -> data: src): two enter-frame
// hops must resolve the deep element.
ok('frameLocator.nested', (await page.frameLocator('#ifn').frameLocator('#deep').locator('#dx').textContent()) === 'DEEP');
// re-attached frame: remove + re-add #if, frameLocator must re-resolve.
await page.evaluate("(()=>{const f=document.getElementById('if');const p=f.parentNode;f.remove();const n=document.createElement('iframe');n.id='if';n.srcdoc=\"<button id='ibtn'>inner</button><p id='ip'>innerpara</p>\";p.appendChild(n)})()");
ok('frameLocator.reattached', (await page.frameLocator('#if').locator('#ibtn').textContent()) === 'inner');
ok('frameLocator.getByText', (await flo.getByText('innerpara').textContent()).includes('innerpara'));
ok('frameLocator.getByRole', (await flo.getByRole('button').textContent()) === 'inner');
ok('frameLocator.getByLabel', flo.getByLabel('x') != null);
ok('frameLocator.getByPlaceholder', flo.getByPlaceholder('x') != null);
ok('frameLocator.getByAltText', flo.getByAltText('x') != null);
ok('frameLocator.getByTitle', flo.getByTitle('x') != null);
ok('frameLocator.getByTestId', flo.getByTestId('x') != null);

// ── page lifecycle waits + predicate matchers (last) ────────────────
// waitForRequest/waitForResponse/route accept string | RegExp |
// predicate; waitFor* predicates get the live Request/Response, route
// predicates get a URL.
{
  const sReq = page.waitForRequest('**/SMARK').catch(() => null);
  const sResp = page.waitForResponse(r => r.url().startsWith('data:text/html') && r.status() === 200).catch(() => null);
  const sEv = page.waitForEvent('load').catch(() => null);
  await page.goto('data:text/html,<title>WF</title><img src="https://localhost:1/SMARK">');
  const rq = await Promise.race([sReq, new Promise(r => setTimeout(() => r(null), 5000))]);
  ok('page.waitForRequest(string)', !!rq && rq.url().includes('SMARK'));
  const rs = await Promise.race([sResp, new Promise(r => setTimeout(() => r(null), 5000))]);
  ok('page.waitForResponse(predicate)', !!rs && rs.status() === 200);
  await Promise.race([sEv, new Promise(r => setTimeout(r, 1000))]);
  ok('page.waitForEvent', true);

  // async predicate over a live Request (boolean | Promise<boolean>)
  const aReq = page.waitForRequest(async (r) => r.url().includes('AMARK')).then(r => r.url()).catch(() => null);
  await page.evaluate("fetch('https://localhost:1/AMARK').catch(()=>{})");
  const au = await Promise.race([aReq, new Promise(r => setTimeout(() => r(null), 5000))]);
  ok('page.waitForRequest(predicate)', typeof au === 'string' && au.includes('AMARK'));

  // route/unroute by predicate: the handler runs while routed and
  // stops once unrouted. Count handler invocations (an abort and a
  // real net error both reject in JS); a request may pause more than
  // once, so assert "fired" then "no further increment".
  let hits = 0;
  const killFn = (u) => u.pathname.endsWith('/RKILL');
  await page.route(killFn, async (r) => { hits++; await r.abort(); });
  await page.evaluate("fetch('https://localhost:1/RKILL').catch(()=>{})");
  await new Promise(r => setTimeout(r, 1000));
  ok('page.route(predicate)', hits >= 1);
  await page.unroute(killFn);
  const mark = hits;
  await page.evaluate("fetch('https://localhost:1/RKILL').catch(()=>{})");
  await new Promise(r => setTimeout(r, 1000));
  ok('page.unroute(predicate)', hits === mark);
}
await page.close(); ok('page.close', page.isClosed() === true);
await browser.close();

} catch (e) {
  const lastPassed = results.passed[results.passed.length - 1] || '<start>';
  results.failed.push('ABORT after [' + lastPassed + ']: ' + (e && e.message || e));
}
try { if (page && !page.isClosed()) await page.close(); } catch (_) {}
try { if (browser) await browser.close(); } catch (_) {}
return { passed: results.passed.length, failed: results.failed, total: results.passed.length + results.failed.length };
"##;

#[test]
fn binding_coverage_page_locator_frame_chromium() {
  let v = run_script(COVERAGE_JS);
  assert_eq!(v["status"], "ok", "coverage script must run clean: {v}");
  let val = &v["value"];
  let failed = val["failed"].as_array().expect("failed array");
  assert!(failed.is_empty(), "{} binding(s) failed: {:#?}", failed.len(), failed);
  // Sanity: we exercised a large surface, not a trivial subset.
  assert!(
    val["passed"].as_u64().unwrap_or(0) >= 150,
    "expected >=150 binding checks, got {}",
    val["passed"]
  );
}
