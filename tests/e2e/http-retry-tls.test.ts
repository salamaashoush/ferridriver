// Per-option coverage for `maxRetries` and per-request
// `ignoreHTTPSErrors` — both reach the core engine but had no test that
// observed the option actually taking effect (Playwright:
// server/fetch.ts retries only ECONNRESET; APIRequestContext options in
// client/fetch.ts).
//
// Two auxiliary fixture listeners make this observable: one aborts a
// budgeted number of connections with a TCP RST, one serves a
// self-signed certificate. Their ephemeral origins come from
// /fx/endpoints.

import { test, describe, expect } from '@ferridriver/test';

type Endpoints = { proxy: string; reset: string; tls: string };

// The four backend projects share one fixture server, so every arming
// call needs its own key or the projects would spend each other's reset
// budget.
let seq = 0;
function resetKey(label: string): string {
  seq += 1;
  return `${label}-${Date.now()}-${seq}-${Math.floor(Math.random() * 1e9)}`;
}

describe('maxRetries', () => {
  test('retries a connection reset until the budget is spent', async ({ request, baseURL }) => {
    const { reset } = (await (await request.get(`${baseURL}/fx/endpoints`)).json()) as Endpoints;
    const key = resetKey('retry-ok');
    // Two resets, then the third connection succeeds.
    await request.get(`${baseURL}/fx/reset-arm?key=${key}&times=2`);

    const resp = await request.get(`${reset}/probe?key=${key}`, { maxRetries: 3 });
    expect(resp.status()).toBe(200);
    expect(await resp.text()).toBe('survived!');
  });

  test('without retries the same reset surfaces as an error', async ({ request, baseURL }) => {
    const { reset } = (await (await request.get(`${baseURL}/fx/endpoints`)).json()) as Endpoints;
    const key = resetKey('retry-none');
    await request.get(`${baseURL}/fx/reset-arm?key=${key}&times=1`);

    // reqwest does not surface the errno in the message, so causality
    // comes from the pairing rather than the string: the identical URL
    // succeeds in the test above once retries are allowed.
    let message = '';
    try {
      await request.get(`${reset}/probe?key=${key}`, { maxRetries: 0 });
    } catch (e) {
      message = String((e as Error)?.message ?? e);
    }
    expect(message).not.toBe('');
  });

  test('too few retries still fails, one more succeeds', async ({ request, baseURL }) => {
    const { reset } = (await (await request.get(`${baseURL}/fx/endpoints`)).json()) as Endpoints;

    // Budget of 2 with only 1 retry (2 attempts) cannot get through...
    const tooFew = resetKey('retry-few');
    await request.get(`${baseURL}/fx/reset-arm?key=${tooFew}&times=2`);
    let failed = false;
    try {
      await request.get(`${reset}/probe?key=${tooFew}`, { maxRetries: 1 });
    } catch {
      failed = true;
    }
    expect(failed).toBe(true);

    // ...while the same budget with 2 retries (3 attempts) does.
    const enough = resetKey('retry-enough');
    await request.get(`${baseURL}/fx/reset-arm?key=${enough}&times=2`);
    const resp = await request.get(`${reset}/probe?key=${enough}`, { maxRetries: 2 });
    expect(resp.status()).toBe(200);
  });
});

describe('per-request ignoreHTTPSErrors', () => {
  test('a self-signed certificate is rejected by default', async ({ request, baseURL }) => {
    const { tls } = (await (await request.get(`${baseURL}/fx/endpoints`)).json()) as Endpoints;
    // reqwest collapses the TLS cause into "error sending request", so
    // the certificate is proven to be the reason by the next test:
    // the same URL returns 200 once the check is waived. The fixture's
    // own reject/accept contract is pinned in
    // crates/ferridriver-fixtures/tests/tls_probe.rs.
    let message = '';
    try {
      await request.get(`${tls}/secure`);
    } catch (e) {
      message = String((e as Error)?.message ?? e);
    }
    expect(message).not.toBe('');
  });

  test('ignoreHTTPSErrors on the request accepts it', async ({ request, baseURL }) => {
    const { tls } = (await (await request.get(`${baseURL}/fx/endpoints`)).json()) as Endpoints;
    const resp = await request.get(`${tls}/secure`, { ignoreHTTPSErrors: true });
    expect(resp.status()).toBe(200);
    expect(await resp.text()).toBe('secured!!');
  });

  test('the option is per-request, not sticky', async ({ request, baseURL }) => {
    const { tls } = (await (await request.get(`${baseURL}/fx/endpoints`)).json()) as Endpoints;
    // Opting in once must not leave the client permanently permissive:
    // the next request without the flag has to fail again.
    const ok = await request.get(`${tls}/secure`, { ignoreHTTPSErrors: true });
    expect(ok.status()).toBe(200);

    let failed = false;
    try {
      await request.get(`${tls}/secure`);
    } catch {
      failed = true;
    }
    expect(failed).toBe(true);
  });
});
