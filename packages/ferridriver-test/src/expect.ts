/**
 * Auto-retrying assertions — mirrors Playwright's expect() API.
 * Polls locator/page state until condition is met or timeout.
 */

import type { Page, Locator } from 'ferridriver';

const DEFAULT_TIMEOUT = 5000;
const POLL_INTERVALS = [100, 250, 500, 1000];

class ExpectError extends Error {
  constructor(message: string, public diff?: string) {
    super(message);
    this.name = 'ExpectError';
  }
}

async function pollUntil(
  timeout: number,
  check: () => Promise<void>,
): Promise<void> {
  const deadline = Date.now() + timeout;
  let lastError: Error | undefined;
  let idx = 0;

  while (true) {
    try {
      await check();
      return;
    } catch (e) {
      lastError = e as Error;
      const interval = POLL_INTERVALS[Math.min(idx++, POLL_INTERVALS.length - 1)];
      if (Date.now() + interval > deadline) break;
      await new Promise((r) => setTimeout(r, interval));
    }
  }
  throw lastError ?? new ExpectError('assertion timed out');
}

// ── Page assertions ──

class PageAssertions {
  constructor(
    private page: Page,
    private isNot: boolean,
    private timeout: number,
  ) {}

  not = new Proxy(this, {
    get: (target, prop) => {
      const negated = new PageAssertions(target.page, !target.isNot, target.timeout);
      return (negated as any)[prop];
    },
  });

  async toHaveTitle(expected: string | RegExp): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const actual = await this.page.title();
      const matches = typeof expected === 'string' ? actual === expected : expected.test(actual);
      if (matches === this.isNot) {
        throw new ExpectError(
          `expected title ${this.isNot ? 'not ' : ''}${expected}\nreceived: "${actual}"`,
        );
      }
    });
  }

  async toHaveURL(expected: string | RegExp): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const actual = await this.page.url();
      const matches = typeof expected === 'string' ? actual === expected : expected.test(actual);
      if (matches === this.isNot) {
        throw new ExpectError(
          `expected URL ${this.isNot ? 'not ' : ''}${expected}\nreceived: "${actual}"`,
        );
      }
    });
  }
}

// ── Locator assertions ──

class LocatorAssertions {
  constructor(
    private locator: Locator,
    private isNot: boolean,
    private timeout: number,
  ) {}

  not = new Proxy(this, {
    get: (target, prop) => {
      const negated = new LocatorAssertions(target.locator, !target.isNot, target.timeout);
      return (negated as any)[prop];
    },
  });

  async toBeVisible(): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const visible = await this.locator.isVisible();
      if (visible === this.isNot) throw new ExpectError(`expected element ${this.isNot ? 'not ' : ''}to be visible`);
    });
  }

  async toBeHidden(): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const hidden = await this.locator.isHidden();
      if (hidden === this.isNot) throw new ExpectError(`expected element ${this.isNot ? 'not ' : ''}to be hidden`);
    });
  }

  async toBeEnabled(): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const enabled = await this.locator.isEnabled();
      if (enabled === this.isNot) throw new ExpectError(`expected element ${this.isNot ? 'not ' : ''}to be enabled`);
    });
  }

  async toBeDisabled(): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const disabled = await this.locator.isDisabled();
      if (disabled === this.isNot) throw new ExpectError(`expected element ${this.isNot ? 'not ' : ''}to be disabled`);
    });
  }

  async toBeChecked(): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const checked = await this.locator.isChecked();
      if (checked === this.isNot) throw new ExpectError(`expected element ${this.isNot ? 'not ' : ''}to be checked`);
    });
  }

  async toHaveText(expected: string | RegExp): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const actual = (await this.locator.textContent()) ?? '';
      const matches = typeof expected === 'string' ? actual.trim() === expected : expected.test(actual);
      if (matches === this.isNot) {
        throw new ExpectError(`expected text ${this.isNot ? 'not ' : ''}${expected}\nreceived: "${actual}"`);
      }
    });
  }

  async toContainText(expected: string | RegExp): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const actual = (await this.locator.textContent()) ?? '';
      const matches = typeof expected === 'string' ? actual.includes(expected) : expected.test(actual);
      if (matches === this.isNot) {
        throw new ExpectError(`expected text ${this.isNot ? 'not ' : ''}to contain ${expected}\nreceived: "${actual}"`);
      }
    });
  }

  async toHaveValue(expected: string | RegExp): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const actual = await this.locator.inputValue();
      const matches = typeof expected === 'string' ? actual === expected : expected.test(actual);
      if (matches === this.isNot) {
        throw new ExpectError(`expected value ${this.isNot ? 'not ' : ''}${expected}\nreceived: "${actual}"`);
      }
    });
  }

  async toHaveAttribute(name: string, value: string | RegExp): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const actual = (await this.locator.getAttribute(name)) ?? '';
      const matches = typeof value === 'string' ? actual === value : value.test(actual);
      if (matches === this.isNot) {
        throw new ExpectError(`expected attribute "${name}" ${this.isNot ? 'not ' : ''}${value}\nreceived: "${actual}"`);
      }
    });
  }

  async toHaveCount(expected: number): Promise<void> {
    await pollUntil(this.timeout, async () => {
      const actual = await this.locator.count();
      if ((actual === expected) === this.isNot) {
        throw new ExpectError(`expected count ${this.isNot ? 'not ' : ''}${expected}\nreceived: ${actual}`);
      }
    });
  }
}

// ── toPass() wrapper ──

interface ToPassOptions {
  /** Maximum time to retry in ms (default: 5000). */
  timeout?: number;
  /** Retry intervals in ms (default: [100, 250, 500, 1000]). */
  intervals?: number[];
  /** Custom error message on final failure. */
  message?: string;
}

class ToPassWrapper {
  constructor(private block: () => Promise<void>) {}

  async toPass(options: ToPassOptions = {}): Promise<void> {
    const timeout = options.timeout ?? DEFAULT_TIMEOUT;
    const intervals = options.intervals ?? POLL_INTERVALS;
    const deadline = Date.now() + timeout;
    let lastError: Error | undefined;
    let idx = 0;
    let attempts = 0;

    while (true) {
      attempts++;
      try {
        await this.block();
        return;
      } catch (e) {
        lastError = e as Error;
        const interval = intervals[Math.min(idx++, intervals.length - 1)];
        if (Date.now() + interval > deadline) break;
        await new Promise((r) => setTimeout(r, interval));
      }
    }

    const prefix = options.message ?? 'toPass()';
    const msg = `${prefix} failed after ${attempts} attempt(s) (${timeout}ms): ${lastError?.message ?? 'timed out'}`;
    throw new ExpectError(msg);
  }
}

// ── expect() entry point ──

type Assertable = Page | Locator;

function isPage(v: Assertable): v is Page {
  return typeof (v as any).title === 'function' && typeof (v as any).goto === 'function';
}

export function expect(subject: () => Promise<void>): ToPassWrapper;
export function expect(subject: Page, timeout?: number): PageAssertions;
export function expect(subject: Locator, timeout?: number): LocatorAssertions;
export function expect(subject: Assertable | (() => Promise<void>), timeout = DEFAULT_TIMEOUT): PageAssertions | LocatorAssertions | ToPassWrapper {
  if (typeof subject === 'function') {
    return new ToPassWrapper(subject);
  }
  if (isPage(subject)) {
    return new PageAssertions(subject, false, timeout);
  }
  return new LocatorAssertions(subject as Locator, false, timeout);
}
