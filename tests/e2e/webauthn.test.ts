// Ported from Playwright's tests/library/browsercontext-webauthn.spec.ts
// and the credential cases of browsercontext-storage-state.spec.ts: the
// context's virtual authenticator answers navigator.credentials from the
// passkeys it holds, which round-trip through storage state.
import { test, expect } from '@ferridriver/test';
import type { Browser, BrowserContext, Page, StorageState } from '@ferridriver/test';

const HOSTNAME = 'localhost';
const EMPTY_PAGE = 'http://localhost:47831/fx/landed';

function contexts(browser: Browser) {
  const opened: BrowserContext[] = [];
  const create = async (options?: { storageState?: StorageState }) => {
    const context = await browser.newContext(options as any);
    opened.push(context);
    return context;
  };
  const closeAll = async () => {
    for (const context of opened)
      await context.close();
  };
  return { create, closeAll };
}

async function assertAndGetSignCount(page: Page, rpId: string) {
  return await page.evaluate(async ({ rpId }: { rpId: string }) => {
    const challenge = crypto.getRandomValues(new Uint8Array(32));
    const cred = await navigator.credentials.get({ publicKey: { challenge, rpId, userVerification: 'preferred' } }) as PublicKeyCredential;
    const resp = cred.response as AuthenticatorAssertionResponse;
    return new DataView(resp.authenticatorData).getUint32(33);
  }, { rpId });
}

async function registerPasskey(page: Page, rpId: string) {
  return await page.evaluate(async ({ rpId }: { rpId: string }) => {
    const created = await navigator.credentials.create({
      publicKey: {
        challenge: crypto.getRandomValues(new Uint8Array(32)),
        rp: { id: rpId, name: 'Test RP' },
        user: { id: new Uint8Array([1, 2, 3, 4]), name: 'u', displayName: 'User' },
        pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
        authenticatorSelection: { residentKey: 'required', userVerification: 'preferred' },
      },
    }) as PublicKeyCredential;
    return created.id;
  }, { rpId });
}

async function discoverableId(page: Page, rpId: string) {
  return await page.evaluate(async ({ rpId }: { rpId: string }) => {
    const cred = await navigator.credentials.get({
      publicKey: { challenge: crypto.getRandomValues(new Uint8Array(32)), rpId, userVerification: 'preferred' },
    }) as PublicKeyCredential;
    return cred.id;
  }, { rpId });
}

test.describe('credentials', () => {
  test('does not intercept navigator.credentials without install()', async ({ browser }) => {
    const { create, closeAll } = contexts(browser);
    try {
      const context = await create();
      await context.credentials.create(HOSTNAME);
      const page = await context.newPage();
      await page.goto(EMPTY_PAGE);
      expect(await page.evaluate(() => (globalThis as any).__pwWebAuthnInstalled === true)).toBe(false);
    } finally {
      await closeAll();
    }
  });

  test('seeds a known credential and authenticates', async ({ browser }) => {
    const { create, closeAll } = contexts(browser);
    try {
      const known = await (await create()).credentials.create(HOSTNAME);
      const context = await create();
      await context.credentials.create(known.rpId, known);
      await context.credentials.install();
      const page = await context.newPage();
      await page.goto(EMPTY_PAGE);
      const signIn = (credentialId: string) => page.evaluate(async ({ rpId, credentialId }: { rpId: string; credentialId: string }) => {
        const b64UrlToBytes = (s: string) => {
          let str = s.replace(/-/g, '+').replace(/_/g, '/');
          while (str.length % 4)
            str += '=';
          return Uint8Array.from(atob(str), c => c.charCodeAt(0));
        };
        try {
          const cred = await navigator.credentials.get({
            publicKey: {
              challenge: crypto.getRandomValues(new Uint8Array(32)),
              rpId,
              allowCredentials: [{ type: 'public-key', id: b64UrlToBytes(credentialId) }],
              userVerification: 'preferred',
            },
          }) as PublicKeyCredential;
          const resp = cred.response as AuthenticatorAssertionResponse;
          return { id: cred.id, type: cred.type, signature: resp.signature.byteLength > 0, flags: new Uint8Array(resp.authenticatorData)[32] };
        } catch (e) {
          return { error: (e as DOMException).name };
        }
      }, { rpId: HOSTNAME, credentialId });
      const result = await signIn(known.id) as any;
      expect(result.id).toBe(known.id);
      expect(result.type).toBe('public-key');
      expect(result.signature).toBe(true);
      expect(result.flags & 0x05).toBe(0x05);

      await context.credentials.delete(known.id);
      expect(await context.credentials.get()).toHaveLength(0);
      expect((await signIn(known.id) as any).error).toBe('NotAllowedError');
    } finally {
      await closeAll();
    }
  });

  test('captures a page-created credential and reuses it in another context', async ({ browser }) => {
    const { create, closeAll } = contexts(browser);
    try {
      const setup = await create();
      await setup.credentials.install();
      const setupPage = await setup.newPage();
      await setupPage.goto(EMPTY_PAGE);
      const createdId = await registerPasskey(setupPage, HOSTNAME);

      const [captured] = await setup.credentials.get({ rpId: HOSTNAME });
      expect(captured.id).toBe(createdId);
      expect(captured.privateKey).toMatch(/^[A-Za-z0-9_-]+$/);
      expect(captured.publicKey).toMatch(/^[A-Za-z0-9_-]+$/);

      const context = await create();
      await context.credentials.create(captured.rpId, captured);
      await context.credentials.install();
      const page = await context.newPage();
      await page.goto(EMPTY_PAGE);
      expect(await discoverableId(page, HOSTNAME)).toBe(createdId);
    } finally {
      await closeAll();
    }
  });

  test('reuses a page-created credential through storageState', async ({ browser }) => {
    const { create, closeAll } = contexts(browser);
    try {
      const setup = await create();
      await setup.credentials.install();
      const setupPage = await setup.newPage();
      await setupPage.goto(EMPTY_PAGE);
      const createdId = await registerPasskey(setupPage, HOSTNAME);
      const storageState = await setup.storageState({ credentials: true });

      const context = await create({ storageState });
      const page = await context.newPage();
      await page.goto(EMPTY_PAGE);
      expect(await discoverableId(page, HOSTNAME)).toBe(createdId);
    } finally {
      await closeAll();
    }
  });

  test('seeds and reports signCount', async ({ browser }) => {
    const { create, closeAll } = contexts(browser);
    try {
      const context = await create();
      const fresh = await context.credentials.create('fresh.example.com');
      expect(fresh.signCount).toBe(0);
      const seeded = await context.credentials.create(HOSTNAME, { signCount: 41 });
      expect(seeded.signCount).toBe(41);
      await context.credentials.install();
      const page = await context.newPage();
      await page.goto(EMPTY_PAGE);
      expect(await assertAndGetSignCount(page, HOSTNAME)).toBe(42);
      expect(await assertAndGetSignCount(page, HOSTNAME)).toBe(43);
      expect(await context.credentials.get({ id: seeded.id })).toEqual([{ ...seeded, signCount: 43 }]);
      expect(await context.credentials.get({ id: fresh.id })).toEqual([fresh]);

      const [captured] = await context.credentials.get({ id: seeded.id });
      const context2 = await create();
      await context2.credentials.create(captured.rpId, captured);
      await context2.credentials.install();
      const page2 = await context2.newPage();
      await page2.goto(EMPTY_PAGE);
      expect(await assertAndGetSignCount(page2, HOSTNAME)).toBe(44);
    } finally {
      await closeAll();
    }
  });

  test('rejects an invalid signCount', async ({ browser }) => {
    const { create, closeAll } = contexts(browser);
    try {
      const context = await create();
      const error = await context.credentials.create('example.com', { signCount: -1 }).catch((e: Error) => e) as Error;
      expect(error.message).toContain('signCount must be between 0 and 4294967295, got -1');
      expect(await context.credentials.get()).toEqual([]);
    } finally {
      await closeAll();
    }
  });

  test('round-trips credentials and signCount through storage state', async ({ browser }) => {
    const { create, closeAll } = contexts(browser);
    try {
      const context = await create();
      const credential = await context.credentials.create(HOSTNAME);
      expect((await context.storageState()).credentials).toBeUndefined();
      const storageState = await context.storageState({ credentials: true });
      expect(storageState.credentials).toEqual([credential]);

      const context2 = await create({ storageState });
      expect(await context2.credentials.get()).toEqual([credential]);
      const page = await context2.newPage();
      await page.goto(EMPTY_PAGE);
      expect(await assertAndGetSignCount(page, HOSTNAME)).toBe(1);

      const legacy = { ...storageState, credentials: [{ ...credential, signCount: undefined }] } as any;
      const legacyContext = await create({ storageState: legacy });
      const legacyPage = await legacyContext.newPage();
      await legacyPage.goto(EMPTY_PAGE);
      expect(await assertAndGetSignCount(legacyPage, HOSTNAME)).toBe(1);
    } finally {
      await closeAll();
    }
  });

  test('setStorageState replaces credentials', async ({ browser }) => {
    const { create, closeAll } = contexts(browser);
    try {
      const ctxA = await create();
      const credA = await ctxA.credentials.create('a.example.com');
      const stateA = await ctxA.storageState({ credentials: true });
      const ctxB = await create();
      const credB = await ctxB.credentials.create('b.example.com');
      const stateB = await ctxB.storageState({ credentials: true });

      const context = await create({ storageState: stateA });
      await context.newPage();
      expect(await context.credentials.get()).toEqual([credA]);
      await context.setStorageState(stateB);
      expect(await context.credentials.get()).toEqual([credB]);
      await context.setStorageState({ cookies: [], origins: [] });
      expect(await context.credentials.get()).toEqual([]);
      await context.setStorageState(stateA);
      expect(await context.credentials.get()).toEqual([credA]);
    } finally {
      await closeAll();
    }
  });
});
