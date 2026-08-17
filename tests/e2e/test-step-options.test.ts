// `test.step(title, body, { box, location, timeout })`, the
// `TestStepInfo` the body receives, and `test.step.skip`.
//
// Runs on every backend project because a step is a live boundary: it
// opens a reporter event and a trace span around whatever the body does
// to the page, and the timeout races the runner's parked clock rather
// than a bare sleep.
//
// What a spec cannot observe from inside itself is where the step said
// it happened — that reaches reporters, the blob and the test-server
// protocol. `crates/ferridriver-cli/tests/test_step_location.rs` reads
// it back off the live stream.

import { test, describe, expect } from '@ferridriver/test';
import type { TestStepInfo } from '@ferridriver/test';

describe('test.step options', () => {
  test('returns the body value and nests', async ({ page }) => {
    await page.setContent('<h1>stepped</h1>');
    const heading = await test.step('outer', async () => {
      return await test.step('inner', async () => page.locator('h1').textContent());
    });
    expect(heading).toBe('stepped');
  });

  test('a timeout fails the step and the test carries on', async () => {
    let caught: Error | undefined;
    try {
      await test.step(
        'slow',
        async () => {
          await new Promise(() => {});
        },
        { timeout: 200 },
      );
    } catch (error) {
      caught = error as Error;
    }
    expect(caught).toBeTruthy();
    expect(String(caught?.message)).toMatch(/Step timeout of 200ms exceeded\./);
    // The test itself is still running and still passing.
    expect(1).toBe(1);
  });

  test('a step that finishes inside its timeout is unaffected', async ({ page }) => {
    const title = await test.step(
      'fast',
      async () => {
        await page.setContent('<title>quick</title>');
        return page.title();
      },
      { timeout: 5000 },
    );
    expect(title).toBe('quick');
  });

  test('a boxed step attributes its error to the call site', async () => {
    // The failure is raised deep inside `login`; `box` re-attributes it
    // to the line that called `login`, which is in this file.
    const login = async () => {
      await test.step(
        'login',
        async () => {
          throw new Error('credentials rejected');
        },
        { box: true },
      );
    };

    let caught: Error | undefined;
    try {
      await login();
    } catch (error) {
      caught = error as Error;
    }
    expect(caught?.message).toBe('credentials rejected');
    const stack = String(caught?.stack ?? '');
    expect(stack).toContain('credentials rejected');
    // Re-attributed to this spec, not to the frame that threw.
    expect(stack).toContain('test-step-options.test.ts');
    // The boxed stack starts at the caller, so the line that raised is
    // not the one named first.
    expect(stack.split('\n')[1]).toContain('test-step-options.test.ts');
  });

  test('an unboxed step keeps the throwing frame', async () => {
    let caught: Error | undefined;
    try {
      await test.step('plain', async () => {
        throw new Error('boom');
      });
    } catch (error) {
      caught = error as Error;
    }
    expect(caught?.message).toBe('boom');
  });

  test('an explicit location is accepted and the body still runs', async ({ page }) => {
    const marked = await test.step(
      'Given the checkout page',
      async () => {
        await page.setContent('<p id="from-feature">ok</p>');
        return page.locator('#from-feature').textContent();
      },
      { location: { file: 'features/checkout.feature', line: 12, column: 3 } },
    );
    expect(marked).toBe('ok');
  });

  test('test.step.skip leaves the body unrun', async () => {
    let ran = false;
    const result = await test.step.skip('unsupported here', async () => {
      ran = true;
      return 'never';
    });
    expect(ran).toBe(false);
    expect(result).toBe(undefined);
  });

  test('stepInfo.skip() aborts the body without failing the test', async () => {
    let reached = false;
    await test.step('conditionally skipped', async (step: TestStepInfo) => {
      step.skip();
      reached = true;
    });
    expect(reached).toBe(false);
  });

  test('stepInfo.skip(condition) only skips when the condition holds', async () => {
    let ran = 0;
    await test.step('kept', async (step: TestStepInfo) => {
      step.skip(false, 'not this time');
      ran += 1;
    });
    await test.step('dropped', async (step: TestStepInfo) => {
      step.skip(true, 'this time');
      ran += 10;
    });
    expect(ran).toBe(1);
  });

  test('titlePath walks the enclosing steps', async () => {
    const paths: string[][] = [];
    await test.step('outer', async (outer: TestStepInfo) => {
      paths.push(outer.titlePath);
      await test.step('inner', async (inner: TestStepInfo) => {
        paths.push(inner.titlePath);
      });
    });
    expect(paths[0][paths[0].length - 1]).toBe('outer');
    expect(paths[1][paths[1].length - 1]).toBe('inner');
    expect(paths[1][paths[1].length - 2]).toBe('outer');
    // The step path continues the test's own, so a step knows the test
    // it belongs to.
    expect(paths[1].length).toBe(paths[0].length + 1);
    expect(paths[0].slice(0, -1)).toEqual(test.info().titlePath);
  });

  test('stepInfo.attach records an attachment', async ({ testInfo }) => {
    const before = testInfo.attachmentCount;
    await test.step('attaching', async (step: TestStepInfo) => {
      await step.attach('note', { body: 'from a step', contentType: 'text/plain' });
    });
    expect(testInfo.attachmentCount).toBe(before + 1);
  });

  test('an annotation pushed by the body is kept', async () => {
    await test.step('annotated', async (step: TestStepInfo) => {
      step.annotations.push({ type: 'issue', description: 'JIRA-1' });
      expect(step.annotations.length).toBe(1);
    });
  });
});
