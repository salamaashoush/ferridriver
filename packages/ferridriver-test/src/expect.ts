/**
 * Auto-retrying assertions — thin wrapper over Rust core expect.
 * All polling/retry logic runs in Rust for zero NAPI round-trips per retry.
 */

import type { Page, Locator } from 'ferridriver';

const DEFAULT_TIMEOUT = 5000;

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

  async toHaveTitle(expected: string): Promise<void> {
    await this.page.expectTitle(expected, this.isNot, this.timeout);
  }

  async toHaveURL(expected: string): Promise<void> {
    await this.page.expectUrl(expected, this.isNot, this.timeout);
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
    await this.locator.expectVisible(this.isNot, this.timeout);
  }

  async toBeHidden(): Promise<void> {
    await this.locator.expectHidden(this.isNot, this.timeout);
  }

  async toBeEnabled(): Promise<void> {
    await this.locator.expectEnabled(this.isNot, this.timeout);
  }

  async toBeDisabled(): Promise<void> {
    await this.locator.expectDisabled(this.isNot, this.timeout);
  }

  async toBeChecked(): Promise<void> {
    await this.locator.expectChecked(this.isNot, this.timeout);
  }

  async toHaveText(expected: string): Promise<void> {
    await this.locator.expectText(expected, this.isNot, this.timeout);
  }

  async toContainText(expected: string): Promise<void> {
    await this.locator.expectContainText(expected, this.isNot, this.timeout);
  }

  async toHaveValue(expected: string): Promise<void> {
    await this.locator.expectValue(expected, this.isNot, this.timeout);
  }

  async toHaveAttribute(name: string, value: string): Promise<void> {
    await this.locator.expectAttribute(name, value, this.isNot, this.timeout);
  }

  async toHaveCount(expected: number): Promise<void> {
    await this.locator.expectCount(expected, this.isNot, this.timeout);
  }
}

// ── toPass() wrapper ──

interface ToPassOptions {
  timeout?: number;
  intervals?: number[];
  message?: string;
}

class ToPassWrapper {
  constructor(private block: () => Promise<void>) {}

  async toPass(options: ToPassOptions = {}): Promise<void> {
    const timeout = options.timeout ?? DEFAULT_TIMEOUT;
    const intervals = options.intervals ?? [100, 250, 500, 1000];
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
    throw new Error(`${prefix} failed after ${attempts} attempt(s) (${timeout}ms): ${lastError?.message ?? 'timed out'}`);
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
