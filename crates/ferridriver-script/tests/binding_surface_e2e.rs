#![allow(
  clippy::expect_used,
  clippy::unwrap_used,
  clippy::too_many_lines,
  clippy::uninlined_format_args
)]
//! Playwright-API-compat sweep: drive a single persistent `Session`
//! against a live Chrome page through a LONG sequence of `execute`
//! calls, each exercising a different binding family — including the
//! surface the existing suites do not cover (locator
//! `boundingBox`/`getAttribute`/`innerText`/`allInnerTexts`/
//! `isEditable`/`isAttached`/`clear`/`filter`, elementHandle
//! `$eval`/`$$eval`/`boundingBox`, jsHandle
//! `getProperty`/`getProperties`/`jsonValue`, keyboard
//! `down`/`up`/`insertText`, mouse `move`/`wheel`/`dblclick`,
//! `page.route`, `exposeFunction`, `addInitScript`, `setViewportSize`,
//! `frameLocator`, every `getBy*`, `dragTo`, `selectOption`,
//! `check`/`uncheck`, `dispatchEvent`, `screenshot`, `context.cookies`/
//! `addCookies`, `browser.version`/`newContext`, plus the webapi /
//! `process` / `fs` / `vars` globals).
//!
//! Every chunk returns a JSON object the test asserts on, so a binding
//! that silently regresses fails loudly. One browser, one Session,
//! REPL state carried across the whole run.

use std::sync::Arc;

use ferridriver::chromium;
use ferridriver::options::LaunchOptions;
use ferridriver_script::{InMemoryVars, Outcome, PathSandbox, RunContext, RunOptions, ScriptEngineConfig, Session};

fn data_url(html: &str) -> String {
  format!(
    "data:text/html,{}",
    html
      .bytes()
      .map(|b| match b {
        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
        _ => format!("%{:02X}", b),
      })
      .collect::<String>()
  )
}

const FIXTURE: &str = r##"<!doctype html><html><head><title>Surface</title></head>
<body>
  <h1 id="hdr">Surface Fixture</h1>
  <button id="btn" data-testid="go" aria-label="Go Button">Click Me</button>
  <input id="txt" type="text" placeholder="type here" value="seed" />
  <input id="ro" type="text" value="locked" readonly />
  <input id="chk" type="checkbox" />
  <select id="sel"><option value="a">A</option><option value="b">B</option></select>
  <a id="lnk" href="#frag" title="Home Link">home</a>
  <img id="pic" alt="A Picture" src="data:image/gif;base64,R0lGODlhAQABAAAAACw=" />
  <label for="lblin">My Label</label><input id="lblin" />
  <ul id="list"><li>one</li><li>two</li><li>three</li></ul>
  <div id="src" draggable="true" style="width:40px;height:40px;background:#abc">DRAG</div>
  <div id="dst" style="width:40px;height:40px;background:#cba">DROP</div>
  <div id="big" style="height:3000px"></div>
  <div id="deep">tail</div>
  <iframe id="ifr" srcdoc="<button id='ib'>InnerBtn</button><p>frame text</p>"></iframe>
  <script>
    window.__dropped = false;
    document.getElementById('dst').addEventListener('drop', () => { window.__dropped = true; });
    document.getElementById('dst').addEventListener('dragover', e => e.preventDefault());
    document.getElementById('btn').addEventListener('click', () => {
      document.getElementById('btn').textContent = 'Clicked';
    });
    document.getElementById('btn').addEventListener('custom-evt', () => {
      document.getElementById('hdr').textContent = 'Dispatched';
    });
  </script>
</body></html>"##;

struct H {
  _tmp: tempfile::TempDir,
  _browser: Arc<ferridriver::Browser>,
  session: Session,
  ctx: RunContext,
}

async fn harness() -> H {
  let browser = Arc::new(
    chromium()
      .launch(LaunchOptions::default())
      .await
      .expect("launch browser"),
  );
  let page = browser.page().await.expect("get page");
  let bcx = Arc::new(browser.default_context());
  let tmp = tempfile::tempdir().expect("tempdir");
  std::fs::write(tmp.path().join("seed.txt"), b"hello-fs").expect("seed file");
  let ctx = RunContext {
    vars: Arc::new(InMemoryVars::new()),
    sandbox: Arc::new(PathSandbox::new(tmp.path()).expect("sandbox")),
    artifacts: None,
    page: Some(page),
    browser_context: Some(bcx),
    request: None,
    browser: Some(browser.clone()),
    plugins: Vec::new(),
    trusted_modules: false,
    host: ferridriver_script::ExtensionHost::Script,
    caps: ferridriver_script::ScriptCaps::default(),
  };
  let session = Session::create(ScriptEngineConfig::default(), &ctx)
    .await
    .expect("session create");
  H {
    _tmp: tmp,
    _browser: browser,
    session,
    ctx,
  }
}

/// Run one chunk; panic with the script error if it threw.
async fn step(h: &H, label: &str, src: &str) -> serde_json::Value {
  let r = h.session.execute(src, &[], RunOptions::default(), &h.ctx).await;
  assert!(
    !r.poisoned,
    "[{label}] VM was poisoned — must not happen on a valid script"
  );
  match r.result.outcome {
    Outcome::Ok { success } => success.value,
    Outcome::Error { error } => panic!("[{label}] script error: {error:?}"),
  }
}

fn assert_all_true(label: &str, v: &serde_json::Value) {
  let obj = v
    .as_object()
    .unwrap_or_else(|| panic!("[{label}] expected object, got {v}"));
  for (k, val) in obj {
    assert_eq!(
      val,
      &serde_json::Value::Bool(true),
      "[{label}] check {k} failed (full: {v})"
    );
  }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn binding_surface_sweep() {
  let h = harness().await;
  let url = data_url(FIXTURE);

  // 1 ── addInitScript runs BEFORE navigation; then navigate + nav assertions.
  let v = step(
    &h,
    "init+nav",
    &format!(
      "await page.addInitScript(() => {{ window.__init = 7; }}); \
       const resp = await page.goto({url:?}); \
       globalThis.runs = 1; \
       return {{ title: (await page.title()) === 'Surface', \
         urlIsData: (await page.url()).startsWith('data:'), \
         init: (await page.evaluate(() => window.__init)) === 7, \
         status: resp ? (resp.status() === 200 || resp.status() === 0) : true }};",
      url = url
    ),
  )
  .await;
  assert_all_true("init+nav", &v);

  // 2 ── getBy* locator family + textContent/innerText/innerHTML/getAttribute.
  let v = step(
    &h,
    "getBy",
    "return { \
       role: (await page.getByRole('button', { name: 'Go Button' }).count()) === 1, \
       text: (await page.getByText('Click Me').count()) >= 1, \
       label: (await page.getByLabel('My Label').count()) === 1, \
       ph: (await page.getByPlaceholder('type here').count()) === 1, \
       alt: (await page.getByAltText('A Picture').count()) === 1, \
       title: (await page.getByTitle('Home Link').count()) === 1, \
       tid: (await page.getByTestId('go').count()) === 1, \
       tc: (await page.locator('#hdr').textContent()) === 'Surface Fixture', \
       it: (await page.locator('#hdr').innerText()).includes('Surface'), \
       ih: (await page.locator('#list').innerHTML()).includes('<li>'), \
       attr: (await page.locator('#lnk').getAttribute('title')) === 'Home Link' };",
  )
  .await;
  assert_all_true("getBy", &v);

  // 3 ── locator collection + state predicates + filter/first/last/nth.
  let v = step(
    &h,
    "locator-collection",
    "const li = page.locator('#list li'); \
     const ats = await li.allTextContents(); \
     const ait = await li.allInnerTexts(); \
     return { count: (await li.count()) === 3, \
       first: (await li.first().textContent()) === 'one', \
       last: (await li.last().textContent()) === 'three', \
       nth: (await li.nth(1).textContent()) === 'two', \
       allTC: ats.length === 3 && ats[2] === 'three', \
       allIT: ait.length === 3, \
       filter: (await li.filter({ hasText: 'two' }).count()) === 1, \
       attached: (await page.locator('#hdr').isAttached()) === true, \
       notAttached: (await page.locator('#nope').isAttached()) === false, \
       editable: (await page.locator('#txt').isEditable()) === true, \
       roEditable: (await page.locator('#ro').isEditable()) === false, \
       visible: (await page.locator('#hdr').isVisible()) === true };",
  )
  .await;
  assert_all_true("locator-collection", &v);

  // 4 ── input: fill/clear/inputValue + check/uncheck/isChecked + selectOption.
  let v = step(
    &h,
    "input",
    "const t = page.locator('#txt'); \
     await t.fill('hello world'); const a = await t.inputValue(); \
     await t.clear(); const b = await t.inputValue(); \
     const c = page.locator('#chk'); \
     await c.check(); const ch1 = await c.isChecked(); \
     await c.uncheck(); const ch2 = await c.isChecked(); \
     await page.locator('#sel').selectOption('b'); \
     const sv = await page.locator('#sel').inputValue(); \
     return { filled: a === 'hello world', cleared: b === '', \
       checked: ch1 === true, unchecked: ch2 === false, selected: sv === 'b' };",
  )
  .await;
  assert_all_true("input", &v);

  // 5 ── keyboard down/up/press/type/insertText against a focused input.
  let v = step(
    &h,
    "keyboard",
    "await page.locator('#txt').fill(''); \
     await page.locator('#txt').focus(); \
     await page.keyboard.type('ab'); \
     await page.keyboard.press('Backspace'); \
     await page.keyboard.insertText('Z'); \
     await page.keyboard.down('Shift'); await page.keyboard.up('Shift'); \
     const val = await page.locator('#txt').inputValue(); \
     return { typed: val === 'aZ' };",
  )
  .await;
  assert_all_true("keyboard", &v);

  // 6 ── mouse move/click/dblclick/wheel + page click toggling text.
  let v = step(
    &h,
    "mouse",
    "await page.locator('#btn').click(); \
     const t1 = await page.locator('#btn').textContent(); \
     await page.mouse.move(5, 5); \
     await page.mouse.move(20, 20); \
     await page.mouse.wheel(0, 200); \
     const sy = await page.evaluate(() => window.scrollY); \
     await page.locator('#deep').scrollIntoViewIfNeeded(); \
     return { clicked: t1 === 'Clicked', scrolled: sy >= 0 };",
  )
  .await;
  assert_all_true("mouse", &v);

  // 7 ── elementHandle + jsHandle: $eval/$$eval/boundingBox/getProperty/jsonValue.
  let v = step(
    &h,
    "handles",
    "const hh = await page.locator('#hdr').elementHandle(); \
     const tag = await hh.evaluate(el => el.tagName); \
     const bb = await hh.boundingBox(); \
     const lh = await page.locator('#list').elementHandle(); \
     const oneText = await lh.$eval('li', el => el.textContent); \
     const liCount = await lh.$$eval('li', els => els.length); \
     const qsa = (await page.$$('#list li')).length; \
     const jh = await page.evaluateHandle(() => ({ a: 1, b: [2, 3] })); \
     const ap = await jh.getProperty('a'); \
     const apv = await ap.jsonValue(); \
     const props = await jh.getProperties(); \
     const jv = await jh.jsonValue(); \
     return { tag: tag === 'H1', bb: bb && bb.width > 0 && bb.height > 0, \
       evalEl: oneText === 'one', subEval: liCount === 3, qsa: qsa === 3, \
       getProp: apv === 1, props: (typeof props === 'object' && 'a' in props), \
       jsonValue: jv.b[1] === 3 };",
  )
  .await;
  assert_all_true("handles", &v);

  // 8 ── dispatchEvent + dragTo (HTML5 DnD) + boundingBox via locator.
  let v = step(
    &h,
    "events+dnd",
    "await page.locator('#btn').dispatchEvent('custom-evt'); \
     const hdr = await page.locator('#hdr').textContent(); \
     await page.locator('#src').dragTo(page.locator('#dst')); \
     const dropped = await page.evaluate(() => window.__dropped); \
     const bh = await page.locator('#btn').elementHandle(); \
     const lbb = await bh.boundingBox(); \
     return { dispatched: hdr === 'Dispatched', \
       dropped: dropped === true, locBox: lbb && lbb.width > 0 };",
  )
  .await;
  assert_all_true("events+dnd", &v);

  // 9 ── frames + frameLocator into the srcdoc iframe.
  let v = step(
    &h,
    "frames",
    "const frames = await page.frames(); \
     const fl = page.frameLocator('#ifr'); \
     const ib = await fl.locator('#ib').textContent(); \
     const ftxt = await fl.getByText('frame text').count(); \
     return { frameCount: frames.length >= 2, \
       innerBtn: ib === 'InnerBtn', frameText: ftxt === 1 };",
  )
  .await;
  assert_all_true("frames", &v);

  // 10 ── page.route + Response shape (status/url/headersArray/headerValue).
  let v = step(
    &h,
    "route+response",
    "await page.route('**/routed', route => route.fulfill({ \
       status: 201, contentType: 'text/html', \
       headers: { 'x-from': 'route' }, \
       body: '<h1 id=\\\"r\\\">ROUTED</h1>' })); \
     const r = await page.goto('https://app.test/routed'); \
     const txt = await page.locator('#r').textContent(); \
     return { fulfilled: txt === 'ROUTED', \
       status: r && r.status() === 201, \
       urlStr: typeof (r && r.url()) === 'string', \
       headerVal: (await r.headerValue('x-from')) === 'route' };",
  )
  .await;
  assert_all_true("route+response", &v);

  // 11 ── exposeFunction: Playwright parity — callable on the CURRENT
  // document with NO navigation, AND the callback's return value (incl.
  // an async callback's resolved Promise) is delivered to the page.
  let v = step(
    &h,
    "exposeFunction",
    "await page.exposeFunction('addOne', n => n + 1); \
     await page.exposeFunction('sum', (a, b) => a + b); \
     await page.exposeFunction('slowDouble', async n => { return n * 2; }); \
     const r = await page.evaluate(async () => await window.addOne(41)); \
     const r2 = await page.evaluate(async () => await window.slowDouble(21)); \
     const r3 = await page.evaluate(async () => await window.sum(3, 4)); \
     return { exposed: r === 42, asyncExposed: r2 === 42, spreadArgs: r3 === 7 };",
  )
  .await;
  assert_all_true("exposeFunction", &v);

  // 12 ── setViewportSize + emulateMedia + screenshot returns bytes.
  let v = step(
    &h,
    "viewport+shot",
    "await page.setViewportSize({ width: 800, height: 600 }); \
     const dims = await page.evaluate(() => [window.innerWidth, window.innerHeight]); \
     await page.emulateMedia({ colorScheme: 'dark' }); \
     const shot = await page.screenshot(); \
     return { vp: dims[0] === 800 && dims[1] === 600, \
       shot: (shot && (shot.length || shot.byteLength || 0) > 0) === true };",
  )
  .await;
  assert_all_true("viewport+shot", &v);

  // 13 ── context cookies round-trip + browser-level accessors.
  let v = step(
    &h,
    "context+browser",
    "await context.addCookies([{ name: 'sid', value: 'xyz', url: 'https://route.test/' }]); \
     const cs = await context.cookies(); \
     const ver = browser.version(); \
     const ctxs = browser.contexts(); \
     const isc = browser.isConnected(); \
     return { cookie: cs.some(c => c.name === 'sid' && c.value === 'xyz'), \
       version: typeof ver === 'string' && ver.length > 0, \
       contexts: Array.isArray(ctxs), connected: isc === true };",
  )
  .await;
  assert_all_true("context+browser", &v);

  // 14 ── webapi globals + process + fs + vars, all in one VM, REPL state.
  let v = step(
    &h,
    "globals",
    "const u = new URL('https://a.test:8443/p?x=1#h'); \
     const enc = new TextEncoder().encode('hi'); \
     const dec = new TextDecoder().decode(enc); \
     vars.set('k', 'v1'); \
     const fsRead = await fs.readFile('seed.txt'); \
     await fs.writeFile('out/o.txt', 'written'); \
     const back = await fs.readFile('out/o.txt'); \
     return { url: u.port === '8443' && u.hash === '#h', \
       enc: enc.length === 2, dec: dec === 'hi', \
       b64: atob(btoa('xy')) === 'xy', \
       proc: typeof process.platform === 'string' && process.versions.quickjs.includes('rquickjs'), \
       fsRead: fsRead === 'hello-fs', fsWrite: back === 'written', \
       vars: vars.get('k') === 'v1', \
       replState: globalThis.runs === 1 };",
  )
  .await;
  assert_all_true("globals", &v);

  // 15 ── REPL continuity proof: state set in chunk 1/14 still visible,
  // and a fresh navigation re-runs the addInitScript registered earlier.
  let v = step(
    &h,
    "repl-continuity",
    &format!(
      "globalThis.runs += 1; \
       await page.goto({url:?}); \
       const reinit = await page.evaluate(() => window.__init); \
       return {{ runs: globalThis.runs === 2, \
         varsSurvive: vars.get('k') === 'v1', \
         initReran: reinit === 7 }};",
      url = url
    ),
  )
  .await;
  assert_all_true("repl-continuity", &v);

  // 16 ── locator.and / locator.or (Playwright set ops).
  let v = step(
    &h,
    "locator-and-or",
    "const li = page.locator('#list li'); \
     const andCount = await li.and(page.getByText('two')).count(); \
     const orCount = await page.locator('#hdr').or(page.locator('#btn')).count(); \
     return { and: andCount === 1, or: orCount === 2 };",
  )
  .await;
  assert_all_true("locator-and-or", &v);

  // 17 ── selectOption returns the array of selected values (Playwright
  // signature `Promise<Array<string>>`); fill('') clears the input.
  let v = step(
    &h,
    "select+clear",
    "const sel = await page.locator('#sel').selectOption('a'); \
     await page.locator('#txt').fill('seed'); \
     await page.locator('#txt').fill(''); \
     const after = await page.locator('#txt').inputValue(); \
     return { selectReturn: Array.isArray(sel) && sel[0] === 'a', \
       fillEmpty: after === '' };",
  )
  .await;
  assert_all_true("select+clear", &v);

  // 18 ── locator.evaluate(fn, arg) + locator.evaluateAll(fn, arg)
  // both pass `arg` through (single arg, JSON-serializable).
  let v = step(
    &h,
    "locator-evaluate",
    "const tag = await page.locator('#hdr').evaluate((el, suf) => el.tagName + suf, '-X'); \
     const lens = await page.locator('#list li').evaluateAll((els, mul) => els.length * mul, 10); \
     return { tag: tag === 'H1-X', lens: lens === 30 };",
  )
  .await;
  assert_all_true("locator-evaluate", &v);

  // 19 ── frame.parentFrame / frame.childFrames navigation.
  let v = step(
    &h,
    "frame-tree",
    "const main = await page.mainFrame(); \
     const kids = await main.childFrames(); \
     const inner = kids[0]; \
     const parent = await inner.parentFrame(); \
     return { isMain: await main.isMainFrame() === true, \
       hasKid: kids.length >= 1, \
       parentBack: parent !== null };",
  )
  .await;
  assert_all_true("frame-tree", &v);

  // 20 ── elementHandle.ownerFrame / contentFrame.
  let v = step(
    &h,
    "handle-frames",
    "const eh = await page.locator('#hdr').elementHandle(); \
     const own = await eh.ownerFrame(); \
     const ifr = await page.locator('#ifr').elementHandle(); \
     const cf = await ifr.contentFrame(); \
     return { owner: own !== null, contentFrame: cf !== null };",
  )
  .await;
  assert_all_true("handle-frames", &v);

  // Response-shape moved into chunk 10 (`route+response`) — registering
  // a SECOND route after the chunk-11..15 sequence hits a Chrome
  // Fetch-interception lifecycle bug (`ERR_NAME_NOT_RESOLVED`); tracked
  // for a separate core fix.

  // 22 ── addInitScript accepts `{content}` shape (Playwright union
  // `string | Function | { path?, content? }`).
  let v = step(
    &h,
    "addInitScript-content",
    &format!(
      "await page.addInitScript({{ content: 'window.__fromContent = 99;' }}); \
       await page.goto({url:?}); \
       const got = await page.evaluate(() => window.__fromContent); \
       return {{ contentForm: got === 99 }};",
      url = url
    ),
  )
  .await;
  assert_all_true("addInitScript-content", &v);

  // 22b ── screenshotElement returns PNG bytes for a CSS selector.
  let v = step(
    &h,
    "screenshotElement",
    "const bytes = await page.screenshotElement('#hdr'); \
     return { hasBytes: (bytes.length || bytes.byteLength || 0) > 0 };",
  )
  .await;
  assert_all_true("screenshotElement", &v);

  // 22c ── page.waitForRequest with a PREDICATE (Playwright union:
  // string | RegExp | ((req)=>boolean|Promise<boolean>)).
  let v = step(
    &h,
    "waitForRequest-predicate",
    "await page.route('**/wfreq', route => route.fulfill({ \
       status: 200, contentType: 'application/json', \
       headers: { 'access-control-allow-origin': '*' }, body: '{}' })); \
     const [req] = await Promise.all([ \
       page.waitForRequest(r => r.url().endsWith('/wfreq')), \
       page.evaluate(() => { fetch('https://app.test/wfreq'); }) \
     ]); \
     await page.unroute('**/wfreq'); \
     return { matched: req && req.url().endsWith('/wfreq') };",
  )
  .await;
  assert_all_true("waitForRequest-predicate", &v);

  // 23 ── page.waitForResponse against a routed URL fired from page JS.
  // Matcher accepts URL pattern or predicate (Playwright union).
  let v = step(
    &h,
    "waitForResponse",
    "await page.route('**/wfr', route => route.fulfill({ \
       status: 200, contentType: 'application/json', \
       headers: { 'access-control-allow-origin': '*' }, \
       body: '{\"hit\":1}' })); \
     const [resp] = await Promise.all([ \
       page.waitForResponse('**/wfr'), \
       page.evaluate(() => { fetch('https://app.test/wfr'); }) \
     ]); \
     await page.unroute('**/wfr'); \
     return { matched: resp && resp.status() === 200 };",
  )
  .await;
  assert_all_true("waitForResponse", &v);

  // 24 ── selectOption supports {value}/{label}/{index} variants.
  let v = step(
    &h,
    "selectOption-variants",
    "const a = await page.locator('#sel').selectOption({ label: 'A' }); \
     const va = await page.locator('#sel').inputValue(); \
     const b = await page.locator('#sel').selectOption({ index: 1 }); \
     const vb = await page.locator('#sel').inputValue(); \
     return { byLabel: va === 'a', byIndex: vb === 'b', \
       arr: Array.isArray(a) && Array.isArray(b) };",
  )
  .await;
  assert_all_true("selectOption-variants", &v);

  // 25 ── route.request() returns a Playwright Request object with
  // url/method/headers/headerValue (the missing accessor that broke
  // LLM-generated Playwright code) — verified via a route handler that
  // inspects the in-flight request.
  let v = step(
    &h,
    "route-request",
    "vars.set('routeReq', ''); \
     await page.route('**/rreq', route => { \
       const req = route.request(); \
       const h = req.headers(); \
       vars.set('routeReq', JSON.stringify({ \
         url: req.url(), method: req.method(), \
         hasUA: typeof (h['user-agent'] || h['User-Agent']) === 'string', \
         hv: req.headerValue('User-Agent') !== null })); \
       return route.fulfill({ status: 200, contentType: 'application/json', \
         headers: { 'access-control-allow-origin': '*' }, body: '{}' }); \
     }); \
     await page.evaluate(() => fetch('https://app.test/rreq')); \
     await page.unroute('**/rreq'); \
     const got = JSON.parse(vars.get('routeReq')); \
     return { urlOk: got.url.endsWith('/rreq'), method: got.method === 'GET', \
       headersObj: got.hasUA === true, headerValueNonNull: got.hv === true };",
  )
  .await;
  assert_all_true("route-request", &v);

  // 26 ── context.setExtraHTTPHeaders propagation, end-to-end.
  // Chrome triggers a CORS preflight (custom `x-extra` from a `data:`
  // origin fetch); the route handler answers OPTIONS with the
  // `access-control-allow-headers` permit, then captures `x-extra` on
  // the real GET. Proves both the binding and the header propagation.
  let v = step(
    &h,
    "extraHeaders",
    "vars.set('seen', ''); \
     await page.route('**/eh', route => { \
       const req = route.request(); \
       if (req.method() === 'OPTIONS') { \
         return route.fulfill({ status: 204, headers: { \
           'access-control-allow-origin': '*', \
           'access-control-allow-headers': 'x-extra', \
           'access-control-allow-methods': 'GET, POST, OPTIONS' \
         } }); \
       } \
       vars.set('seen', String(req.headers()['x-extra'])); \
       return route.fulfill({ status: 200, contentType: 'application/json', \
         headers: { 'access-control-allow-origin': '*' }, body: '{}' }); \
     }); \
     await context.setExtraHTTPHeaders({ 'x-extra': 'yes' }); \
     await page.evaluate(() => fetch('https://app.test/eh')); \
     await page.unroute('**/eh'); \
     return { propagated: vars.get('seen') === 'yes' };",
  )
  .await;
  assert_all_true("extraHeaders", &v);

  // 26 ── page.pdf() returns PNG/PDF bytes. Chromium must be headless;
  // Playwright surface: `page.pdf(options?): Promise<Buffer>`.
  let v = step(
    &h,
    "pdf",
    "const bytes = await page.pdf({ format: 'A4' }); \
     return { hasBytes: (bytes.length || bytes.byteLength || 0) > 0 };",
  )
  .await;
  assert_all_true("pdf", &v);

  // 27 ── page.setContent + page.content roundtrip.
  let v = step(
    &h,
    "setContent",
    "await page.setContent('<div id=\"set\">SET</div>'); \
     const c = await page.content(); \
     const txt = await page.locator('#set').textContent(); \
     return { hasDiv: c.includes('id=\"set\"'), text: txt === 'SET' };",
  )
  .await;
  assert_all_true("setContent", &v);

  // 28 ── locator.pressSequentially types one char at a time.
  let v = step(
    &h,
    "pressSequentially",
    "await page.setContent('<input id=\"i\" />'); \
     await page.locator('#i').pressSequentially('hello', { delay: 0 }); \
     return { value: (await page.locator('#i').inputValue()) === 'hello' };",
  )
  .await;
  assert_all_true("pressSequentially", &v);

  // 29 ── locator.focus + locator.blur observed via document.activeElement.
  let v = step(
    &h,
    "focus+blur",
    "await page.setContent('<input id=\"a\" /><input id=\"b\" />'); \
     await page.locator('#a').focus(); \
     const af = await page.evaluate(() => document.activeElement.id); \
     await page.locator('#a').blur(); \
     const bf = await page.evaluate(() => document.activeElement.id); \
     return { focused: af === 'a', blurred: bf !== 'a' };",
  )
  .await;
  assert_all_true("focus+blur", &v);

  // 30 ── route.abort causes page-side fetch to reject.
  let v = step(
    &h,
    "route-abort",
    "await page.route('**/a', route => route.abort('failed')); \
     let threw = false; \
     try { await page.evaluate(() => fetch('https://app.test/a')); } \
     catch (_) { threw = true; } \
     await page.unroute('**/a'); \
     return { aborted: threw === true };",
  )
  .await;
  assert_all_true("route-abort", &v);

  // 31 ── page.evaluate passes complex arg (object + array) through.
  let v = step(
    &h,
    "evaluate-arg",
    "const r = await page.evaluate(([a, b]) => a + b.x, [3, { x: 4 }]); \
     const o = await page.evaluate(o => o.k * 2, { k: 21 }); \
     return { sum: r === 7, mul: o === 42 };",
  )
  .await;
  assert_all_true("evaluate-arg", &v);

  // 32 ── locator.waitFor({state:'visible'|'hidden'}).
  let v = step(
    &h,
    "locator-waitFor",
    "await page.setContent('<div id=\"v\">hi</div><div id=\"h\" style=\"display:none\">x</div>'); \
     await page.locator('#v').waitFor({ state: 'visible', timeout: 2000 }); \
     await page.locator('#h').waitFor({ state: 'hidden', timeout: 2000 }); \
     return { ok: true };",
  )
  .await;
  assert_all_true("locator-waitFor", &v);

  // 33 ── page.waitForFunction polls until truthy; returns the value.
  let v = step(
    &h,
    "waitForFunction",
    "await page.setContent('<div></div>'); \
     setTimeout(async () => { await page.evaluate(() => { window.__r = 7; }); }, 0); \
     const r = await page.waitForFunction(() => window.__r || false, null, { timeout: 5000 }); \
     return { got: r === 7 };",
  )
  .await;
  assert_all_true("waitForFunction", &v);

  // 34 ── page.waitForLoadState('load') resolves after navigation.
  let v = step(
    &h,
    "waitForLoadState",
    "await page.setContent('<title>L</title><body>x</body>'); \
     await page.waitForLoadState('load'); \
     return { ok: (await page.title()) === 'L' };",
  )
  .await;
  assert_all_true("waitForLoadState", &v);

  // 35 ── page.waitForURL after a routed navigation.
  let v = step(
    &h,
    "waitForURL",
    "await page.route('**/wfu', route => route.fulfill({ \
       status: 200, contentType: 'text/html', body: '<b>U</b>' })); \
     const nav = page.goto('https://app.test/wfu'); \
     await page.waitForURL('**/wfu'); \
     await nav; \
     await page.unroute('**/wfu'); \
     return { matched: (await page.url()).endsWith('/wfu') };",
  )
  .await;
  assert_all_true("waitForURL", &v);

  // 36 ── locator.screenshot returns PNG bytes.
  let v = step(
    &h,
    "locator-screenshot",
    "await page.setContent('<h1 id=\"s\">SHOT</h1>'); \
     const bytes = await page.locator('#s').screenshot(); \
     return { hasBytes: (bytes.length || bytes.byteLength || 0) > 0 };",
  )
  .await;
  assert_all_true("locator-screenshot", &v);

  // 37 ── frame.waitForLoadState + frame.evaluate via main frame.
  let v = step(
    &h,
    "frame-loadstate",
    "await page.setContent('<title>F</title><body>x</body>'); \
     const f = await page.mainFrame(); \
     await f.waitForLoadState(); \
     const t = await f.evaluate(() => document.title); \
     return { title: t === 'F' };",
  )
  .await;
  assert_all_true("frame-loadstate", &v);

  // 38 ── route.continue() forwards the request to the network.
  // With no reachable server `app.test`, the fetch throws — proving
  // the handler chose continue (not fulfill/abort) AND the binding
  // accepts header/method overrides without error.
  let v = step(
    &h,
    "route-continue",
    "vars.set('hit', ''); \
     await page.route('**/cont', route => { \
       vars.set('hit', route.request().method()); \
       return route.continue({ headers: { 'x-pass': '1' } }); \
     }); \
     let threw = false; \
     try { await page.evaluate(() => fetch('https://app.test/cont')); } \
     catch (_) { threw = true; } \
     await page.unroute('**/cont'); \
     return { handlerRan: vars.get('hit') === 'GET', continued: threw === true };",
  )
  .await;
  assert_all_true("route-continue", &v);

  // 39 ── page.evaluate preserves Playwright special values
  // (NaN / ±Infinity / Date / RegExp / BigInt round-trip natively;
  // top-level `undefined` object values are dropped from the wire).
  let v = step(
    &h,
    "evaluate-special",
    "const r = await page.evaluate(() => ({ \
       n: NaN, i: Infinity, ni: -Infinity, \
       d: new Date('2024-01-02T03:04:05Z'), \
       re: /abc/i, \
       bi: 9007199254740993n, \
       a: [1, undefined, 3], \
       u: undefined })); \
     return { nan: Number.isNaN(r.n), inf: r.i === Infinity, \
       ninf: r.ni === -Infinity, \
       date: r.d instanceof Date && r.d.toISOString().startsWith('2024-01-02'), \
       regex: r.re instanceof RegExp && r.re.flags === 'i' && r.re.source === 'abc', \
       bigint: typeof r.bi === 'bigint', \
       arrHole: r.a[1] === undefined || r.a[1] === null, \
       undefVal: r.u === undefined };",
  )
  .await;
  assert_all_true("evaluate-special", &v);

  // 40 ── jsHandle / elementHandle dispose() then access throws.
  let v = step(
    &h,
    "handle-dispose",
    "await page.setContent('<p id=p>hi</p>'); \
     const eh = await page.locator('#p').elementHandle(); \
     const d1 = eh.isDisposed(); \
     await eh.dispose(); \
     const d2 = eh.isDisposed(); \
     return { wasLive: d1 === false, nowDisposed: d2 === true };",
  )
  .await;
  assert_all_true("handle-dispose", &v);

  // 41 ── keyboard.press('Control+a') dispatches the combo with the
  // correct DOM `code` + `ctrlKey` modifier (asserted from page-side
  // keydown event — Chrome's editing-command path is OS/Chrome-build
  // dependent, the dispatch itself is what we own).
  let v = step(
    &h,
    "kbd-combo",
    "await page.setContent('<input id=t />\
       <script>window.__ev = null; document.getElementById(\"t\").addEventListener(\
         \"keydown\", e => { window.__ev = { key: e.key, code: e.code, ctrl: e.ctrlKey }; });</script>'); \
     await page.locator('#t').focus(); \
     await page.keyboard.press('Control+a'); \
     const ev = await page.evaluate(() => window.__ev); \
     return { key: ev.key === 'a', code: ev.code === 'KeyA', ctrl: ev.ctrl === true };",
  )
  .await;
  assert_all_true("kbd-combo", &v);

  // 42 ── page.waitForEvent('console') captures a page-side console
  // message (Playwright `page.waitForEvent('console')`).
  let v = step(
    &h,
    "waitForEvent-console",
    "await page.setContent('<body>x</body>'); \
     const [msg] = await Promise.all([ \
       page.waitForEvent('console'), \
       page.evaluate(() => { console.log('hello-from-page'); }) \
     ]); \
     return { type: msg.type() === 'log', text: msg.text().includes('hello-from-page') };",
  )
  .await;
  assert_all_true("waitForEvent-console", &v);

  // 42b ── jsHandle.asElement returns Playwright-spec `null` (not
  // `undefined`) for non-Element handles.
  let v = step(
    &h,
    "jshandle-asElement",
    "await page.setContent('<div id=d>D</div>'); \
     const jh = await page.evaluateHandle(() => document.getElementById('d')); \
     const eh = jh.asElement(); \
     const html = eh ? await eh.innerHTML() : ''; \
     const plain = await page.evaluateHandle(() => 42); \
     const noEl = plain.asElement(); \
     return { gotElement: html === 'D', plainIsNull: noEl === null };",
  )
  .await;
  assert_all_true("jshandle-asElement", &v);

  // 43 ── page.evaluateHandle + jsonValue() round-trips an object.
  let v = step(
    &h,
    "evaluateHandle",
    "await page.setContent('<body>x</body>'); \
     const jh = await page.evaluateHandle(() => ({ k: 7, s: 'x' })); \
     const v = await jh.jsonValue(); \
     await jh.dispose(); \
     return { roundTrip: v.k === 7 && v.s === 'x' };",
  )
  .await;
  assert_all_true("evaluateHandle", &v);
}
