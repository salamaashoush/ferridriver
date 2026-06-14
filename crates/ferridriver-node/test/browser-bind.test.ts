// NAPI `browser.bind()` / `browser.unbind()` coverage. Binds a live browser
// over a loopback TCP endpoint, then connects a raw socket and speaks the
// session NUL-JSON command protocol to prove the bound server actually drives
// the browser. Mirrors Playwright's `browser.bind(title, { host, port })`.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe"];

// Speak one NUL-delimited-JSON command over a TCP endpoint and await the reply.
async function callSession(
  endpoint: string,
  command: Record<string, unknown>,
): Promise<{ ok: boolean; text: string; error?: string }> {
  const url = new URL(endpoint); // ws://host:port
  const chunks: Uint8Array[] = [];
  let resolveReply: (v: { ok: boolean; text: string; error?: string }) => void;
  let rejectReply: (e: unknown) => void;
  const reply = new Promise<{ ok: boolean; text: string; error?: string }>(
    (res, rej) => {
      resolveReply = res;
      rejectReply = rej;
    },
  );

  const socket = await Bun.connect({
    hostname: url.hostname,
    port: Number(url.port),
    socket: {
      data(_sock, data: Uint8Array) {
        chunks.push(data);
        // A reply is one frame terminated by a NUL byte.
        const nul = data.indexOf(0);
        if (nul !== -1) {
          const joined = Buffer.concat(chunks);
          const end = joined.indexOf(0);
          const json = joined.subarray(0, end).toString("utf8");
          resolveReply(JSON.parse(json));
        }
      },
      error(_sock, err) {
        rejectReply(err);
      },
    },
  });

  socket.write(Buffer.from(JSON.stringify(command) + "\0"));
  try {
    return await reply;
  } finally {
    socket.end();
  }
}

for (const backend of BACKENDS) {
  describe(`browser.bind [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.unbind().catch(() => {});
      await browser.close();
    });

    it("bind returns a ws endpoint and the bound server drives the browser", async () => {
      const page = await browser.newPage();
      await page.setContent("<h1 id=greet>bound!</h1>");

      const { endpoint } = await browser.bind("napi-test", {
        host: "127.0.0.1",
        port: 0,
      });
      expect(endpoint).toStartWith("ws://127.0.0.1:");

      // A url verb over the session socket reaches this exact browser.
      const reply = await callSession(endpoint, {
        id: 1,
        verb: "snapshot",
        args: {},
      });
      expect(reply.ok).toBe(true);
      expect(reply.text).toContain("bound!");

      await browser.unbind();
    });

    it("unbind is idempotent and safe before any bind", async () => {
      await browser.unbind();
      await browser.unbind();
    });
  });
}
