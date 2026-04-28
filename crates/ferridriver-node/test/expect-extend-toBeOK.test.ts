// Cluster 4 — expect.extend + APIResponse.toBeOK (§7.15 / §7.16).

import { test, expect as bunExpect } from 'bun:test';
import { tmpdir } from 'os';
import { join } from 'path';
import { ApiRequestContext } from '../index.js';
import { expect } from '../../../packages/ferridriver-test/src/expect';

test('expect.extend registers a custom matcher used through ValueAssertions', async () => {
  expect.extend({
    toBeWithin(actual: number, lo: number, hi: number) {
      const pass = actual >= lo && actual <= hi;
      return {
        pass,
        message: () => pass
          ? `expected ${actual} not to be within [${lo}, ${hi}]`
          : `expected ${actual} to be within [${lo}, ${hi}]`,
      };
    },
  });

  // Pass case
  await (expect(5) as any).toBeWithin(0, 10);
  // Fail case
  await bunExpect(async () => (expect(50) as any).toBeWithin(0, 10)).toThrow();
});

test('expect.extend matchers compose with .not', async () => {
  expect.extend({
    toBeEvenNumber(actual: number) {
      const pass = typeof actual === 'number' && actual % 2 === 0;
      return {
        pass,
        message: () => pass
          ? `expected ${actual} not to be an even number`
          : `expected ${actual} to be an even number`,
      };
    },
  });

  await (expect(4) as any).toBeEvenNumber();
  await (expect(3).not as any).toBeEvenNumber();
  await bunExpect(async () => (expect(3) as any).toBeEvenNumber()).toThrow();
});

// Spawn a one-shot HTTP server that emits the requested status. Lets the
// toBeOK tests run without an internet round-trip (httpbin/etc) so they
// stay deterministic in CI / offline.
function startStatusServer(): Promise<{ url: string; close: () => void }> {
  const Bun = (globalThis as any).Bun;
  if (!Bun) throw new Error('toBeOK status server requires Bun');
  return new Promise((resolve) => {
    const server = Bun.serve({
      port: 0,
      fetch(req: Request) {
        const url = new URL(req.url);
        const m = url.pathname.match(/\/status\/(\d{3})/);
        const code = m ? Number(m[1]) : 200;
        return new Response(`status ${code}`, { status: code });
      },
    });
    resolve({
      url: `http://127.0.0.1:${server.port}`,
      close: () => server.stop(true),
    });
  });
}

test('APIResponse.toBeOK passes for 2xx', async () => {
  const server = await startStatusServer();
  try {
    const ctx = ApiRequestContext.create({ timeout: 5000 });
    const resp = await ctx.get(`${server.url}/status/200`);
    expect(resp).toBeOK();
  } finally {
    server.close();
  }
});

test('APIResponse.toBeOK fails for non-2xx', async () => {
  const server = await startStatusServer();
  try {
    const ctx = ApiRequestContext.create({ timeout: 5000 });
    const resp = await ctx.get(`${server.url}/status/500`);
    await bunExpect(async () => expect(resp).toBeOK()).toThrow();
  } finally {
    server.close();
  }
});

test('APIResponse.toBeOK + .not inverts', async () => {
  const server = await startStatusServer();
  try {
    const ctx = ApiRequestContext.create({ timeout: 5000 });
    const resp404 = await ctx.get(`${server.url}/status/404`);
    expect(resp404).not.toBeOK();
  } finally {
    server.close();
  }
});
