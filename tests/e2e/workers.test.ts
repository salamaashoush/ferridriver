// Playwright's tests/page/workers.spec.ts: a page reports the dedicated
// workers it starts, each can be evaluated in, and each closes with its
// termination or the page's navigation.
import { test, expect } from '@ferridriver/test';
import type { Worker } from '@ferridriver/test';

const ORIGIN = 'http://127.0.0.1:47831';

// Firefox announces a worker's realm over WebDriver BiDi and then refuses
// to run scripts in it; Playwright's own Firefox expectations fail the
// same tests. Where that is so, the refusal has to say it, not time out.
const UNSUPPORTED = "does not run scripts in it";
async function evaluates(worker: Worker): Promise<boolean> {
  const probe = await worker.evaluate('1').then(() => '', (e: Error) => e.message);
  if (probe === '') return true;
  expect(probe).toContain(UNSUPPORTED);
  return false;
}

const start = (source: string) =>
  `window.__worker = new Worker(URL.createObjectURL(new Blob([${JSON.stringify(source)}], { type: 'application/javascript' })))`;

test.describe('workers', () => {
  test('page.workers lists a started worker that evaluates in its own scope', async ({ page }) => {
    await page.goto(`${ORIGIN}/empty.html`);
    expect(page.workers()).toEqual([]);
    const [worker] = await Promise.all([
      page.waitForEvent('worker') as Promise<Worker>,
      page.evaluate(start('self.answer = 42;')),
    ]);
    expect(worker.url()).toContain('blob:');
    expect(page.workers().length).toBe(1);
    expect(page.workers()[0] === worker).toBe(true);
    if (!(await evaluates(worker))) return;
    await expect.poll(() => worker.evaluate('self.answer')).toBe(42);
    expect(await worker.evaluate(() => typeof window)).toBe('undefined');
    expect(await worker.evaluate((n: number) => [n * 2, new Date(0)], 21)).toEqual([42, new Date(0)]);
    expect(await worker.evaluate(async () => { await new Promise(r => setTimeout(r, 10)); return 'later'; })).toBe('later');
    const thrown = await worker.evaluate(() => { throw new Error('in the worker'); }).then(() => 'resolved', (e: Error) => e.message);
    expect(thrown).toContain('in the worker');
  });

  test('a worker closes when terminated', async ({ page }) => {
    await page.goto(`${ORIGIN}/empty.html`);
    const [worker] = await Promise.all([
      page.waitForEvent('worker') as Promise<Worker>,
      page.evaluate(start('self.onmessage = () => {};')),
    ]);
    await Promise.all([
      worker.waitForEvent('close'),
      page.evaluate(() => (window as any).__worker.terminate()),
    ]);
    expect(page.workers()).toEqual([]);
    const after = await worker.evaluate('1').then(() => 'resolved', (e: Error) => e.message);
    expect(after).toContain('closed');
    expect(after).not.toContain(UNSUPPORTED);
  });

  test('close listeners hear the worker, once each, until removed', async ({ page }) => {
    await page.goto(`${ORIGIN}/empty.html`);
    const [worker] = await Promise.all([
      page.waitForEvent('worker') as Promise<Worker>,
      page.evaluate(start('self.onmessage = () => {};')),
    ]);
    const heard: string[] = [];
    const removed = () => heard.push('removed');
    worker.on('close', closed => heard.push(closed === worker ? 'same worker' : 'another worker'));
    worker.once('close', () => heard.push('once'));
    worker.on('close', removed);
    worker.off('close', removed);
    await Promise.all([
      worker.waitForEvent('close'),
      page.evaluate(() => (window as any).__worker.terminate()),
    ]);
    await expect.poll(() => heard.length).toBe(2);
    expect(heard).toEqual(['same worker', 'once']);
  });

  test('workers close when the page navigates away', async ({ page }) => {
    await page.goto(`${ORIGIN}/empty.html`);
    const [worker] = await Promise.all([
      page.waitForEvent('worker') as Promise<Worker>,
      page.evaluate(start('self.onmessage = () => {};')),
    ]);
    await Promise.all([worker.waitForEvent('close', { timeout: 10_000 }), page.goto(`${ORIGIN}/frames/frame.html`)]);
    expect(page.workers()).toEqual([]);
  });

  test('a worker started from an iframe belongs to the page', async ({ page }) => {
    await page.goto(`${ORIGIN}/frames/one-frame.html`);
    const frame = page.frame('child')!;
    const [worker] = await Promise.all([
      page.waitForEvent('worker') as Promise<Worker>,
      frame.evaluate(start('self.from = "frame";')),
    ]);
    expect(page.workers()[0] === worker).toBe(true);
    if (!(await evaluates(worker))) return;
    await expect.poll(() => worker.evaluate('self.from')).toBe('frame');
  });
});
