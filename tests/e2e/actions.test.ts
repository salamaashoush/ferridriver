// Ported from crates/ferridriver-cli/tests/backends_support/
// {script_handles_local,action_options}.rs (action half) — coordinate
// clicks, mouse/drag/wheel primitives, native HTML5 drag-and-drop,
// locator.drop, emulateMedia, per-option click/dblclick/press/type
// surfaces, check/fill semantics, native tap, action timeouts,
// screenshot masking, addInitScript, and keyboard input. Test titles
// mirror the original Rust fn names.

import { test, describe, expect } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

describe('actions', () => {
  test('script_click_at', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<div id='d' onclick=\"this.textContent='clicked'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>click me</div>",
      ),
    );
    await page.clickAt(50, 50);
    expect(await page.textContent('#d')).toBe('clicked');
  });

  test('script_mouse_click_coords', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<div id='d' onclick=\"this.textContent='mouse-clicked'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>click me</div>",
      ),
    );
    await page.mouse.click(40, 40);
    expect(await page.textContent('#d')).toBe('mouse-clicked');
  });

  test('script_drag_coords', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<div id='d' onmousedown=\"this.dataset.down='1'\" onmouseup=\"this.dataset.up='1'\" onmousemove=\"this.dataset.moved='1'\" style='position:fixed;left:0;top:0;width:200px;height:200px'>drag</div>",
      ),
    );
    await page.mouse.down();
    await page.moveMouseSmooth(50, 50, 150, 150, 5);
    await page.mouse.up();
    expect(await page.evaluate("document.getElementById('d').dataset.down")).toBe('1');
    expect(await page.evaluate("document.getElementById('d').dataset.up")).toBe('1');
  });

  test('script_drag_and_drop', async ({ page }) => {
    await page.goto(
      dataUrl(
        "<div id='src' style='width:60px;height:60px;background:#f00' onmousedown=\"this.dataset.d='1'\"></div><div id='tgt' style='width:60px;height:60px;margin-top:80px;background:#0f0' onmouseup=\"this.dataset.u='1'\"></div>",
      ),
    );
    await page.dragAndDrop('#src', '#tgt');
    expect(await page.evaluate("document.getElementById('src').dataset.d || ''")).toBe('1');
  });

  test('script_drag_and_drop_options', async ({ page }) => {
    await page.goto(
      dataUrl(
        '<style>html,body{margin:0;padding:0}</style>' +
          "<div id='src' style='width:80px;height:80px;background:#f00;position:absolute;left:20px;top:20px'></div>" +
          "<div id='tgt' style='width:80px;height:80px;background:#0f0;position:absolute;left:200px;top:200px'></div>" +
          "<div id='out' style='position:fixed;top:0;right:0'>idle</div>" +
          '<script>' +
          "var o=document.getElementById('out');" +
          'var moves=0;' +
          "window.addEventListener('mousedown',function(e){o.dataset.down=JSON.stringify({x:e.clientX,y:e.clientY});},true);" +
          "window.addEventListener('mouseup',function(e){o.dataset.up=JSON.stringify({x:e.clientX,y:e.clientY});},true);" +
          "window.addEventListener('mousemove',function(){moves+=1;o.dataset.moves=String(moves);},true);" +
          "window.addEventListener('pointermove',function(e){" +
          "var c=typeof e.getCoalescedEvents==='function'?e.getCoalescedEvents():[];" +
          'if(c.length>1){moves+=c.length-1;o.dataset.moves=String(moves);}' +
          '},true);' +
          '</script>',
      ),
    );
    await page.dragAndDrop('#src', '#tgt', {
      sourcePosition: { x: 5, y: 5 },
      targetPosition: { x: 10, y: 10 },
      steps: 6,
    });
    const state = (await page.evaluate(() => {
      const o = document.getElementById('out') as HTMLElement;
      return {
        d: o.dataset.down ? (JSON.parse(o.dataset.down) as { x: number; y: number }) : null,
        u: o.dataset.up ? (JSON.parse(o.dataset.up) as { x: number; y: number }) : null,
        m: parseInt(o.dataset.moves || '0', 10),
      };
    })) as { d: { x: number; y: number } | null; u: { x: number; y: number } | null; m: number };
    // mousedown at source padding-box + sourcePosition (~25,25); mouseup
    // at target padding-box + targetPosition (~210,210); steps=6 emits
    // at least 6 mousemove dispatches.
    expect(state.d).not.toBeNull();
    expect(state.u).not.toBeNull();
    expect(state.d!.x).toBeGreaterThanOrEqual(24);
    expect(state.d!.x).toBeLessThanOrEqual(26);
    expect(state.d!.y).toBeGreaterThanOrEqual(24);
    expect(state.d!.y).toBeLessThanOrEqual(26);
    expect(state.u!.x).toBeGreaterThanOrEqual(209);
    expect(state.u!.x).toBeLessThanOrEqual(211);
    expect(state.u!.y).toBeGreaterThanOrEqual(209);
    expect(state.u!.y).toBeLessThanOrEqual(211);
    expect(state.m).toBeGreaterThanOrEqual(6);
  });

  test('script_locator_drag_to_options', async ({ page }) => {
    await page.goto(
      dataUrl(
        '<style>html,body{margin:0;padding:0}</style>' +
          "<div id='src' style='width:80px;height:80px;background:#f00;position:absolute;left:20px;top:20px'></div>" +
          "<div id='tgt' style='width:80px;height:80px;background:#0f0;position:absolute;left:200px;top:200px'></div>" +
          "<div id='out' style='position:fixed;top:0;right:0'></div>" +
          '<script>' +
          "var o=document.getElementById('out');" +
          "window.addEventListener('mouseup',function(e){o.dataset.up=JSON.stringify({x:e.clientX,y:e.clientY});},true);" +
          '</script>',
      ),
    );
    await page.locator('#src').dragTo(page.locator('#tgt'), { targetPosition: { x: 15, y: 15 } });
    const up = (await page.evaluate(() => {
      const raw = (document.getElementById('out') as HTMLElement).dataset.up || '';
      return raw ? (JSON.parse(raw) as { x: number; y: number }) : null;
    })) as { x: number; y: number } | null;
    expect(up).not.toBeNull();
    expect(up!.x).toBeGreaterThanOrEqual(214);
    expect(up!.x).toBeLessThanOrEqual(216);
    expect(up!.y).toBeGreaterThanOrEqual(214);
    expect(up!.y).toBeLessThanOrEqual(216);
  });

  test('script_drag_buttons_held', async ({ page }) => {
    // dragTo must hold the left button DOWN across the move (Playwright:
    // move(source) -> down -> move(target,{steps}) -> up). Drag libraries
    // (interact.js, dnd-kit, native HTML5 DnD) gate the drag on a
    // mousemove where `event.buttons` reflects the held button; CDP
    // previously emitted the drag moves without the `buttons` bitmask, so
    // no drag ever started. Observes a mousemove with buttons===1 firing
    // between mousedown and mouseup — an effect ONLY present when the
    // held-button state is wired.
    await page.goto(
      dataUrl(
        '<style>html,body{margin:0;padding:0}</style>' +
          "<div id='src' style='width:80px;height:80px;background:#f00;position:absolute;left:20px;top:20px'></div>" +
          "<div id='tgt' style='width:80px;height:80px;background:#0f0;position:absolute;left:200px;top:200px'></div>" +
          "<div id='out'></div>" +
          '<script>' +
          'var down=false, moveWithButton=false;' +
          "window.addEventListener('mousedown',function(){down=true;},true);" +
          "window.addEventListener('mousemove',function(e){ if(down && e.buttons===1){moveWithButton=true;} },true);" +
          "window.addEventListener('mouseup',function(){ document.getElementById('out').dataset.r=JSON.stringify({moveWithButton:moveWithButton}); },true);" +
          '</script>',
      ),
    );
    await page.locator('#src').dragTo(page.locator('#tgt'), { steps: 4 });
    const result = (await page.evaluate(() => {
      const raw = (document.getElementById('out') as HTMLElement).dataset.r || '';
      return raw ? (JSON.parse(raw) as { moveWithButton: boolean }) : null;
    })) as { moveWithButton: boolean } | null;
    expect(result).not.toBeNull();
    expect(result!.moveWithButton).toBe(true);
  });

  test('script_drag_default_steps', async ({ page }) => {
    // A default dragTo (no options) must emit intermediate mousemoves
    // between press and release — ferridriver defaults `steps` to 5.
    // Drag libraries track the pointer across several moves to cross
    // their drag threshold; a single source->target jump never starts
    // the drag. Playwright defaults to one move but compensates with
    // native drag interception, which only helps native HTML5 DnD —
    // stepped moves are the fix for mousemove-tracked libraries.
    await page.goto(
      dataUrl(
        '<style>html,body{margin:0;padding:0}</style>' +
          "<div id='src' style='width:60px;height:60px;background:#f00;position:absolute;left:20px;top:20px'></div>" +
          "<div id='tgt' style='width:60px;height:60px;background:#0f0;position:absolute;left:300px;top:300px'></div>" +
          '<script>' +
          'var down=false, movesWhileDown=0, allHeld=true;' +
          "window.addEventListener('mousedown',function(){down=true;},true);" +
          "window.addEventListener('mouseup',function(){down=false;},true);" +
          "window.addEventListener('mousemove',function(e){ if(down){movesWhileDown++; if(e.buttons!==1){allHeld=false;}} },true);" +
          '</script>',
      ),
    );
    await page.locator('#src').dragTo(page.locator('#tgt'));
    expect((await page.evaluate('movesWhileDown')) as number).toBeGreaterThanOrEqual(5);
    expect(await page.evaluate('allHeld')).toBe(true);
  });

  test('script_drag_native_html5', async ({ page, browserName }) => {
    // Native HTML5 drag-and-drop end-to-end: a `draggable` source that
    // stashes a DataTransfer payload and a target that accepts the drop.
    // On CDP this exercises the `Input.setInterceptDrags` +
    // `Input.dispatchDragEvent` port of Playwright's `crDragDrop.ts` —
    // plain synthetic mouse events cannot drive a Chromium native drag
    // and used to wedge the input queue for the rest of the page's life,
    // so the post-drag click assertion is part of the contract. WebKit's
    // Playwright build runs the native drag from the dispatched mouse
    // events directly. Firefox over BiDi starts the drag (dragstart/
    // dragenter fire) but its remote input stack never delivers the
    // final drop — the same hole as Playwright's own BiDi backend, which
    // has no drag handling at all — so Firefox asserts the achievable
    // subset (drag started + input pipeline alive).
    await page.goto(
      dataUrl(
        '<style>html,body{margin:0;padding:0}</style>' +
          "<div id='src' draggable='true' style='width:60px;height:60px;background:#f00;position:absolute;left:20px;top:20px'></div>" +
          "<div id='dst' style='width:120px;height:120px;background:#00f;position:absolute;left:300px;top:300px'></div>" +
          '<script>' +
          'window.log=[];' +
          "var src=document.getElementById('src'), dst=document.getElementById('dst');" +
          "src.addEventListener('dragstart',function(e){e.dataTransfer.setData('text/plain','payload-123');window.log.push('dragstart');});" +
          "dst.addEventListener('dragenter',function(e){e.preventDefault();if(window.log.indexOf('dragenter')<0){window.log.push('dragenter');}});" +
          "dst.addEventListener('dragover',function(e){e.preventDefault();if(window.log.indexOf('dragover')<0){window.log.push('dragover');}});" +
          "dst.addEventListener('drop',function(e){e.preventDefault();window.log.push('drop:'+e.dataTransfer.getData('text/plain'));});" +
          '</script>',
      ),
    );
    await page.locator('#src').dragTo(page.locator('#dst'));
    const log = (await page.evaluate('window.log')) as string[];
    await page.evaluate(
      "window.clicked=false; document.getElementById('dst').addEventListener('click',function(){window.clicked=true;})",
    );
    await page.locator('#dst').click();
    expect(log).toContain('dragstart');
    expect(log).toContain('dragenter');
    // Input pipeline must stay alive after the native drag (no wedged
    // drag session).
    expect(await page.evaluate('window.clicked')).toBe(true);
    if (browserName !== 'firefox') {
      expect(log).toContain('drop:payload-123');
    }
  });

  test('script_locator_drop_payload', async ({ page }) => {
    // A drop zone whose dragover calls preventDefault (accepts the drop)
    // and whose drop handler records the DataTransfer's text payload
    // plus the dropped file name/bytes back onto a dataset attribute.
    // The data ONLY appears when the payload reached the page-side drop
    // handler.
    await page.goto(
      dataUrl(
        '<style>html,body{margin:0;padding:0}</style>' +
          "<div id='zone' style='width:200px;height:200px;background:#eee'></div>" +
          '<script>' +
          "var z=document.getElementById('zone');" +
          "z.addEventListener('dragover',function(e){e.preventDefault();});" +
          "z.addEventListener('drop',function(e){" +
          'e.preventDefault();' +
          'var dt=e.dataTransfer;' +
          "var text=dt.getData('text/plain');" +
          'var f=dt.files[0];' +
          'var r=new FileReader();' +
          'r.onload=function(){' +
          "z.dataset.result=JSON.stringify({text:text,name:f?f.name:'',body:r.result});" +
          '};' +
          "if(f){r.readAsText(f);}else{z.dataset.result=JSON.stringify({text:text,name:'',body:''});}" +
          '});' +
          '</script>',
      ),
    );
    await page.locator('#zone').drop({
      files: { name: 'card.txt', mimeType: 'text/plain', buffer: new TextEncoder().encode('pw-bytes') },
      data: { 'text/plain': 'hello-drop' },
    });
    await page.waitForFunction("document.getElementById('zone').dataset.result !== undefined");
    const result = (await page.evaluate(
      "JSON.parse(document.getElementById('zone').dataset.result)",
    )) as { text: string; name: string; body: string };
    expect(result.text).toBe('hello-drop');
    expect(result.name).toBe('card.txt');
    expect(result.body).toBe('pw-bytes');
  });

  test('script_locator_drop_rejected', async ({ page }) => {
    // Drop zone with NO dragover preventDefault -> the drop is rejected;
    // `Locator.drop` must surface an error, not resolve silently.
    await page.goto(dataUrl("<div id='zone' style='width:200px;height:200px;background:#eee'></div>"));
    let msg = 'no-error';
    try {
      await page.locator('#zone').drop({ data: { 'text/plain': 'x' } });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    expect(msg.includes('did not accept the drop')).toBe(true);
  });

  test('script_emulate_media_all_fields', async ({ page, browserName }) => {
    await page.goto(dataUrl("<div id='x'></div>"));
    if (browserName === 'firefox') {
      // Firefox's BiDi implementation has no working command for any of
      // the five media knobs (Playwright's own BiDi updateEmulateMedia
      // is an empty stub; emulation.setForcedColorsModeThemeOverride is
      // rejected by shipping Firefox). Each field surfaces a typed
      // Unsupported instead of silently pretending the override worked.
      for (const bag of [
        { media: 'print' as const },
        { colorScheme: 'dark' as const },
        { reducedMotion: 'reduce' as const },
        { forcedColors: 'active' as const },
        { contrast: 'more' as const },
      ]) {
        let msg = 'no-throw';
        try {
          await page.emulateMedia(bag);
        } catch (e) {
          msg = String((e as Error).message ?? e);
        }
        expect(msg.toLowerCase().includes('does not support')).toBe(true);
      }
      return;
    }
    await page.emulateMedia({
      media: 'print',
      colorScheme: 'dark',
      reducedMotion: 'reduce',
      forcedColors: 'active',
      contrast: 'more',
    });
    const result = (await page.evaluate(() => ({
      print: matchMedia('print').matches,
      screen: matchMedia('screen').matches,
      dark: matchMedia('(prefers-color-scheme: dark)').matches,
      reduced: matchMedia('(prefers-reduced-motion: reduce)').matches,
      forced: matchMedia('(forced-colors: active)').matches,
      contrast: matchMedia('(prefers-contrast: more)').matches,
    }))) as { print: boolean; screen: boolean; dark: boolean; reduced: boolean; forced: boolean; contrast: boolean };
    expect(result.print).toBe(true);
    expect(result.screen).toBe(false);
    // WebKit's print rendering FORCES `prefers-color-scheme: light`
    // while a print media override is active — it does not fall back to
    // the host appearance, which is what this assertion used to claim
    // and why it only ever passed on a light host. Measured on a dark
    // host: `{colorScheme:'dark'}` alone reports dark,
    // `{media:'print', colorScheme:'dark'}` reports light, and switching
    // back to `screen` reports dark again. Chromium honours the override
    // outright. Engine semantics, not a driver gap — Playwright's own
    // page-emulate-media spec never asserts the print+dark combination.
    expect(result.dark).toBe(browserName !== 'webkit');
    expect(result.reduced).toBe(true);
    expect(result.forced).toBe(true);
    expect(result.contrast).toBe(true);
  });

  test('script_emulate_media_null_disables_single_field', async ({ page, browserName }) => {
    await page.goto(dataUrl('<html><body>init</body></html>'));
    if (browserName === 'firefox') {
      // See script_emulate_media_all_fields — every knob is typed
      // Unsupported on Firefox/BiDi, including the null-reset form.
      let msg = 'no-throw';
      try {
        await page.emulateMedia({ colorScheme: null });
      } catch (e) {
        msg = String((e as Error).message ?? e);
      }
      expect(msg.toLowerCase().includes('does not support')).toBe(true);
      return;
    }
    // Removal restores the SYSTEM preference, which is whatever the host
    // runs (WebKit reads the real macOS appearance; headless Chromium
    // defaults to light). Capture the baseline so the assertion is
    // environment-independent: override to the opposite of the baseline,
    // remove, and expect the baseline back.
    const baselineDark = (await page.evaluate("matchMedia('(prefers-color-scheme: dark)').matches")) as boolean;
    const override = baselineDark ? ('light' as const) : ('dark' as const);
    await page.emulateMedia({ colorScheme: override, reducedMotion: 'reduce' });
    expect(await page.evaluate("matchMedia('(prefers-color-scheme: dark)').matches")).toBe(!baselineDark);
    await page.emulateMedia({ colorScheme: null });
    expect(await page.evaluate("matchMedia('(prefers-color-scheme: dark)').matches")).toBe(baselineDark);
    // A sibling reset must not disturb the surviving override.
    expect(await page.evaluate("matchMedia('(prefers-reduced-motion: reduce)').matches")).toBe(true);
  });

  test('script_drag_and_drop_trial', async ({ page }) => {
    await page.goto(
      dataUrl(
        '<style>html,body{margin:0;padding:0}</style>' +
          "<div id='src' style='width:60px;height:60px;background:#f00;position:absolute;left:20px;top:20px'></div>" +
          "<div id='tgt' style='width:60px;height:60px;background:#0f0;position:absolute;left:200px;top:200px'></div>" +
          "<div id='log' data-fired='0'></div>" +
          '<script>' +
          "window.addEventListener('mousedown',function(){document.getElementById('log').dataset.fired='1';},true);" +
          '</script>',
      ),
    );
    await page.dragAndDrop('#src', '#tgt', { trial: true });
    expect(await page.evaluate("document.getElementById('log').dataset.fired")).toBe('0');
  });

  test('script_mouse_wheel', async ({ page }) => {
    // Verify the binding dispatches the wheel event without error.
    // Whether the event produces a visible scroll depends on the
    // engine's input routing with the current mouse position.
    await page.goto(dataUrl("<body style='height:3000px'></body>"));
    await page.mouse.wheel(0, 400);
  });

  test('script_click_options', async ({ page }) => {
    // Full `ClickOptions` surface — button, modifiers, delay, position,
    // clickCount, trial, and the error paths for unknown button /
    // modifier strings. Every sub-assertion is a distinct DOM probe so
    // per-option failures point at the exact wire bug.

    // button:'right' -> contextmenu fires.
    await page.goto(
      dataUrl(
        "<button id='b' oncontextmenu=\"document.getElementById('out').textContent='right';return false\">b</button><div id='out'>n</div>",
      ),
    );
    await page.locator('#b').click({ button: 'right' });
    expect(await page.evaluate("document.getElementById('out').textContent")).toBe('right');

    // clickCount:2 -> dblclick handler fires.
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='out'>n</div>" +
          "<script>document.getElementById('b').addEventListener('dblclick',()=>document.getElementById('out').textContent='dbl')</script>",
      ),
    );
    await page.locator('#b').click({ clickCount: 2 });
    expect(await page.evaluate("document.getElementById('out').textContent")).toBe('dbl');

    // modifiers:['Shift'] -> click event has shiftKey === true.
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='out'>n</div>" +
          "<script>document.getElementById('b').addEventListener('click',e=>document.getElementById('out').textContent=e.shiftKey?'shift':'none')</script>",
      ),
    );
    await page.locator('#b').click({ modifiers: ['Shift'] });
    expect(await page.evaluate("document.getElementById('out').textContent")).toBe('shift');

    // position:{x:10,y:20} -> event coords land at padding-box offset.
    await page.goto(
      dataUrl(
        "<div id='b' style='width:200px;height:100px;background:#ccc'></div><div id='out'>n</div>" +
          "<script>document.getElementById('b').addEventListener('click',e=>{var r=e.currentTarget.getBoundingClientRect();document.getElementById('out').textContent=(Math.round(e.clientX-r.left))+','+(Math.round(e.clientY-r.top))})</script>",
      ),
    );
    await page.locator('#b').click({ position: { x: 10, y: 20 } });
    expect(await page.evaluate("document.getElementById('out').textContent")).toBe('10,20');

    // delay:120 -> mousedown->mouseup gap is honored (allow slack for
    // timer resolution; demand >= 80ms so flaky schedulers still pass).
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='out'>0</div>" +
          '<script>' +
          'let down=0;' +
          "const b=document.getElementById('b');" +
          "b.addEventListener('mousedown',()=>{down=Date.now()});" +
          "b.addEventListener('mouseup',()=>{document.getElementById('out').textContent=String(Date.now()-down)});" +
          '</script>',
      ),
    );
    await page.locator('#b').click({ delay: 120 });
    const heldMs = parseInt((await page.evaluate("document.getElementById('out').textContent")) as string, 10);
    expect(heldMs).toBeGreaterThanOrEqual(80);

    // trial:true -> click handler doesn't fire, but modifier keydown does.
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='clicked'>no</div><div id='kd'>none</div>" +
          '<script>' +
          "document.getElementById('b').addEventListener('click',()=>document.getElementById('clicked').textContent='yes');" +
          "document.addEventListener('keydown',e=>{if(e.key==='Shift')document.getElementById('kd').textContent='shift'});" +
          '</script>',
      ),
    );
    await page.locator('#b').click({ trial: true, modifiers: ['Shift'] });
    expect(await page.evaluate("document.getElementById('clicked').textContent")).toBe('no');
    expect(await page.evaluate("document.getElementById('kd').textContent")).toBe('shift');

    // Bad button string -> typed error, not silent default.
    let msg = 'no-throw';
    try {
      await page.locator('#b').click({ button: 'garbage' as unknown as 'left' });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    expect(msg.includes('Unknown mouse button')).toBe(true);

    // Bad modifier string -> typed error.
    msg = 'no-throw';
    try {
      await page.locator('#b').click({ modifiers: ['Hyper' as unknown as 'Shift'] });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    expect(msg.includes('Unknown modifier')).toBe(true);
  });

  test('script_dispatch_event_timeout', async ({ page }) => {
    // `locator.dispatchEvent` honors `opts.timeout` via the retry loop.
    // Playwright's dispatchEvent does NOT run actionability — it's a
    // programmatic event dispatch, polled only for element presence.
    await page.goto(dataUrl("<button id='b'>b</button>"));
    const t0 = Date.now();
    let msg = 'no-throw';
    try {
      await page.locator('#nope').dispatchEvent('click', {}, { timeout: 200 });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    const elapsed = Date.now() - t0;
    expect(msg.includes('Timeout')).toBe(true);
    expect(msg.includes('200ms')).toBe(true);
    expect(elapsed).toBeLessThan(1500);
  });

  test('script_select_option_force', async ({ page }) => {
    // `selectOption` honors `opts.timeout` (via retry_resolve) AND
    // `opts.force` (skips the ['visible','enabled'] pre-check that would
    // otherwise return error:notenabled).
    await page.goto(dataUrl("<select id='s' disabled><option value='a'>A</option><option value='b'>B</option></select>"));
    const t0 = Date.now();
    let msg = 'no-throw';
    try {
      await page.locator('#s').selectOption('b', { timeout: 200 });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    const elapsed = Date.now() - t0;
    expect(msg.includes('Timeout')).toBe(true);
    expect(msg.includes('200ms')).toBe(true);
    expect(elapsed).toBeLessThan(1500);
    // Value unchanged.
    expect(await page.evaluate("document.getElementById('s').value")).toBe('a');

    // force: true bypasses the pre-check and selects even when disabled.
    await page.goto(dataUrl("<select id='s' disabled><option value='a'>A</option><option value='b'>B</option></select>"));
    await page.locator('#s').selectOption('b', { force: true });
    expect(await page.evaluate("document.getElementById('s').value")).toBe('b');
  });

  test('script_check_behavior', async ({ page }) => {
    // `check`/`uncheck` verify the final state matches the target AND
    // reject uncheck-of-radio, matching Playwright's
    // `server/dom.ts::_setChecked`.

    // 1. Plain checkbox: check() toggles to checked.
    await page.goto(dataUrl("<input id='cb' type='checkbox'>"));
    await page.locator('#cb').check();
    expect(await page.evaluate("document.getElementById('cb').checked")).toBe(true);

    // 2. Checkbox that intercepts the click -> state does not change ->
    //    check() throws the Playwright-exact "did not change its state".
    await page.goto(dataUrl("<input id='cb' type='checkbox' onclick='event.preventDefault()'>"));
    let msg = 'no-throw';
    try {
      await page.locator('#cb').check({ timeout: 500 });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    expect(msg.includes('did not change its state')).toBe(true);

    // 3. Uncheck a checked radio -> typed Playwright radio-group error.
    await page.goto(dataUrl("<input id='r' type='radio' name='g' checked><input type='radio' name='g'>"));
    msg = 'no-throw';
    try {
      await page.locator('#r').uncheck();
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    expect(msg.includes('Cannot uncheck radio button')).toBe(true);

    // 4. trial: true skips the post-click verification AND the click.
    await page.goto(dataUrl("<input id='cb' type='checkbox' onclick='event.preventDefault()'>"));
    await page.locator('#cb').check({ trial: true });
    expect(await page.evaluate("document.getElementById('cb').checked")).toBe(false);

    // 5. check() on an already-checked checkbox is a no-op (no click, no
    //    verification error). Prove by attaching an onclick listener and
    //    asserting it never fires.
    await page.goto(
      dataUrl(
        "<input id='cb' type='checkbox' checked>" +
          "<div id='count'>0</div>" +
          '<script>' +
          "document.getElementById('cb').addEventListener('click', () => {" +
          "const el = document.getElementById('count');" +
          'el.textContent = String(parseInt(el.textContent, 10) + 1);' +
          '});' +
          '</script>',
      ),
    );
    await page.locator('#cb').check();
    expect(await page.evaluate("document.getElementById('count').textContent")).toBe('0');
  });

  test('script_fill_force', async ({ page }) => {
    // `fill.force` bypasses Playwright's ['visible','enabled','editable']
    // pre-check: without force a readonly input polls error:noteditable
    // until timeout; with force the JS `.value = 'x'` assignment goes
    // through regardless of the readonly attribute.
    await page.goto(dataUrl("<input id='ro' readonly value=''><div id='out'></div>"));
    const t0 = Date.now();
    let msg = 'no-throw';
    try {
      await page.locator('#ro').fill('hello', { timeout: 250 });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    const elapsed = Date.now() - t0;
    expect(msg.includes('Timeout')).toBe(true);
    expect(elapsed).toBeLessThan(1500);
    // Value stays empty — confirms no write happened.
    expect(await page.evaluate("document.getElementById('ro').value")).toBe('');

    // force: true on the same readonly input -> writes successfully.
    await page.goto(dataUrl("<input id='ro' readonly value=''>"));
    await page.locator('#ro').fill('bypass', { force: true });
    expect(await page.evaluate("document.getElementById('ro').value")).toBe('bypass');
  });

  test('script_tap_native', async ({ page, browserName }) => {
    // `locator.tap` uses the backend's native touch primitive on every
    // backend: CDP `Input.dispatchTouchEvent`, WebKit
    // `Input.dispatchTapEvent`, BiDi `input.performActions` with a touch
    // pointer source. All emit a trusted touchstart + touchend pair.
    await page.goto(
      dataUrl(
        "<button id='b' style='width:100px;height:50px'>b</button>" +
          "<div id='trusted'>n</div><div id='inrect'>n</div>" +
          '<script>' +
          "const b = document.getElementById('b');" +
          "b.addEventListener('touchstart', e => {" +
          'const t = e.changedTouches[0];' +
          'const r = b.getBoundingClientRect();' +
          "document.getElementById('trusted').textContent = String(e.isTrusted);" +
          "document.getElementById('inrect').textContent = String(" +
          't.clientX >= r.left && t.clientX <= r.right && t.clientY >= r.top && t.clientY <= r.bottom' +
          ');' +
          '}, { passive: true });' +
          '</script>',
      ),
    );
    await page.locator('#b').tap();
    expect(await page.evaluate("document.getElementById('trusted').textContent")).toBe('true');
    expect(await page.evaluate("document.getElementById('inrect').textContent")).toBe('true');

    // Modifiers propagate to the touch event: tap + Shift ->
    // event.shiftKey. Firefox never applies key-source modifier state to
    // touch-pointer events (the BiDi wire has no per-action modifier
    // field), so on Firefox tap-with-modifiers surfaces typed
    // Unsupported instead of silently dropping the modifiers.
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='out'>no</div>" +
          '<script>' +
          "document.getElementById('b').addEventListener('touchstart', e => {" +
          "document.getElementById('out').textContent = e.shiftKey ? 'shift' : 'none';" +
          '}, { passive: true });' +
          '</script>',
      ),
    );
    if (browserName === 'firefox') {
      let msg = 'no-throw';
      try {
        await page.locator('#b').tap({ modifiers: ['Shift'], timeout: 2000 });
      } catch (e) {
        msg = String((e as Error).message ?? e);
      }
      expect(msg.toLowerCase().includes('unsupported')).toBe(true);
      expect(msg.includes('modifiers')).toBe(true);
      expect(await page.evaluate("document.getElementById('out').textContent")).toBe('no');
    } else {
      await page.locator('#b').tap({ modifiers: ['Shift'] });
      expect(await page.evaluate("document.getElementById('out').textContent")).toBe('shift');
    }

    // trial:true skips the touch dispatch but still presses modifiers.
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='tap'>no</div><div id='kd'>no</div>" +
          '<script>' +
          "document.getElementById('b').addEventListener('touchstart', () => { document.getElementById('tap').textContent = 'yes'; }, { passive: true });" +
          "document.addEventListener('keydown', e => { if (e.key === 'Shift') document.getElementById('kd').textContent = 'shift'; });" +
          '</script>',
      ),
    );
    await page.locator('#b').tap({ trial: true, modifiers: ['Shift'] });
    expect(await page.evaluate("document.getElementById('tap').textContent")).toBe('no');
    expect(await page.evaluate("document.getElementById('kd').textContent")).toBe('shift');
  });

  test('script_action_timeout', async ({ page }) => {
    // `opts.timeout` honors the user's deadline on every action method.
    // Each action is called on a selector that doesn't exist with
    // timeout:200; the call must throw a TimeoutError within ~1.5s (wall
    // clock) instead of waiting out the page default. Proves the
    // deadline threaded through retry_resolve! actually fires.
    await page.goto(dataUrl("<button id='b'>b</button>"));
    const actions: Array<[string, () => Promise<unknown>]> = [
      ['click', () => page.locator('#nope').click({ timeout: 200 })],
      ['fill', () => page.locator('#nope').fill('x', { timeout: 200 })],
      ['hover', () => page.locator('#nope').hover({ timeout: 200 })],
      ['tap', () => page.locator('#nope').tap({ timeout: 200 })],
      ['press', () => page.locator('#nope').press('A', { timeout: 200 })],
      ['type', () => page.locator('#nope').type('x', { timeout: 200 })],
      ['dblclick', () => page.locator('#nope').dblclick({ timeout: 200 })],
      ['check', () => page.locator('#nope').check({ timeout: 200 })],
      ['uncheck', () => page.locator('#nope').uncheck({ timeout: 200 })],
    ];
    for (const [name, action] of actions) {
      const t0 = Date.now();
      let msg = 'no-throw';
      try {
        await action();
      } catch (e) {
        msg = String((e as Error).message ?? e);
      }
      const elapsed = Date.now() - t0;
      expect([name, msg.includes('Timeout') && msg.includes('200ms')]).toEqual([name, true]);
      expect([name, elapsed < 1500]).toEqual([name, true]);
    }
  });

  test('script_screenshot_mask_locator', async ({ page }) => {
    // `page.screenshot({ mask })` takes Locator[], not selector strings
    // (which would leak the internal wire shape). Masking a green box
    // with a custom magenta color overpaints those pixels, so the PNG
    // bytes differ from an unmasked capture; masking a Locator that
    // matches nothing leaves the capture byte-identical. Checksum the
    // byte array (no zlib for full PNG decode in the sandbox) and
    // compare the three captures.
    await page.goto(
      dataUrl(
        '<style>html,body{margin:0;padding:0;background:#fff}' +
          '#box{position:fixed;left:0;top:0;width:100px;height:100px;background:#00ff00}</style>' +
          "<div id='box'></div>",
      ),
    );
    const sum = (bytes: Uint8Array): number => {
      let s = 0;
      for (let i = 0; i < bytes.length; i++) {
        s = (s + bytes[i] * ((i % 7) + 1)) >>> 0;
      }
      return s;
    };
    const plain = await page.screenshot({ type: 'png', scale: 'css' });
    const masked = await page.screenshot({
      type: 'png',
      scale: 'css',
      mask: [page.locator('#box')],
      maskColor: '#ff00ff',
    });
    const empty = await page.screenshot({
      type: 'png',
      scale: 'css',
      mask: [page.locator('#does-not-exist')],
      maskColor: '#ff00ff',
    });
    expect(plain.length).toBeGreaterThan(0);
    expect(sum(plain)).not.toBe(sum(masked));
    expect(sum(plain)).toBe(sum(empty));
  });

  test('script_screenshot_scale', async ({ page, browser, browserName }) => {
    // `scale: 'css'` means one image pixel per CSS pixel even on a
    // 2x context. CDP carries the factor on the capture CLIP, so a
    // viewport shot -- which sends no clip of its own -- dropped the
    // option entirely and always came back at device pixels.
    const context = await browser.newContext({ viewport: { width: 200, height: 100 }, deviceScaleFactor: 2 });
    try {
      const p = await context.newPage();
      await p.setContent('<style>html,body{margin:0;background:#334455}</style>');
      // PNG stores width big-endian at byte 16.
      const width = (bytes: Uint8Array): number =>
        (bytes[16] << 24) | (bytes[17] << 16) | (bytes[18] << 8) | bytes[19];

      const device = await p.screenshot({ type: 'png', scale: 'device' });
      const css = await p.screenshot({ type: 'png', scale: 'css' });

      expect(width(device)).toBe(400);
      if (browserName === 'firefox') {
        // `browsingContext.captureScreenshot` has no scale parameter and
        // Playwright's BiDi backend drops the argument the same way
        // (`bidi/bidiPage.ts::takeScreenshot`).
        expect(width(css)).toBe(400);
      } else {
        expect(width(css)).toBe(200);
      }
    } finally {
      await context.close();
    }
  });

  test('script_add_init_script', async ({ page }) => {
    // `page.addInitScript(script, arg)` — the full Playwright surface
    // (Function + arg, string, `{ content }`), including the
    // Rust-core-driven `Cannot evaluate a string with arguments` error
    // for the string+arg form. Every assertion fires after a goto so the
    // init script really did run at document start.

    // Function + typed arg -> init script runs before page JS with arg.
    await page.addInitScript(
      (cfg: { answer: number; label: string }) => {
        (window as unknown as { __fd_init_arg: unknown }).__fd_init_arg = cfg;
      },
      { answer: 42, label: 'hi' },
    );
    await page.goto(dataUrl('<title>x</title>'));
    expect(await page.evaluate('window.__fd_init_arg.answer')).toBe(42);
    expect(await page.evaluate('window.__fd_init_arg.label')).toBe('hi');

    // Function with no arg -> rendered as `(fn)(undefined)`.
    await page.addInitScript((x: unknown) => {
      (window as unknown as { __fd_init_noarg: string }).__fd_init_noarg = typeof x;
    });
    await page.goto(dataUrl('<title>y</title>'));
    expect(await page.evaluate('window.__fd_init_noarg')).toBe('undefined');

    // Function with explicit null -> arg is null.
    await page.addInitScript((x: unknown) => {
      (window as unknown as { __fd_init_null: string }).__fd_init_null = x === null ? 'is-null' : typeof x;
    }, null);
    await page.goto(dataUrl('<title>z</title>'));
    expect(await page.evaluate('window.__fd_init_null')).toBe('is-null');

    // { content } -> used verbatim.
    await page.addInitScript({ content: "window.__fd_init_content = 'from-content';" });
    await page.goto(dataUrl('<title>w</title>'));
    expect(await page.evaluate('window.__fd_init_content')).toBe('from-content');

    // String + arg -> Rust core rejects with Playwright's exact message.
    let msg = 'no-throw';
    try {
      await page.addInitScript('window.x = 1', { bad: true });
    } catch (e) {
      msg = String((e as Error).message ?? e);
    }
    expect(msg.includes('Cannot evaluate a string with arguments')).toBe(true);
  });

  test('script_keyboard_press', async ({ page }) => {
    await page.goto(dataUrl("<textarea id='t'></textarea>"));
    await page.locator('#t').focus();
    await page.keyboard.press('A');
    await page.keyboard.press('B');
    expect((await page.inputValue('#t')).length).toBeGreaterThan(0);
  });

  test('script_keyboard_type_named_keys', async ({ page }) => {
    // namedKeys=true: `{Enter}` presses Enter, producing a newline in
    // the textarea. Without the option the literal text `{Enter}` would
    // be typed, so the resulting value distinguishes the two paths.
    await page.goto(dataUrl("<textarea id='t'></textarea>"));
    await page.locator('#t').focus();
    await page.keyboard.type('Hello{Enter}World', { namedKeys: true });
    expect(await page.inputValue('#t')).toBe('Hello\nWorld');

    // namedKeys=false (default): `{Enter}` is typed verbatim.
    await page.goto(dataUrl("<textarea id='t2'></textarea>"));
    await page.locator('#t2').focus();
    await page.keyboard.type('A{Enter}B');
    expect(await page.inputValue('#t2')).toBe('A{Enter}B');

    // Escaped `{{` types a literal `{`.
    await page.goto(dataUrl("<textarea id='t3'></textarea>"));
    await page.locator('#t3').focus();
    await page.keyboard.type('a{{b', { namedKeys: true });
    expect(await page.inputValue('#t3')).toBe('a{b');

    // `{Backspace}` is a real key press that edits the value.
    await page.goto(dataUrl("<textarea id='t4'></textarea>"));
    await page.locator('#t4').focus();
    await page.keyboard.type('abc{Backspace}d', { namedKeys: true });
    expect(await page.inputValue('#t4')).toBe('abd');

    // `{Control+a}` dispatches a keydown carrying the Ctrl modifier.
    await page.goto(dataUrl("<input id='t5'>"));
    await page.locator('#t5').focus();
    await page.evaluate(
      "(() => { window.__ctrlA = ''; document.getElementById('t5').addEventListener('keydown', e => { if (e.key === 'a') window.__ctrlA += 'ctrl=' + e.ctrlKey + ';'; }); })()",
    );
    await page.keyboard.type('{Control+a}', { namedKeys: true });
    expect(((await page.evaluate('window.__ctrlA')) as string).includes('ctrl=true')).toBe(true);
  });

  test('script_dblclick_options', async ({ page }) => {
    // Full `DblClickOptions` surface, one page-visible probe per field.

    // 1. Baseline: ondblclick handler fires on a plain dblclick().
    //    Proves the click_count=2 lowering actually produces a DOM
    //    dblclick event (not just two disconnected clicks).
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='out'>no</div>" +
          "<script>document.getElementById('b').addEventListener('dblclick',()=>document.getElementById('out').textContent='yes')</script>",
      ),
    );
    await page.locator('#b').dblclick();
    expect(await page.evaluate("document.getElementById('out').textContent")).toBe('yes');

    // 2. modifiers:['Shift'] — dblclick event carries shiftKey.
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='out'>no</div>" +
          "<script>document.getElementById('b').addEventListener('dblclick',e=>document.getElementById('out').textContent=e.shiftKey?'shift':'none')</script>",
      ),
    );
    await page.locator('#b').dblclick({ modifiers: ['Shift'] });
    expect(await page.evaluate("document.getElementById('out').textContent")).toBe('shift');

    // 3. position:{x:15,y:25} — the dblclick event fires at the offset
    //    (not the element centre).
    await page.goto(
      dataUrl(
        "<div id='b' style='width:200px;height:100px;background:#ccc'></div><div id='out'>none</div>" +
          "<script>document.getElementById('b').addEventListener('dblclick',e=>{var r=e.currentTarget.getBoundingClientRect();document.getElementById('out').textContent=(Math.round(e.clientX-r.left))+','+(Math.round(e.clientY-r.top))})</script>",
      ),
    );
    await page.locator('#b').dblclick({ position: { x: 15, y: 25 } });
    expect(await page.evaluate("document.getElementById('out').textContent")).toBe('15,25');

    // 4. delay:120 — each mousedown->mouseup pair holds the button for
    //    >= 80ms (conservative floor). Record the first down->up gap.
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='out'>0</div>" +
          '<script>' +
          'let downAt = 0;' +
          'let gap = null;' +
          "const b = document.getElementById('b');" +
          "b.addEventListener('mousedown', () => { downAt = Date.now(); });" +
          "b.addEventListener('mouseup', () => { " +
          "if (gap === null) { gap = Date.now() - downAt; document.getElementById('out').textContent = String(gap); } " +
          '});' +
          '</script>',
      ),
    );
    await page.locator('#b').dblclick({ delay: 120 });
    const gapMs = parseInt((await page.evaluate("document.getElementById('out').textContent")) as string, 10);
    expect(gapMs).toBeGreaterThanOrEqual(80);

    // 5. trial:true — skips the entire click dispatch; ondblclick never
    //    fires but modifier keydown still does (matches Playwright).
    await page.goto(
      dataUrl(
        "<button id='b'>b</button><div id='dbl'>no</div><div id='kd'>none</div>" +
          '<script>' +
          "document.getElementById('b').addEventListener('dblclick',()=>document.getElementById('dbl').textContent='yes');" +
          "document.addEventListener('keydown',e=>{if(e.key==='Shift')document.getElementById('kd').textContent='shift'});" +
          '</script>',
      ),
    );
    await page.locator('#b').dblclick({ trial: true, modifiers: ['Shift'] });
    expect(await page.evaluate("document.getElementById('dbl').textContent")).toBe('no');
    expect(await page.evaluate("document.getElementById('kd').textContent")).toBe('shift');

    // 6. button:'right' — a right-dblclick emits contextmenu events with
    //    event.button === 2. CDP + BiDi produce two (one per click of the
    //    pair), WebKit coalesces occasionally — allow >= 1 while still
    //    proving button:'right' took effect.
    await page.goto(
      dataUrl(
        "<button id='b' oncontextmenu='event.preventDefault()'>b</button><div id='count'>0</div><div id='btn'>-1</div>" +
          '<script>' +
          "const b = document.getElementById('b');" +
          "const cnt = document.getElementById('count');" +
          "const btn = document.getElementById('btn');" +
          "b.addEventListener('contextmenu', e => { " +
          'cnt.textContent = String(parseInt(cnt.textContent,10) + 1); ' +
          'btn.textContent = String(e.button); ' +
          '});' +
          '</script>',
      ),
    );
    await page.locator('#b').dblclick({ button: 'right' });
    const count = parseInt((await page.evaluate("document.getElementById('count').textContent")) as string, 10);
    expect(count).toBeGreaterThanOrEqual(1);
    expect(await page.evaluate("document.getElementById('btn').textContent")).toBe('2');
  });

  test('script_press_options', async ({ page }) => {
    // 1. delay:120 — pressing A with delay produces a keydown->keyup gap
    //    of at least 80ms on every backend.
    await page.goto(
      dataUrl(
        "<input id='i'><div id='out'>0</div>" +
          '<script>' +
          'let downAt = 0;' +
          "const i = document.getElementById('i');" +
          "i.addEventListener('keydown', () => { downAt = performance.now(); });" +
          "i.addEventListener('keyup', () => { " +
          "document.getElementById('out').textContent = String(Math.round(performance.now() - downAt)); " +
          '});' +
          '</script>',
      ),
    );
    await page.locator('#i').click();
    await page.locator('#i').press('A', { delay: 120 });
    const withDelay = parseInt((await page.evaluate("document.getElementById('out').textContent")) as string, 10);
    expect(withDelay).toBeGreaterThanOrEqual(80);

    // 2. delay:0 (default) — the same measurement should be near-zero.
    //    Proves that `delay` actually changed the dispatch path and
    //    wasn't a coincidence of backend scheduler granularity.
    await page.goto(
      dataUrl(
        "<input id='i'><div id='out'>0</div>" +
          '<script>' +
          'let downAt = 0;' +
          "const i = document.getElementById('i');" +
          "i.addEventListener('keydown', () => { downAt = performance.now(); });" +
          "i.addEventListener('keyup', () => { " +
          "document.getElementById('out').textContent = String(Math.round(performance.now() - downAt)); " +
          '});' +
          '</script>',
      ),
    );
    await page.locator('#i').click();
    await page.locator('#i').press('B');
    const noDelay = parseInt((await page.evaluate("document.getElementById('out').textContent")) as string, 10);
    expect(noDelay).toBeLessThan(80);

    // 3. noWaitAfter:true — call returns promptly (< 2s wall-clock). The
    //    event-loop distinction isn't directly observable; the smoke
    //    check is that the option is accepted and the call completes in
    //    bounded time.
    await page.goto(dataUrl("<input id='i'>"));
    await page.locator('#i').click();
    const t0 = Date.now();
    await page.locator('#i').press('C', { noWaitAfter: true });
    expect(Date.now() - t0).toBeLessThan(2000);
  });

  test('script_type_options', async ({ page }) => {
    // 1. delay:50 over 3 chars produces at least 2 inter-stroke gaps
    //    each >= ~35ms (conservative floor; actual ~50ms minus scheduler
    //    jitter). `autofocus` is unreliable across navigations, so click
    //    the input first.
    await page.goto(
      dataUrl(
        "<input id='i'><div id='marks'>[]</div>" +
          '<script>' +
          'const marks = [];' +
          "document.getElementById('i').addEventListener('keydown', () => { " +
          'marks.push(performance.now()); ' +
          "document.getElementById('marks').textContent = JSON.stringify(marks); " +
          '});' +
          '</script>',
      ),
    );
    await page.locator('#i').click();
    await page.locator('#i').type('abc', { delay: 50 });
    const marks = (await page.evaluate("JSON.parse(document.getElementById('marks').textContent)")) as number[];
    expect(marks.length).toBe(3);
    const g1 = marks[1] - marks[0];
    const g2 = marks[2] - marks[1];
    expect(Math.min(g1, g2)).toBeGreaterThanOrEqual(35);

    // 2. Final input value is 'abc' — proves the keys actually typed
    //    into the focused input (not just fired events).
    expect(await page.inputValue('#i')).toBe('abc');

    // 3. delay:0 (default) — three strokes complete well under the
    //    150ms floor that delay:50 would require (3 x 50ms).
    await page.goto(dataUrl("<input id='i'>"));
    await page.locator('#i').click();
    const t0 = Date.now();
    await page.locator('#i').type('xyz');
    expect(Date.now() - t0).toBeLessThan(1000);
    expect(await page.inputValue('#i')).toBe('xyz');
  });

  test('script_set_input_files_polymorphism', async ({ page }) => {
    // Polymorphic `string | string[] | FilePayload | FilePayload[]` —
    // all four forms; assert on `input.files[i].{name,type,size}` so
    // each form produces a distinct page-visible effect. Path forms use
    // real on-disk files written into the test's outputDir through the
    // sandboxed fs global.
    const path1 = test.info().outputPath('ferridriver_opts_a.txt');
    const path2 = test.info().outputPath('ferridriver_opts_b.txt');
    await fs.promises.writeFile(path1, 'alpha');
    await fs.promises.writeFile(path2, 'beta-beta');

    // Form 1 — single path string.
    await page.goto(dataUrl("<input type='file' id='f'>"));
    await page.locator('#f').setInputFiles(path1);
    expect(await page.evaluate("document.getElementById('f').files.length")).toBe(1);
    expect(await page.evaluate("document.getElementById('f').files[0].name")).toBe('ferridriver_opts_a.txt');
    expect(await page.evaluate("document.getElementById('f').files[0].size")).toBe(5);

    // Form 2 — array of path strings. Two files upload in order.
    await page.goto(dataUrl("<input type='file' id='f' multiple>"));
    await page.locator('#f').setInputFiles([path1, path2]);
    expect(await page.evaluate("document.getElementById('f').files.length")).toBe(2);
    expect(await page.evaluate("document.getElementById('f').files[0].size")).toBe(5);
    expect(await page.evaluate("document.getElementById('f').files[1].size")).toBe(9);

    // Form 3 — single in-memory FilePayload. Verify the bytes reach the
    // page intact by reading back name, type, size.
    await page.goto(dataUrl("<input type='file' id='f'>"));
    const payloadBytes = new TextEncoder().encode('payload-body');
    await page.locator('#f').setInputFiles({ name: 'payload.txt', mimeType: 'text/plain', buffer: payloadBytes });
    expect(await page.evaluate("document.getElementById('f').files.length")).toBe(1);
    expect(await page.evaluate("document.getElementById('f').files[0].name")).toBe('payload.txt');
    expect(await page.evaluate("document.getElementById('f').files[0].type")).toBe('text/plain');
    expect(await page.evaluate("document.getElementById('f').files[0].size")).toBe(payloadBytes.length);

    // Form 4 — array of FilePayloads. Mixed names + mimeTypes; two
    // distinct byte counts so the ordering is observable.
    await page.goto(dataUrl("<input type='file' id='f' multiple>"));
    await page.locator('#f').setInputFiles([
      { name: 'a.txt', mimeType: 'text/plain', buffer: new TextEncoder().encode('one') },
      { name: 'b.json', mimeType: 'application/json', buffer: new TextEncoder().encode('twelvebytes!') },
    ]);
    expect(await page.evaluate("document.getElementById('f').files.length")).toBe(2);
    expect(await page.evaluate("document.getElementById('f').files[0].name")).toBe('a.txt');
    expect(await page.evaluate("document.getElementById('f').files[0].type")).toBe('text/plain');
    expect(await page.evaluate("document.getElementById('f').files[0].size")).toBe(3);
    expect(await page.evaluate("document.getElementById('f').files[1].name")).toBe('b.json');
    expect(await page.evaluate("document.getElementById('f').files[1].type")).toBe('application/json');
    expect(await page.evaluate("document.getElementById('f').files[1].size")).toBe(12);
  });
});
