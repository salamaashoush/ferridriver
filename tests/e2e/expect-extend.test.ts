// `expect.extend` and the expect factory around it. Runs on every
// backend project: a custom matcher drives a live locator, so the whole
// path — context, receiver, verdict, message — has to work on each of
// them.

import { test, describe, expect, mergeExpects } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

function thrown(fn: () => unknown): string {
  try {
    fn();
  } catch (e) {
    return String((e as Error).message ?? e);
  }
  return '';
}

const withinExpect = expect.extend({
  toBeWithin(received: number, lo: number, hi: number) {
    const pass = received >= lo && received <= hi;
    return {
      pass,
      message: () => `expected ${received} ${this.isNot ? 'not ' : ''}to be within ${lo}..${hi}`,
    };
  },
});

describe('expect.extend', () => {
  test('a custom matcher passes, fails and inverts', async () => {
    withinExpect(5).toBeWithin(0, 10);
    withinExpect(50).not.toBeWithin(0, 10);
    expect(thrown(() => withinExpect(50).toBeWithin(0, 10))).toContain('to be within 0..10');
    // `this.isNot` reaches the message.
    expect(thrown(() => withinExpect(5).not.toBeWithin(0, 10))).toContain('not to be within');
  });

  test('a custom matcher drives a live locator', async ({ page }) => {
    await page.goto(dataUrl("<ul><li>a</li><li>b</li><li>c</li></ul>"));
    const counted = expect.extend({
      async toHaveItemCount(locator: { count(): Promise<number> }, expected: number) {
        const actual = await locator.count();
        return {
          pass: actual === expected,
          message: () => `expected ${expected} items, saw ${actual} (timeout ${this.timeout})`,
        };
      },
    });
    await counted(page.locator('li')).toHaveItemCount(3);
    await counted(page.locator('li')).not.toHaveItemCount(4);
    const msg = await (async () => {
      try {
        await counted(page.locator('li')).toHaveItemCount(9);
        return '';
      } catch (e) {
        return String((e as Error).message ?? e);
      }
    })();
    expect(msg).toContain('expected 9 items, saw 3');
    // The matcher body reads the assertion's timeout from the same
    // source the built-ins do.
    expect(msg).toContain('timeout 5000');
  });

  test('a configured timeout is the number the matcher observes', async () => {
    const configured = expect.configure({ timeout: 1234 }).extend({
      toSeeTimeout(_received: unknown) {
        return { pass: this.timeout === 1234, message: () => `timeout was ${this.timeout}` };
      },
    });
    configured(1).toSeeTimeout();
    expect(thrown(() => expect.extend({
      toSeeTimeout(_received: unknown) {
        return { pass: this.timeout === 1234, message: () => `timeout was ${this.timeout}` };
      },
    })(1).toSeeTimeout())).toContain('timeout was 5000');
  });

  test('extend does not clobber a built-in on the original expect', async () => {
    const lying = expect.extend({
      toBe(_received: unknown, _expected: unknown) {
        return { pass: true, message: () => 'always' };
      },
    });
    // The returned expect takes the override…
    lying(1).toBe(2);
    // …and the one it came from keeps the real matcher.
    expect(thrown(() => expect(1).toBe(2))).toContain('toBe');
  });

  test('configure returns a new expect and leaves the old one alone', async () => {
    const labelled = expect.configure({ message: 'ids match' });
    expect(thrown(() => labelled(1).toBe(2))).toContain('ids match');
    expect(thrown(() => expect(1).toBe(2))).not.toContain('ids match');
  });

  test('mergeExpects exposes every matcher', async () => {
    const a = expect.extend({
      toBeAlpha(received: string) {
        return { pass: received === 'a', message: () => 'not alpha' };
      },
    });
    const b = expect.extend({
      toBeBeta(received: string) {
        return { pass: received === 'b', message: () => 'not beta' };
      },
    });
    const both = mergeExpects(a, b);
    both('a').toBeAlpha();
    both('b').toBeBeta();
    expect(thrown(() => both('z').toBeAlpha())).toContain('not alpha');
  });

  test('a custom matcher reaches the settled chain', async () => {
    const a = expect.extend({
      toBeAlpha(received: string) {
        return { pass: received === 'a', message: () => 'not alpha' };
      },
    });
    await a(Promise.resolve('a')).resolves.toBeAlpha();
    await a(Promise.resolve('z')).resolves.not.toBeAlpha();
  });

  test('extend refuses a non-function and junk results', async () => {
    expect(thrown(() => expect.extend({ toBeX: 5 as unknown as () => never }))).toContain('is not a valid matcher');
    const junk = expect.extend({
      toBeJunk(_received: unknown) {
        return 5 as unknown as { pass: boolean };
      },
    });
    expect(thrown(() => junk(1).toBeJunk())).toContain('Unexpected return from a matcher function');
  });

  test('soft is a getter that answers an expect', async () => {
    expect(typeof expect.soft).toBe('function');
    expect.soft(1).toBe(1);
    await expect.soft.poll(() => 1, { timeout: 500, intervals: [5] }).toBe(1);
    expect(typeof expect.getState()).toBe('object');
  });
});
