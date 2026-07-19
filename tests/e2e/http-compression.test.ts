// Transparent response decompression on the shared fetch engine
// (Playwright: server/fetch.ts pipes gzip/deflate/br/zstd through
// zlib before the body reaches APIResponse).
//
// `/fx/compressed/<algo>` always encodes with <algo> and echoes back the
// Accept-Encoding it received, so a single response covers both halves:
// the client advertised the coding, and the client decoded the reply.
// Undecoded, the payload is compressed bytes and json() cannot parse it.

import { test, describe, expect } from '@ferridriver/test';

const CODINGS = ['gzip', 'deflate', 'br', 'zstd'] as const;

type Probe = { algo: string; acceptEncoding: string; payload: string };

const EXPECTED_PAYLOAD = 'ferridriver-compression-probe '.repeat(64);

describe('response decompression', () => {
  for (const algo of CODINGS) {
    test(`standalone_request_decodes_${algo}`, async ({ request, baseURL }) => {
      const resp = await request.get(`${baseURL}/fx/compressed/${algo}`);
      expect(resp.status()).toBe(200);

      const body = (await resp.json()) as Probe;
      expect(body.algo).toBe(algo);
      expect(body.payload).toBe(EXPECTED_PAYLOAD);
      // The client advertised this coding, which is why the server used it.
      expect(body.acceptEncoding).toContain(algo);

      // Decoded bodies must not keep announcing the encoding they no
      // longer carry, nor a content-length measured on the wire.
      const headers = resp.headers();
      expect(headers['content-encoding']).toBeUndefined();
      expect(headers['content-length']).toBeUndefined();
    });
  }

  test('context_bound_request_decodes_gzip', async ({ page, context, baseURL }) => {
    // The context-bound pool builds its clients through the same
    // build_client() as the standalone pool; assert the jar-less,
    // bridge-backed variant decodes too.
    await page.goto(`${baseURL}/fx/landed`);
    const resp = await context.request.get(`${baseURL}/fx/compressed/gzip`);
    const body = (await resp.json()) as Probe;
    expect(body.algo).toBe('gzip');
    expect(body.payload).toBe(EXPECTED_PAYLOAD);
    expect(resp.headers()['content-encoding']).toBeUndefined();
  });

  test('all_codings_advertised_in_one_accept_encoding', async ({ request, baseURL }) => {
    const resp = await request.get(`${baseURL}/fx/compressed/gzip`);
    const { acceptEncoding } = (await resp.json()) as Probe;
    for (const algo of CODINGS) {
      expect(acceptEncoding).toContain(algo);
    }
  });
});
