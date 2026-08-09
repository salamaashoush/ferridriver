// Ported from crates/ferridriver-cli/tests/backends_support/
// {handle_surface,script_handles_local}.rs (handle/evaluate half) —
// JSHandle/ElementHandle lifecycle, jsonValue/getProperty/getProperties,
// multi-arg evaluate, $eval/$$eval, scoped $/$$, ownerFrame/contentFrame,
// element-scoped waits, the temp-tag action bridge, selectText, the
// injected UtilityScript surface, rich-type round-trips, waitForFunction,
// and handle materialisation. Test titles mirror the original Rust fn
// names.

import { test, describe, expect } from '@ferridriver/test';
import type { ElementHandle, JSHandle } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

describe('handles', () => {
  test('handle_json_value', async ({ page }) => {
    await page.goto(dataUrl("<button id='primary'>ok</button>"));

    // jsonValue round-trips JSON-expressible values through the utility
    // script's isomorphic serializer.
    const jh = await page.evaluateHandle(() => ({ a: 1, b: 'two', c: [3, 4] }));
    const v = (await jh.jsonValue()) as { a: number; b: string; c: number[] };
    await jh.dispose();
    expect(v.a).toBe(1);
    expect(v.b).toBe('two');
    expect(v.c).toEqual([3, 4]);

    // jsonValue rehydrates rich types (Date, NaN) into native JS —
    // matching Playwright's `parseResult` in
    // `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts:19`.
    const rich = await page.evaluateHandle(() => ({ d: new Date(0), n: NaN }));
    const rv = (await rich.jsonValue()) as { d: Date; n: number };
    await rich.dispose();
    expect(rv.d instanceof Date).toBe(true);
    expect(rv.d.toISOString()).toBe('1970-01-01T00:00:00.000Z');
    expect(Number.isNaN(rv.n)).toBe(true);
  });

  test('handle_properties', async ({ page }) => {
    await page.goto(dataUrl("<button id='primary'>ok</button>"));

    // getProperty on both primitive and object values. Playwright's
    // JSHandle can be backed by either a remote reference (`_objectId`)
    // or an inline primitive (`_value`) — the two shapes round-trip
    // through jsonValue identically.
    const jh = await page.evaluateHandle(() => ({ x: 42, y: 'hi', z: { n: 7 } }));
    const xh = await jh.getProperty('x');
    const yh = await jh.getProperty('y');
    const zh = await jh.getProperty('z');
    expect(await xh.jsonValue()).toBe(42);
    expect(await yh.jsonValue()).toBe('hi');
    expect(await zh.jsonValue()).toEqual({ n: 7 });
    await xh.dispose();
    await yh.dispose();
    await zh.dispose();
    await jh.dispose();

    // getProperties enumerates own enumerable string-keyed props as
    // (key, JSHandle) pairs. Handles of primitive-valued props are
    // value-backed; dispose is a no-op for those.
    const obj = await page.evaluateHandle(() => ({ a: 1, b: 2 }));
    const props = await obj.getProperties();
    expect(Object.keys(props).sort()).toEqual(['a', 'b']);
    expect(await props.a.jsonValue()).toBe(1);
    expect(await props.b.jsonValue()).toBe(2);
    await props.a.dispose();
    await props.b.dispose();
    await obj.dispose();
  });

  test('handle_multi_arg_evaluate', async ({ page }) => {
    await page.goto(dataUrl("<body><button id='primary'>ok</button></body>"));

    // `handle.evaluate(fn, userArg)` passes the handle AND the user arg
    // as two positional parameters — the user function signature is
    // `(target, userArg) => ...`. Mirrors Playwright's
    // `javascript.ts:161-163` `evaluate(ctx, true, fn, this, arg)`.
    const eh = (await page.querySelector('button#primary')) as ElementHandle;
    expect(await eh.evaluate((el: Element, suffix: string) => el.tagName + suffix, '!')).toBe('BUTTON!');
    await eh.dispose();

    // Passing a JSHandle AS the user arg exercises the rich-arg walker
    // (top-level class-instance detection -> `{h: 0}` wire shape).
    const body = (await page.querySelector('body')) as ElementHandle;
    const btn = (await page.querySelector('button#primary')) as ElementHandle;
    expect(await btn.evaluate((el: Element, other: Element) => other.contains(el), body)).toBe(true);
    await btn.dispose();
    await body.dispose();
  });

  test('element_handle_eval', async ({ page }) => {
    await page.goto(dataUrl("<div id='parent'><button class='b'>one</button><button class='b'>two</button></div>"));

    // $eval runs `fn` with the first matched descendant as arg.
    const p = (await page.querySelector('#parent')) as ElementHandle;
    expect(await p.$eval('button.b', (el: Element) => el.textContent)).toBe('one');

    // $$eval runs `fn` with the array of matches as arg.
    expect(await p.$$eval('button.b', (els: Element[]) => els.map((e) => e.textContent).join('|'))).toBe('one|two');

    // $eval on a missing selector errors (Playwright parity).
    let missThrew = false;
    try {
      await p.$eval('button.does-not-exist', (el: Element) => el.textContent);
    } catch {
      missThrew = true;
    }
    expect(missThrew).toBe(true);

    // $$eval with no match returns an empty array — not an error.
    expect(await p.$$eval('button.none', (els: Element[]) => els.length)).toBe(0);
    await p.dispose();
  });

  test('element_handle_query', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<div id='parent'><button class='b'>one</button><span class='b'>two</span><button class='b'>three</button></div><button class='b'>outside</button>",
      ),
    );

    // $ resolves the first descendant inside this element's subtree only
    // — the `outside` button is a sibling of #parent, so it must not be
    // returned even though it matches `.b`.
    const p = (await page.querySelector('#parent')) as ElementHandle;
    const first = (await p.$('.b')) as ElementHandle;
    expect(await first.textContent()).toBe('one');
    await first.dispose();

    // $ on a non-matching selector returns null (Playwright parity;
    // rquickjs maps Option::None to undefined, accept either).
    expect((await p.$('.does-not-exist')) == null).toBe(true);

    // $$ returns every scoped descendant in document order — three `.b`
    // inside #parent, NOT the fourth `.b` sibling outside it. Proves the
    // query is scoped to the handle, not the whole document.
    const els = await p.$$('.b');
    const texts: Array<string | null> = [];
    for (const e of els) {
      texts.push(await e.textContent());
    }
    for (const e of els) {
      await e.dispose();
    }
    expect(els.length).toBe(3);
    expect(texts).toEqual(['one', 'two', 'three']);

    // $$ with no match returns an empty array, not an error.
    expect((await p.$$('.none')).length).toBe(0);
    await p.dispose();
  });

  test('element_handle_frames', async ({ page }) => {
    await page.goto(dataUrl("<button id='b'>ok</button>"));

    // ownerFrame returns the element's containing frame — the main
    // frame for any connected element on the top-level page.
    const b = (await page.querySelector('#b')) as ElementHandle;
    expect((await b.ownerFrame()) != null).toBe(true);

    // contentFrame returns null for a non-iframe element.
    expect((await b.contentFrame()) == null).toBe(true);
    await b.dispose();
  });

  test('element_handle_waits', async ({ page }) => {
    await page.goto(dataUrl("<button id='b'>ok</button>"));

    // waitForElementState('visible'): already-visible returns fast.
    const b = (await page.querySelector('#b')) as ElementHandle;
    await b.waitForElementState('visible', { timeout: 5000 });
    await b.dispose();

    // Element-scoped waitForSelector — polls subtree until non-null.
    await page.goto(dataUrl("<div id='p'><span class='inner'>hi</span></div>"));
    const p = (await page.querySelector('#p')) as ElementHandle;
    const eh = await p.waitForSelector('.inner', { timeout: 2000 });
    expect(eh != null).toBe(true);
    await eh?.dispose();
    await p.dispose();
  });

  test('element_handle_temp_tag_actions', async ({ page }) => {
    // fill
    await page.goto(dataUrl("<input id='i' value=''>"));
    let eh = (await page.querySelector('#i')) as ElementHandle;
    await eh.fill('hello');
    expect(await eh.inputValue()).toBe('hello');
    await eh.dispose();

    // check / uncheck
    await page.goto(dataUrl("<input type='checkbox' id='c'>"));
    eh = (await page.querySelector('#c')) as ElementHandle;
    await eh.check();
    expect(await eh.isChecked()).toBe(true);
    await eh.uncheck();
    expect(await eh.isChecked()).toBe(false);
    await eh.dispose();

    // setChecked
    await page.goto(dataUrl("<input type='checkbox' id='c'>"));
    eh = (await page.querySelector('#c')) as ElementHandle;
    await eh.setChecked(true);
    expect(await eh.isChecked()).toBe(true);
    await eh.dispose();

    // press — target a focused input so the character lands at a
    // predictable spot.
    await page.goto(dataUrl("<input id='i' value=''>"));
    eh = (await page.querySelector('#i')) as ElementHandle;
    await eh.press('a');
    expect(await eh.inputValue()).toBe('a');
    await eh.dispose();

    // dispatchEvent — synthetic click fires the page-side handler.
    await page.goto(dataUrl("<button id='b' onclick=\"document.title='tt'\">b</button>"));
    eh = (await page.querySelector('#b')) as ElementHandle;
    await eh.dispatchEvent('click');
    expect(await page.title()).toBe('tt');
    await eh.dispose();

    // selectOption by value.
    await page.goto(dataUrl("<select id='s'><option value='a'>A</option><option value='b'>B</option></select>"));
    eh = (await page.querySelector('#s')) as ElementHandle;
    expect(await eh.selectOption('b')).toEqual(['b']);
    await eh.dispose();
  });

  test('element_handle_action_options', async ({ page }) => {
    // click({ button: 'right' }) — the mousedown handler records the
    // numeric button; right button is 2. A no-option click would record 0.
    // `return false` on contextmenu: without it the browser performs its
    // default action for a right click, and Firefox paints a real native
    // menu — over the user's desktop, and still there if the run dies
    // while it is open. Every other right-click test suppresses it too.
    await page.goto(
      dataUrl(
        "<button id='b' oncontextmenu='return false'>b</button>" +
          "<script>window.__btn=-1;document.getElementById('b').addEventListener('mousedown',function(e){window.__btn=e.button;});</script>",
      ),
    );
    let eh = (await page.querySelector('#b')) as ElementHandle;
    await eh.click({ button: 'right' });
    expect(await page.evaluate('window.__btn')).toBe(2);
    await eh.dispose();

    // dblclick() — the dblclick handler only fires on a genuine
    // double-click sequence, so a flag flip proves the two clicks landed.
    await page.goto(
      dataUrl(
        "<button id='b'>b</button>" +
          "<script>window.__dbl=false;document.getElementById('b').addEventListener('dblclick',function(){window.__dbl=true;});</script>",
      ),
    );
    eh = (await page.querySelector('#b')) as ElementHandle;
    await eh.dblclick();
    expect(await page.evaluate('window.__dbl')).toBe(true);
    await eh.dispose();

    // hover() — mouseover sets a sentinel that is absent until the
    // pointer moves over the element.
    await page.goto(
      dataUrl(
        "<div id='d' style='width:80px;height:80px'>d</div>" +
          "<script>window.__hov=false;document.getElementById('d').addEventListener('mouseover',function(){window.__hov=true;});</script>",
      ),
    );
    const dh = (await page.querySelector('#d')) as ElementHandle;
    await dh.hover();
    expect(await page.evaluate('window.__hov')).toBe(true);
    await dh.dispose();

    // type(text, { delay }) — every character lands; `delay` only paces
    // the keystrokes and must not drop input. Observe the full value.
    await page.goto(dataUrl("<input id='i' value=''>"));
    eh = (await page.querySelector('#i')) as ElementHandle;
    await eh.focus();
    await eh.type('xyz', { delay: 1 });
    expect(await eh.inputValue()).toBe('xyz');
    await eh.dispose();
  });

  test('element_handle_select_text', async ({ page }) => {
    await page.goto(dataUrl("<input id='i' value='abc'>"));
    const eh = (await page.querySelector('#i')) as ElementHandle;
    await eh.selectText();
    expect(await page.evaluate('document.activeElement && document.activeElement.id')).toBe('i');
    await eh.dispose();
  });

  test('script_utility_script_exposed', async ({ page }) => {
    // The injected `window.__fd` namespace exposes the Playwright
    // `UtilityScript` class and its isomorphic serializer helpers —
    // the load-bearing primitives for `page.evaluate(fn, arg)` and the
    // JSHandle round-trip. Proves the bundle surfaces them on every
    // backend.
    await page.goto(dataUrl("<div id='x'></div>"));

    expect(await page.evaluate('typeof window.__fd.UtilityScript')).toBe('function');
    expect(await page.evaluate('typeof window.__fd.newUtilityScript')).toBe('function');
    expect(await page.evaluate('typeof window.__fd.parseEvaluationResultValue')).toBe('function');
    expect(await page.evaluate('typeof window.__fd.serializeAsCallArgument')).toBe('function');

    // The factory returns a working instance — its `evaluate` and
    // `jsonValue` methods are invokable.
    expect(await page.evaluate('typeof window.__fd.newUtilityScript().evaluate')).toBe('function');
    expect(await page.evaluate('typeof window.__fd.newUtilityScript().jsonValue')).toBe('function');

    // The deserializer round-trips Playwright's wire shapes for rich
    // types — a smoke check that the isomorphic format built on the
    // Rust side is the same one the page's utility script parses.
    const probes: Array<[string, string, unknown]> = [
      ['nan', "Number.isNaN(window.__fd.parseEvaluationResultValue({v: 'NaN'}))", true],
      ['inf', "window.__fd.parseEvaluationResultValue({v: 'Infinity'}) === Infinity", true],
      ['neginf', "window.__fd.parseEvaluationResultValue({v: '-Infinity'}) === -Infinity", true],
      ['negzero', "1 / window.__fd.parseEvaluationResultValue({v: '-0'}) === -Infinity", true],
      ['null', "window.__fd.parseEvaluationResultValue({v: 'null'}) === null", true],
      ['undef', "typeof window.__fd.parseEvaluationResultValue({v: 'undefined'})", 'undefined'],
      ['date', "window.__fd.parseEvaluationResultValue({d: '2024-01-01T00:00:00.000Z'}) instanceof Date", true],
      ['url', "window.__fd.parseEvaluationResultValue({u: 'https://a.test/x'}) instanceof URL", true],
      ['regexp', "window.__fd.parseEvaluationResultValue({r: {p: 'foo', f: 'gi'}}) instanceof RegExp", true],
      ['bigint', "typeof window.__fd.parseEvaluationResultValue({bi: '42'})", 'bigint'],
      ['error', "window.__fd.parseEvaluationResultValue({e: {n: 'TypeError', m: 'oops', s: ''}}) instanceof Error", true],
    ];
    for (const [name, probeExpr, expected] of probes) {
      const got = await page.evaluate(probeExpr);
      expect([name, got]).toEqual([name, expected]);
    }

    // Round-trip: serialize a rich value -> deserialize -> re-serialize
    // and assert the wire shape is stable. Exercises the complete
    // isomorphic format end-to-end inside the page.
    const roundTrip = await page.evaluate(
      "(() => {" +
        "const raw = {d: '2024-06-01T00:00:00.000Z'};" +
        'const dateObj = window.__fd.parseEvaluationResultValue(raw);' +
        'return window.__fd.serializeAsCallArgument(dateObj, (v) => ({fallThrough: v}));' +
        '})()',
    );
    expect(roundTrip).toEqual({ d: '2024-06-01T00:00:00.000Z' });
  });

  test('script_handle_lifecycle', async ({ page }) => {
    await page.goto(dataUrl("<button id='primary'>ok</button><div class='needle'>x</div>"));

    // querySelector returns an ElementHandle with isDisposed=false.
    const h = (await page.querySelector('button#primary')) as ElementHandle;
    expect(h != null).toBe(true);
    expect(h.isDisposed()).toBe(false);

    // $ alias returns a handle too.
    expect((await page.$('div.needle')) != null).toBe(true);

    // Missing selector returns null/undefined (not an error) — rquickjs
    // maps Rust's `Option::None` to `undefined` while Playwright's TS
    // types say `null`; what matters is non-truthy, not which of the two.
    expect((await page.querySelector('button#does-not-exist')) == null).toBe(true);

    // dispose() latches isDisposed and is idempotent.
    expect(h.isDisposed()).toBe(false);
    await h.dispose();
    expect(h.isDisposed()).toBe(true);
    await h.dispose();
    expect(h.isDisposed()).toBe(true);

    // asJSHandle shares the disposed flag with the ElementHandle
    // (shared Arc<AtomicBool> on the Rust side).
    const eh = (await page.querySelector('button#primary')) as ElementHandle;
    const jh = eh.asJSHandle();
    expect(eh.isDisposed()).toBe(false);
    expect(jh.isDisposed()).toBe(false);
    await eh.dispose();
    expect(eh.isDisposed()).toBe(true);
    expect(jh.isDisposed()).toBe(true);

    // JSHandle.asElement is functional — probes `h instanceof Node` and
    // re-wraps the remote as an ElementHandle when true.
    const eh2 = (await page.querySelector('button#primary')) as ElementHandle;
    const jh2 = eh2.asJSHandle();
    expect(jh2.asElement() != null).toBe(true);
    await eh2.dispose();

    // Non-DOM remotes (plain objects, arrays, functions) yield null.
    // BiDi refers to these via `{type: 'handle', handle}` — not the
    // node-only `sharedReference` wire shape — so the evaluate path
    // must emit the correct form when the handle rides through as an
    // argument.
    const plain = await page.evaluateHandle(() => ({ not: 'a dom node' }));
    expect(plain.asElement() == null).toBe(true);
    await plain.dispose();
  });

  test('script_evaluate_fn_and_handle', async ({ page }) => {
    await page.goto(dataUrl("<button id='primary'>ok</button>"));

    // page.evaluate(fn, primitive) — function-call semantics.
    expect(await page.evaluate((x: number) => x + 1, 41)).toBe(42);

    // page.evaluate(fn, object) — JSON round-trip.
    expect(await page.evaluate((o: { a: number; b: number }) => o.a + o.b, { a: 2, b: 3 })).toBe(5);

    // page.evaluate(fn, null) — no-arg function-call with null.
    expect(await page.evaluate(() => 7, null)).toBe(7);

    // String form also accepted (Playwright parity — `String(pageFunction)`).
    expect(await page.evaluate('1 + 1')).toBe(2);

    // page.evaluateHandle — returns a live JSHandle.
    const h = await page.evaluateHandle(() => ({ x: 42 }));
    expect(h.isDisposed()).toBe(false);
    await h.dispose();
    expect(h.isDisposed()).toBe(true);

    // handle.evaluate passes the handle as arg[0].
    const bodyHandle = await page.evaluateHandle(() => document.body);
    expect(await bodyHandle.evaluate((el: Element) => el.tagName)).toBe('BODY');
    await bodyHandle.dispose();

    // ElementHandle.evaluate routes through its JSHandle.
    const eh = (await page.querySelector('button#primary')) as ElementHandle;
    expect(await eh.evaluate((el: Element) => el.tagName)).toBe('BUTTON');
    await eh.dispose();

    // Disposed-handle use raises the Playwright 'disposed' error.
    const eh2 = (await page.querySelector('button#primary')) as ElementHandle;
    const jh2 = eh2.asJSHandle();
    await eh2.dispose();
    let threw = false;
    let msg = '';
    try {
      await jh2.evaluate((el: Element) => el.tagName);
    } catch (e) {
      threw = true;
      msg = String((e as Error).message ?? e);
    }
    expect(threw).toBe(true);
    expect(msg.includes('disposed')).toBe(true);
  });

  test('script_evaluate_rich_types', async ({ page }) => {
    // Rich-type round-trip — Date / RegExp / NaN / Infinity / BigInt /
    // undefined arrive on the JS side as native values, matching
    // Playwright's `parseSerializedValue` at
    // `/tmp/playwright/packages/playwright-core/src/protocol/serializers.ts:19`.
    await page.goto(dataUrl('<div></div>'));

    // Date: rehydrates to `Date` instance.
    const d = (await page.evaluate(() => new Date('2024-06-01T00:00:00.000Z'))) as Date;
    expect(d instanceof Date).toBe(true);
    expect(d.toISOString()).toBe('2024-06-01T00:00:00.000Z');

    // RegExp: rehydrates to `RegExp` instance.
    const r = (await page.evaluate(() => /foo.*bar/gi)) as RegExp;
    expect(r instanceof RegExp).toBe(true);
    expect(r.source).toBe('foo.*bar');
    expect(r.flags).toBe('gi');

    // NaN: rehydrates to literal NaN.
    expect(Number.isNaN(await page.evaluate(() => NaN))).toBe(true);

    // Infinity: literal +Infinity.
    expect((await page.evaluate(() => Infinity)) === Infinity).toBe(true);

    // BigInt: rehydrates to a `bigint`.
    const b = (await page.evaluate(() => 9007199254740993n)) as bigint;
    expect(typeof b).toBe('bigint');
    expect(String(b)).toBe('9007199254740993');

    // undefined: rehydrates to literal undefined (== null, !== null).
    const u = await page.evaluate(() => undefined);
    expect(u === undefined).toBe(true);
    expect(u == null).toBe(true);
  });

  test('script_element_handle_methods', async ({ page }) => {
    await page.goto(dataUrl("<a id='l' href='/x' data-k='v'>hello <b>world</b></a>"));

    // innerHTML / innerText / textContent / getAttribute. BiDi injects a
    // `data-fdref` attribute on DOM elements it references, so the
    // serialised innerHTML is `<b data-fdref="...">` rather than a bare
    // `<b>` — match the substrings that matter.
    const link = (await page.querySelector('a#l')) as ElementHandle;
    const inner = await link.innerHTML();
    expect(inner.includes('<b')).toBe(true);
    expect(inner.includes('world</b>')).toBe(true);
    expect(await link.innerText()).toBe('hello world');
    expect(await link.textContent()).toBe('hello world');
    expect(await link.getAttribute('href')).toBe('/x');
    expect(await link.getAttribute('data-k')).toBe('v');
    await link.dispose();

    // inputValue
    await page.goto(dataUrl("<input id='i' value='hi'>"));
    const input = (await page.querySelector('#i')) as ElementHandle;
    expect(await input.inputValue()).toBe('hi');
    await input.dispose();

    // State predicates
    await page.goto(
      dataUrl("<button id='v'>x</button><button id='d' disabled>x</button><button id='h' style='display:none'>x</button>"),
    );
    const vis = (await page.querySelector('#v')) as ElementHandle;
    const dis = (await page.querySelector('#d')) as ElementHandle;
    const hid = (await page.querySelector('#h')) as ElementHandle;
    expect(await vis.isVisible()).toBe(true);
    expect(await vis.isEnabled()).toBe(true);
    expect(await dis.isDisabled()).toBe(true);
    expect(await hid.isHidden()).toBe(true);
    await vis.dispose();
    await dis.dispose();
    await hid.dispose();

    // isChecked + isEditable
    await page.goto(dataUrl("<input type='checkbox' id='c' checked><input id='i'><input id='r' readonly>"));
    const cb = (await page.querySelector('#c')) as ElementHandle;
    const editable = (await page.querySelector('#i')) as ElementHandle;
    const readonly = (await page.querySelector('#r')) as ElementHandle;
    expect(await cb.isChecked()).toBe(true);
    expect(await editable.isEditable()).toBe(true);
    expect(await readonly.isEditable()).toBe(false);
    await cb.dispose();
    await editable.dispose();
    await readonly.dispose();

    // boundingBox
    await page.goto(dataUrl("<button id='b' style='position:absolute;left:10px;top:20px;width:50px;height:30px'>b</button>"));
    const box = (await page.querySelector('#b')) as ElementHandle;
    const bbox = await box.boundingBox();
    expect(bbox).not.toBeNull();
    expect(bbox!.width).toBeGreaterThan(0);
    expect(bbox!.height).toBeGreaterThan(0);
    await box.dispose();

    // click fires the native handler. The onclick handler is synchronous
    // so the title update is observable on the next page.title round-trip.
    await page.goto(dataUrl("<button id='b' onclick=\"document.title='clicked'\">b</button>"));
    const clicker = (await page.querySelector('#b')) as ElementHandle;
    await clicker.click();
    expect(await page.title()).toBe('clicked');
    await clicker.dispose();

    // focus updates activeElement
    await page.goto(dataUrl("<input id='i'>"));
    const focusable = (await page.querySelector('#i')) as ElementHandle;
    await focusable.focus();
    expect(await page.evaluate('document.activeElement && document.activeElement.id')).toBe('i');
    await focusable.dispose();

    // scrollIntoViewIfNeeded shouldn't throw on an offscreen element
    await page.goto(dataUrl("<div style='height:2000px'></div><button id='b'>b</button>"));
    const offscreen = (await page.querySelector('#b')) as ElementHandle;
    await offscreen.scrollIntoViewIfNeeded();
    await offscreen.dispose();
  });

  test('script_handle_materialisation', async ({ page }) => {
    await page.goto(dataUrl('<ul><li>a</li><li>b</li><li>c</li></ul>'));

    // page.querySelectorAll returns one handle per match in document
    // order. Each handle's lifecycle is independent — disposing one
    // doesn't affect the others.
    const items = await page.querySelectorAll('li');
    const texts: Array<string | null> = [];
    for (const it of items) {
      texts.push(await it.textContent());
    }
    for (const it of items) {
      await it.dispose();
    }
    expect(items.length).toBe(3);
    expect(texts).toEqual(['a', 'b', 'c']);

    // $$ alias
    const aliased = await page.$$('li');
    expect(aliased.length).toBe(3);
    for (const it of aliased) {
      await it.dispose();
    }

    // Empty selector returns empty array (not error).
    expect((await page.querySelectorAll('li.does-not-exist')).length).toBe(0);

    // locator.elementHandle resolves the locator's selector to a
    // single pinned ElementHandle.
    await page.goto(dataUrl("<button id='b'>click</button>"));
    const eh = await page.locator('#b').elementHandle();
    expect(await eh.evaluate((el: Element) => el.tagName)).toBe('BUTTON');
    await eh.dispose();

    // locator.elementHandles returns one handle per match.
    await page.goto(dataUrl("<ul><li class='it'>x</li><li class='it'>y</li></ul>"));
    const ehs = await page.locator('li.it').elementHandles();
    const itemTexts: Array<string | null> = [];
    for (const handle of ehs) {
      itemTexts.push(await handle.textContent());
    }
    for (const handle of ehs) {
      await handle.dispose();
    }
    expect(ehs.length).toBe(2);
    expect(itemTexts).toEqual(['x', 'y']);
  });

  test('script_page_wait_for_function_arg_polling', async ({ page }) => {
    // Playwright: `page.waitForFunction(pageFunction, arg?, options?):
    // Promise<JSHandle>` — function form with an arg, interval polling,
    // and a JSHandle result whose jsonValue is the truthy value.
    await page.goto(dataUrl('<script>setTimeout(function(){window.counter=9;},80)</script>'));
    const handle: JSHandle = await page.waitForFunction(
      (min: number) => ((window as unknown as { counter?: number }).counter || 0) >= min
        && (window as unknown as { counter?: number }).counter,
      5,
      { polling: 20, timeout: 5000 },
    );
    expect(await handle.jsonValue()).toBe(9);
    await handle.dispose();

    // Non-'raf' polling keyword must be rejected with the Playwright
    // message (runtime validation of a deliberately invalid input).
    let badPolling = '';
    try {
      await page.waitForFunction('true', undefined, { polling: 'interval' as unknown as 'raf' });
    } catch (e) {
      badPolling = String((e as Error).message ?? e);
    }
    expect(badPolling.includes('Unknown polling option')).toBe(true);
  });
});
