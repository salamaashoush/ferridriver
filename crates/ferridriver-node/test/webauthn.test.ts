/**
 * browserContext.credentials: a virtual WebAuthn authenticator answering
 * navigator.credentials, and its passkeys round-tripping through storage
 * state.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const backend = process.env.FERRIDRIVER_BACKEND ?? "cdp-pipe";

describe(`[${backend}] credentials`, () => {
  let browser: Browser;
  // WebAuthn needs a secure context; localhost is one.
  const server = Bun.serve({ hostname: "localhost", port: 0, fetch: () => new Response("<p>x</p>", { headers: { "content-type": "text/html" } }) });
  const EMPTY_PAGE = `http://localhost:${server.port}/`;

  beforeAll(async () => {
    browser = await launchForBackend(backend);
  });

  afterAll(async () => {
    await browser.close();
    server.stop(true);
  });

  it("registers a passkey in the page and signs in with it elsewhere", async () => {
    const setup = browser.newContext();
    await setup.credentials.install();
    const page = await setup.newPage();
    await page.goto(EMPTY_PAGE);
    const createdId = await page.evaluate(`(async () => {
      const created = await navigator.credentials.create({ publicKey: {
        challenge: crypto.getRandomValues(new Uint8Array(32)),
        rp: { id: 'localhost', name: 'Test RP' },
        user: { id: new Uint8Array([1, 2, 3, 4]), name: 'u', displayName: 'User' },
        pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
        authenticatorSelection: { residentKey: 'required' },
      } });
      return created.id;
    })()`);
    const [captured] = await setup.credentials.get({ rpId: "localhost" });
    expect(captured.id).toBe(createdId as string);
    expect(captured.signCount).toBe(0);
    const state = await setup.storageState({ credentials: true });
    expect(state.credentials).toEqual([captured]);
    await setup.close();

    const context = browser.newContext();
    await context.credentials.create(captured.rpId, captured);
    await context.credentials.install();
    const page2 = await context.newPage();
    await page2.goto(EMPTY_PAGE);
    const gotId = await page2.evaluate(`(async () => (await navigator.credentials.get({ publicKey: {
      challenge: crypto.getRandomValues(new Uint8Array(32)), rpId: 'localhost',
    } })).id)()`);
    expect(gotId).toBe(createdId as string);
    expect((await context.credentials.get())[0].signCount).toBe(1);
    await context.credentials.delete(captured.id);
    expect(await context.credentials.get()).toEqual([]);
    await context.close();
  });

  it("rejects an out-of-range signCount", async () => {
    const context = browser.newContext();
    const error = (await context.credentials.create("example.com", { signCount: -1 }).catch((e: Error) => e)) as Error;
    expect(error.message).toContain("signCount must be between 0 and 4294967295, got -1");
    await context.close();
  });
});
