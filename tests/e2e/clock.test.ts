// Ported from crates/ferridriver-cli/tests/backends_support/clock.rs —
// context.clock fake-clock engine (protocol-agnostic: init script +
// main-world evaluates), driven identically on all four backends. Test
// titles mirror the original Rust fn names. The runner gives every test
// a fresh context, so no post-test clock restore is needed.

import { test, describe, expect } from '@ferridriver/test';

describe('clock', () => {
  test('clock_controls_time', async ({ page, context }) => {
    // install -> pauseAt -> runFor fire timers at fake time; the paused
    // clock survives a cross-document navigation (init-script log
    // replay).
    const clock = context.clock;
    await clock.install({ time: '2024-02-02T10:00:00Z' });
    await page.goto('data:text/html,<body>clock</body>');
    await clock.pauseAt('2024-02-02T10:00:05Z');
    const paused = Number(await page.evaluate(() => Date.now()));
    await page.evaluate(() => {
      const w = window as unknown as { __fired: number };
      w.__fired = 0;
      setTimeout(() => {
        w.__fired = Date.now();
      }, 2000);
    });
    await clock.runFor('05');
    const fired = Number(await page.evaluate(() => (window as unknown as { __fired: number }).__fired));
    const after = Number(await page.evaluate(() => Date.now()));
    await page.goto('data:text/html,<body>two</body>');
    const replayed = Number(await page.evaluate(() => Date.now()));
    expect(paused).toBe(1706868005000);
    expect(fired).toBe(1706868007000);
    expect(after).toBe(1706868010000);
    expect(replayed).toBe(1706868010000);
  });

  test('clock_fixed_time_and_errors', async ({ page, context }) => {
    // setFixedTime freezes Date.now while timers keep running; invalid
    // grammar and dates reject with Playwright's messages.
    const clock = context.clock;
    await clock.install({ time: 1000000000000 });
    await page.goto('data:text/html,<body>fixed</body>');
    await clock.setFixedTime(1234567890000);
    const f1 = Number(await page.evaluate(() => Date.now()));
    const f2 = Number(await page.evaluate(() => Date.now()));
    let ticksError = '';
    try {
      await clock.runFor('1:00');
    } catch (e) {
      ticksError = String(e);
    }
    let dateError = '';
    try {
      await clock.pauseAt('not a date');
    } catch (e) {
      dateError = String(e);
    }
    expect(f1).toBe(1234567890000);
    expect(f2).toBe(1234567890000);
    expect(ticksError.includes('mm:ss')).toBe(true);
    expect(dateError.includes('Invalid date')).toBe(true);
  });
});
