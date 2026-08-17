// Playwright's Page / BrowserContext / Browser are Node event emitters,
// and an unmodified suite (MSW's browser integration is the obvious one)
// uses far more of that surface than `on` / `off`: it chains
// registrations, inserts a listener at the FRONT, counts listeners, and
// removes one registration by identity while leaving its sibling
// attached.
//
// Every assertion here observes ordering or delivery, never just that a
// call returned.

import { test, describe, expect } from '@ferridriver/test';
import type { BrowserContext, ConsoleMessage, Page } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

const BLANK = dataUrl('<p>emitter</p>');

/// A console round-trip is the cheapest observable page event.
async function log(page: Page, text: string) {
  await page.evaluate(`console.log(${JSON.stringify(text)})`);
}

async function settle(page: Page) {
  await page.evaluate('1');
  await page.evaluate('1');
}

describe('event emitter surface', () => {
  test('on returns the emitter, so registrations chain', async ({ page }) => {
    await page.goto(BLANK);
    page.removeAllListeners();
    const seen: string[] = [];
    const chained = page
      .on('console', (msg) => seen.push(`a:${(msg as ConsoleMessage).text()}`))
      .on('console', (msg) => seen.push(`b:${(msg as ConsoleMessage).text()}`));
    expect(chained === page).toBe(true);
    await log(page, 'chained');
    await settle(page);
    expect(seen).toEqual(['a:chained', 'b:chained']);
  });

  test('prependListener runs before the listeners already attached', async ({ page }) => {
    await page.goto(BLANK);
    page.removeAllListeners();
    const order: string[] = [];
    page.on('console', () => order.push('second'));
    page.prependListener('console', () => order.push('first'));
    await log(page, 'ordered');
    await settle(page);
    expect(order).toEqual(['first', 'second']);
  });

  test('prependOnceListener fires first and only once', async ({ page }) => {
    await page.goto(BLANK);
    page.removeAllListeners();
    const order: string[] = [];
    page.on('console', () => order.push('always'));
    page.prependOnceListener('console', () => order.push('once'));
    await log(page, 'one');
    await settle(page);
    await log(page, 'two');
    await settle(page);
    expect(order).toEqual(['once', 'always', 'always']);
  });

  test('removeListener drops one registration by identity, not both', async ({ page }) => {
    await page.goto(BLANK);
    page.removeAllListeners();
    const seen: string[] = [];
    const first = () => seen.push('first');
    const second = () => seen.push('second');
    page.addListener('console', first);
    page.addListener('console', second);
    page.removeListener('console', first);
    await log(page, 'after-removal');
    await settle(page);
    expect(seen).toEqual(['second']);
  });

  test('listeners, listenerCount and eventNames report the registrations', async ({ page }) => {
    await page.goto(BLANK);
    // A worker reuses its page across the tests in a file, so start
    // from a known-empty emitter rather than assuming one.
    page.removeAllListeners();
    const first = () => {};
    const second = () => {};
    page.on('console', first);
    page.on('console', second);
    page.on('load', () => {});
    expect(page.listenerCount('console')).toBe(2);
    expect(page.listenerCount('response')).toBe(0);
    const listeners = page.listeners('console');
    expect(listeners.length).toBe(2);
    expect(listeners[0] === first).toBe(true);
    expect(listeners[1] === second).toBe(true);
    expect(page.rawListeners('console').length).toBe(2);
    const names = page.eventNames().sort();
    expect(names.includes('console')).toBe(true);
    expect(names.includes('load')).toBe(true);
  });

  test('removeAllListeners drops one event, then all of them', async ({ page }) => {
    await page.goto(BLANK);
    page.removeAllListeners();
    const seen: string[] = [];
    page.on('console', () => seen.push('console'));
    // The load listener is only here to prove the OTHER event survives
    // `removeAllListeners('console')`; it must not write into `seen`,
    // since a late `load` from the navigation above would then look
    // like a console delivery that should not have happened.
    page.on('load', () => {});
    expect(page.removeAllListeners('console') === page).toBe(true);
    expect(page.listenerCount('console')).toBe(0);
    expect(page.listenerCount('load')).toBe(1);
    await log(page, 'ignored');
    await settle(page);
    expect(seen).toEqual([]);
    page.removeAllListeners();
    // Counted per event rather than asserting an empty `eventNames()`:
    // a worker reuses its page across tests in the file, so another
    // test's registration can still be attached here.
    expect(page.listenerCount('console')).toBe(0);
    expect(page.listenerCount('load')).toBe(0);
  });

  test('removeAllListeners with a behavior returns a promise that settles', async ({ page }) => {
    await page.goto(BLANK);
    page.removeAllListeners();
    const seen: string[] = [];
    page.on('console', () => seen.push('console'));
    const result = page.removeAllListeners('console', { behavior: 'wait' });
    expect(typeof (result as Promise<void>).then).toBe('function');
    await result;
    await log(page, 'after');
    await settle(page);
    expect(seen).toEqual([]);
  });

  test('setMaxListeners and getMaxListeners round-trip', async ({ page }) => {
    const before = page.getMaxListeners();
    expect(page.setMaxListeners(before + 15) === page).toBe(true);
    expect(page.getMaxListeners()).toBe(before + 15);
    page.setMaxListeners(before);
    expect(page.getMaxListeners()).toBe(before);
  });

  test('context.on delivers the live page it names', async ({ page, context }: { page: Page; context: BrowserContext }) => {
    await page.goto(BLANK);
    const urls: string[] = [];
    const listener = (created: unknown) => urls.push(typeof (created as Page).url === 'function' ? 'page' : 'other');
    expect(context.on('page', listener) === context).toBe(true);
    expect(context.listenerCount('page')).toBe(1);
    const opened = await context.newPage();
    await opened.goto(BLANK);
    await settle(page);
    expect(urls).toEqual(['page']);
    context.removeListener('page', listener);
    expect(context.listenerCount('page')).toBe(0);
    await opened.close();
  });
});
