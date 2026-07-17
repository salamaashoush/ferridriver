// Ported from crates/ferridriver-cli/tests/backends_support/
// api_response.rs — the browser-independent HTTP client (`request`
// fixture). Test titles mirror the original Rust fn names.

import { test, describe, expect } from '@ferridriver/test';

describe('api request', () => {
  test('api_response_server_addr', async ({ request, baseURL }) => {
    // apiResponse.serverAddr() reports the resolved peer address — a
    // protocol-visible effect that only holds when the value is captured
    // end-to-end.
    const expectedPort = Number(new URL(baseURL!).port);
    const resp = await request.get(`${baseURL}/fx/landed`);
    const addr = resp.serverAddr();
    expect(resp.status()).toBe(200);
    expect(addr).not.toBeNull();
    expect(addr!.ipAddress).toBe('127.0.0.1');
    expect(addr!.port).toBe(expectedPort);
  });
});
