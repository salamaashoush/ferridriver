//! Rule-9 integration tests for `context.clock` / `page.clock` through
//! QuickJS `run_script`, on every backend. The fake-clock engine is
//! protocol-agnostic (init script + main-world evaluates), so all four
//! backends must drive it identically.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_pass_by_value)]

use super::client::McpClient;

/// install → pauseAt → runFor fire timers at fake time; the paused
/// clock survives a cross-document navigation (init-script log replay).
pub fn test_clock_controls_time(c: &mut McpClient) {
  // WebKit's single-context backend shares the MCP default context, so
  // the test restores near-real auto-advancing time afterwards; other
  // backends isolate in a fresh context.
  let fresh_context = c.backend != "webkit";
  let script = if fresh_context {
    r"
    const ctx = await browser.newContext({});
    try {
      const p = await ctx.newPage();
      const clock = ctx.clock;
      await clock.install({ time: '2024-02-02T10:00:00Z' });
      await p.goto('data:text/html,<body>clock</body>');
      await clock.pauseAt('2024-02-02T10:00:05Z');
      const paused = Number(await p.evaluate(() => Date.now()));
      await p.evaluate(() => { window.__fired = 0; setTimeout(() => { window.__fired = Date.now(); }, 2000); });
      await clock.runFor('05');
      const fired = Number(await p.evaluate(() => window.__fired));
      const after = Number(await p.evaluate(() => Date.now()));
      await p.goto('data:text/html,<body>two</body>');
      const replayed = Number(await p.evaluate(() => Date.now()));
      return { paused, fired, after, replayed };
    } finally {
      await ctx.close();
    }
    "
  } else {
    r"
    const clock = page.clock;
    await clock.install({ time: '2024-02-02T10:00:00Z' });
    await page.goto('data:text/html,<body>clock</body>');
    await clock.pauseAt('2024-02-02T10:00:05Z');
    const paused = Number(await page.evaluate(() => Date.now()));
    await page.evaluate(() => { window.__fired = 0; setTimeout(() => { window.__fired = Date.now(); }, 2000); });
    await clock.runFor('05');
    const fired = Number(await page.evaluate(() => window.__fired));
    const after = Number(await page.evaluate(() => Date.now()));
    await page.goto('data:text/html,<body>two</body>');
    const replayed = Number(await page.evaluate(() => Date.now()));
    // Shared context: leave the clock auto-advancing from (near) real
    // time so later tests in this category see sane wall time.
    await clock.setSystemTime(args[0]);
    await clock.resume();
    return { paused, fired, after, replayed };
    "
  };
  let now_ms = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_millis() as u64;
  let v = if fresh_context {
    c.script_value(script)
  } else {
    c.script_value_with_args(script, serde_json::json!([now_ms]))
  };
  assert_eq!(v["paused"].as_i64(), Some(1_706_868_005_000), "pauseAt: {v}");
  assert_eq!(
    v["fired"].as_i64(),
    Some(1_706_868_007_000),
    "timer must fire at its fake due time during runFor: {v}"
  );
  assert_eq!(v["after"].as_i64(), Some(1_706_868_010_000), "runFor advance: {v}");
  assert_eq!(
    v["replayed"].as_i64(),
    Some(1_706_868_010_000),
    "paused clock must survive navigation via log replay: {v}"
  );
}

/// setFixedTime freezes `Date.now` while timers keep running; invalid
/// grammar and dates reject with Playwright's messages.
pub fn test_clock_fixed_time_and_errors(c: &mut McpClient) {
  let fresh_context = c.backend != "webkit";
  let script = if fresh_context {
    r"
    const ctx = await browser.newContext({});
    try {
      const p = await ctx.newPage();
      const clock = ctx.clock;
      await clock.install({ time: 1000000000000 });
      await p.goto('data:text/html,<body>fixed</body>');
      await clock.setFixedTime(1234567890000);
      const f1 = Number(await p.evaluate(() => Date.now()));
      const f2 = Number(await p.evaluate(() => Date.now()));
      let ticksError = '';
      try { await clock.runFor('1:00'); } catch (e) { ticksError = String(e); }
      let dateError = '';
      try { await clock.pauseAt('not a date'); } catch (e) { dateError = String(e); }
      return { f1, f2, ticksError, dateError };
    } finally {
      await ctx.close();
    }
    "
  } else {
    r"
    const clock = page.clock;
    await clock.setFixedTime(1234567890000);
    const f1 = Number(await page.evaluate(() => Date.now()));
    const f2 = Number(await page.evaluate(() => Date.now()));
    let ticksError = '';
    try { await clock.runFor('1:00'); } catch (e) { ticksError = String(e); }
    let dateError = '';
    try { await clock.pauseAt('not a date'); } catch (e) { dateError = String(e); }
    await clock.setSystemTime(args[0]);
    await clock.resume();
    return { f1, f2, ticksError, dateError };
    "
  };
  let now_ms = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_millis() as u64;
  let v = if fresh_context {
    c.script_value(script)
  } else {
    c.script_value_with_args(script, serde_json::json!([now_ms]))
  };
  assert_eq!(v["f1"].as_i64(), Some(1_234_567_890_000), "setFixedTime: {v}");
  assert_eq!(
    v["f2"].as_i64(),
    Some(1_234_567_890_000),
    "fixed time must not advance: {v}"
  );
  assert!(
    v["ticksError"].as_str().unwrap_or("").contains("mm:ss"),
    "bad ticks grammar must reject with Playwright's message: {v}"
  );
  assert!(
    v["dateError"].as_str().unwrap_or("").contains("Invalid date"),
    "bad date must reject with Playwright's message: {v}"
  );
}

pub fn register(set: &mut super::super::TestSet<'_>) {
  set.run(
    "backends_support::clock::test_clock_controls_time",
    test_clock_controls_time,
  );
  set.run(
    "backends_support::clock::test_clock_fixed_time_and_errors",
    test_clock_fixed_time_and_errors,
  );
}
