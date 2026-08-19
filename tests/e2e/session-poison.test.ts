// A worker VM force-halted by a test timeout is rebuilt before the next
// test runs in that worker.
//
// A force-halt stops the interpreter wherever it was, so its module
// state is suspect even though its registrations still look intact. The
// rebuild re-evaluates the bundle and re-verifies the registration
// counts; these assert the worker is usable — and CLEAN — afterwards.

import { test, describe, expect } from '@ferridriver/test';

// Module state, so a rebuilt VM is observably a different one.
let moduleCounter = 0;

describe('worker VM poisoning', () => {
  // Expected to fail: timing out IS the point, and the run has to stay
  // green with it in the suite.
  test('a test whose body runs past its timeout is halted', async ({}) => {
    test.fail();
    test.setTimeout(600);
    moduleCounter += 1;
    // A busy loop: no await, so only the interrupt handler can stop it.
    const until = Date.now() + 30_000;
    while (Date.now() < until) {
      /* spin until the deadline force-halts the interpreter */
    }
  });

  test('the next test gets a working VM with module state rebuilt', async ({ page }) => {
    // The VM this runs in was rebuilt, so the module's top-level state
    // starts over rather than carrying the halted test's increment.
    expect(moduleCounter).toBe(0);

    // And it is a usable VM, not a husk: bindings work and the bundle's
    // own imports resolved again.
    await page.setContent('<p id="ok">rebuilt</p>');
    expect(await page.textContent('#ok')).toBe('rebuilt');
  });
});
