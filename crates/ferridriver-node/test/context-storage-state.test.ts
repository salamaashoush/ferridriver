// NAPI Rule-9 coverage for `BrowserContext.storageState(options?)` —
// `/tmp/playwright/packages/playwright-core/src/client/browserContext.ts:460`.
//
// We set a cookie + a localStorage entry on a real http origin, then call
// `context.storageState()` and assert BOTH are present in the exported state,
// with Playwright's exact `{ cookies, origins:[{origin, localStorage:[{name,
// value}]}] }` shape. We also assert `{ path }` writes the same JSON to disk.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { createServer, type Server } from "node:http";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readFileSync, rmSync } from "node:fs";
import { type Browser } from "../index.js";
import { launchForBackend } from "./_helpers.js";

let testServer: Server;
let testUrl = "";
let testHost = "";

const FIXTURE = `<!DOCTYPE html><html><head><title>storage</title></head><body><h1>storage</h1></body></html>`;

beforeAll(async () => {
  testServer = createServer((_req, res) => {
    res.writeHead(200, { "Content-Type": "text/html" });
    res.end(FIXTURE);
  });
  await new Promise<void>((resolve) => {
    testServer.listen(0, "127.0.0.1", () => {
      const addr = testServer.address() as any;
      testUrl = `http://127.0.0.1:${addr.port}`;
      testHost = `127.0.0.1`;
      resolve();
    });
  });
});

afterAll(() => {
  testServer?.close();
});

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw"];

for (const backend of BACKENDS) {
  describe(`BrowserContext.storageState [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
    });

    it("exports cookies + per-origin localStorage", async () => {
      const ctx = browser.newContext();
      try {
        const page = await ctx.newPage();
        await page.goto(testUrl, null);
        await ctx.addCookies([
          { name: "sid", value: "abc", domain: testHost, path: "/", secure: false, httpOnly: false },
        ]);
        await page.evaluate("localStorage.setItem('token', 't1')");

        const state = await ctx.storageState();

        const sid = state.cookies.find((c) => c.name === "sid");
        expect(sid).toBeDefined();
        expect(sid!.value).toBe("abc");

        const origin = state.origins.find((o) => o.origin === testUrl);
        expect(origin).toBeDefined();
        const token = origin!.localStorage.find((kv) => kv.name === "token");
        expect(token).toBeDefined();
        expect(token!.value).toBe("t1");
      } finally {
        await ctx.close();
      }
    });

    it("writes the same JSON to { path }", async () => {
      const ctx = browser.newContext();
      const out = join(tmpdir(), `ferri-storage-${backend}-${Date.now()}.json`);
      try {
        const page = await ctx.newPage();
        await page.goto(testUrl, null);
        await page.evaluate("localStorage.setItem('persisted', 'yes')");

        const state = await ctx.storageState({ path: out });
        const onDisk = JSON.parse(readFileSync(out, "utf8"));

        expect(onDisk).toEqual(state);
        const origin = onDisk.origins.find((o: any) => o.origin === testUrl);
        expect(origin).toBeDefined();
        expect(origin.localStorage.find((kv: any) => kv.name === "persisted")?.value).toBe("yes");
      } finally {
        rmSync(out, { force: true });
        await ctx.close();
      }
    });

    it("omits origins with empty localStorage", async () => {
      const ctx = browser.newContext();
      try {
        const page = await ctx.newPage();
        await page.goto(testUrl, null);
        // No localStorage set on this fresh origin.
        const state = await ctx.storageState();
        expect(state.origins.find((o) => o.origin === testUrl)).toBeUndefined();
      } finally {
        await ctx.close();
      }
    });

    // `put` then read back through the page's own IndexedDB and OPFS, so
    // the test fails if the option key does not reach core or the restore
    // does nothing.
    const SEED_DB = `new Promise((resolve, reject) => {
      const open = indexedDB.open('library', 3);
      open.onupgradeneeded = () => open.result.createObjectStore('books', { keyPath: 'id' });
      open.onerror = () => reject(open.error);
      open.onsuccess = () => {
        const tx = open.result.transaction('books', 'readwrite');
        tx.objectStore('books').put({ id: 1, title: 'plain' });
        tx.objectStore('books').put({ id: 2, title: 'rich', when: new Date(0) });
        tx.oncomplete = () => { open.result.close(); resolve(true); };
        tx.onerror = () => reject(tx.error);
      };
    })`;
    const READ_DB = `new Promise((resolve, reject) => {
      const open = indexedDB.open('library');
      open.onerror = () => reject(open.error);
      open.onsuccess = () => {
        const db = open.result;
        if (!db.objectStoreNames.contains('books')) { db.close(); resolve(JSON.stringify({ version: db.version })); return; }
        const all = db.transaction('books', 'readonly').objectStore('books').getAll();
        all.onerror = () => reject(all.error);
        all.onsuccess = () => {
          const rich = all.result.find(row => row.id === 2);
          resolve(JSON.stringify({
            version: db.version,
            titles: all.result.map(row => row.title),
            when: rich && rich.when instanceof Date ? rich.when.getTime() : 'not a Date',
          }));
          db.close();
        };
      };
    })`;

    it("exports IndexedDB with { indexedDB: true } and restores it into a new context", async () => {
      const seed = browser.newContext();
      let state;
      try {
        const page = await seed.newPage();
        await page.goto(testUrl, null);
        await page.evaluate(SEED_DB);
        expect((await seed.storageState()).origins.find((o) => o.origin === testUrl)).toBeUndefined();
        state = await seed.storageState({ indexedDB: true });
        const origin = state.origins.find((o) => o.origin === testUrl);
        expect(origin?.indexedDB?.length).toBe(1);
        expect(origin!.indexedDB![0].name).toBe("library");
        expect(origin!.indexedDB![0].version).toBe(3);
        expect(origin!.indexedDB![0].stores[0].records.length).toBe(2);
        expect(origin!.indexedDB![0].stores[0].records[0].value).toEqual({ id: 1, title: "plain" });
        expect(origin!.indexedDB![0].stores[0].records[1].valueEncoded).toBeDefined();
      } finally {
        await seed.close();
      }

      const restored = browser.newContext({ storageState: state });
      try {
        const page = await restored.newPage();
        await page.goto(testUrl, null);
        expect(JSON.parse((await page.evaluate(READ_DB)) as string)).toEqual({
          version: 3,
          titles: ["plain", "rich"],
          when: 0,
        });
      } finally {
        await restored.close();
      }
    });

    it("exports the origin private file system with { opfs: true } and restores it", async () => {
      const seed = browser.newContext();
      let state;
      try {
        const page = await seed.newPage();
        await page.goto(testUrl, null);
        await page.evaluate(`(async () => {
          const root = await navigator.storage.getDirectory();
          const dir = await root.getDirectoryHandle('docs', { create: true });
          const writable = await (await dir.getFileHandle('note.txt', { create: true })).createWritable();
          await writable.write('hello opfs');
          await writable.close();
          return true;
        })()`);
        state = await seed.storageState({ opfs: true });
        const entries = [...(state.origins.find((o) => o.origin === testUrl)?.opfs ?? [])].sort((a, b) =>
          a.path.localeCompare(b.path),
        );
        expect(entries).toEqual([
          { path: "docs", type: "directory" },
          { path: "docs/note.txt", type: "file", base64: Buffer.from("hello opfs").toString("base64") },
        ]);
      } finally {
        await seed.close();
      }

      const restored = browser.newContext({ storageState: state });
      try {
        const page = await restored.newPage();
        await page.goto(testUrl, null);
        const text = await page.evaluate(`(async () => {
          const root = await navigator.storage.getDirectory();
          const file = await (await (await root.getDirectoryHandle('docs')).getFileHandle('note.txt')).getFile();
          return file.text();
        })()`);
        expect(text).toBe("hello opfs");
      } finally {
        await restored.close();
      }
    });

    it("hydrates a new context from storageState({ path })", async () => {
      const out = join(tmpdir(), `ferri-reload-path-${backend}-${Date.now()}.json`);
      const seed = browser.newContext();
      try {
        const page = await seed.newPage();
        await page.goto(testUrl, null);
        await seed.addCookies([
          { name: "sid", value: "abc", domain: testHost, path: "/", secure: false, httpOnly: false },
        ]);
        await page.evaluate("localStorage.setItem('token', 't1')");
        await seed.storageState({ path: out });
      } finally {
        await seed.close();
      }

      const restored = browser.newContext({ storageState: out });
      try {
        const page = await restored.newPage();
        await page.goto(testUrl, null);
        const cookieValue = await page.evaluate(
          "document.cookie.split('; ').find((c) => c.startsWith('sid='))?.slice(4) ?? ''",
        );
        expect(cookieValue).toBe("abc");
        const token = await page.evaluate("localStorage.getItem('token')");
        expect(token).toBe("t1");
      } finally {
        rmSync(out, { force: true });
        await restored.close();
      }
    });

    it("hydrates a new context from an inline storageState object", async () => {
      const seed = browser.newContext();
      let state: any;
      try {
        const page = await seed.newPage();
        await page.goto(testUrl, null);
        await seed.addCookies([
          { name: "sid", value: "inline-xyz", domain: testHost, path: "/", secure: false, httpOnly: false },
        ]);
        await page.evaluate("localStorage.setItem('token', 'inline-t')");
        state = await seed.storageState();
      } finally {
        await seed.close();
      }

      // Pass the exported state straight back as an inline object.
      const restored = browser.newContext({ storageState: state });
      try {
        const page = await restored.newPage();
        await page.goto(testUrl, null);
        const cookieValue = await page.evaluate(
          "document.cookie.split('; ').find((c) => c.startsWith('sid='))?.slice(4) ?? ''",
        );
        expect(cookieValue).toBe("inline-xyz");
        const token = await page.evaluate("localStorage.getItem('token')");
        expect(token).toBe("inline-t");
      } finally {
        await restored.close();
      }
    });

    it("setStorageState clears existing cookies and applies new ones (1.59)", async () => {
      const ctx = browser.newContext();
      try {
        await ctx.newPage();
        await ctx.addCookies([
          { name: "stale", value: "yes", domain: "example.com", path: "/", secure: false, httpOnly: false },
        ]);
        await ctx.setStorageState({
          cookies: [{ name: "seeded", value: "fromState", domain: "example.com", path: "/" }],
          origins: [],
        });
        const names = (await ctx.cookies()).map((c) => c.name);
        expect(names).not.toContain("stale");
        expect(names).toContain("seeded");
      } finally {
        await ctx.close();
      }
    });
  });
}
