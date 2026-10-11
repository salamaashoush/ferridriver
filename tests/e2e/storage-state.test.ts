// storageState({ indexedDB, opfs }) and its restore, through Playwright's
// own StorageScript (injected/src/storageScript.ts). Every test writes
// through the page's real storage APIs and reads back the same way in a
// second context, so nothing passes on the shape of the state alone.

import { test, describe, expect } from '@ferridriver/test';
import type { BrowserContext, Page, StorageState } from '@ferridriver/test';

type Row = { id: number; name: string; when?: Date; bytes?: Uint8Array };

const seedDatabase = (page: Page) =>
  page.evaluate(
    () =>
      new Promise<void>((resolve, reject) => {
        const open = indexedDB.open('library', 2);
        open.onupgradeneeded = () => {
          const store = open.result.createObjectStore('books', { keyPath: 'id' });
          store.createIndex('by_name', 'name', { unique: true });
        };
        open.onerror = () => reject(open.error);
        open.onsuccess = () => {
          const tx = open.result.transaction('books', 'readwrite');
          tx.objectStore('books').put({ id: 1, name: 'plain' });
          // A Date and a typed array are not JSON: they cross encoded.
          tx.objectStore('books').put({ id: 2, name: 'rich', when: new Date(0), bytes: new Uint8Array([1, 2, 3]) });
          tx.oncomplete = () => {
            open.result.close();
            resolve();
          };
          tx.onerror = () => reject(tx.error);
        };
      }),
  );

const readDatabase = (page: Page) =>
  page.evaluate(
    () =>
      new Promise<unknown>((resolve, reject) => {
        const open = indexedDB.open('library');
        open.onerror = () => reject(open.error);
        open.onsuccess = () => {
          const db = open.result;
          if (!db.objectStoreNames.contains('books')) {
            const version = db.version;
            db.close();
            resolve({ version, stores: [] });
            return;
          }
          const store = db.transaction('books', 'readonly').objectStore('books');
          const all = store.getAll();
          all.onerror = () => reject(all.error);
          all.onsuccess = () => {
            const rows = all.result as Row[];
            const rich = rows.find((row) => row.id === 2);
            resolve({
              version: db.version,
              stores: Array.from(db.objectStoreNames),
              keyPath: store.keyPath,
              indexes: Array.from(store.indexNames),
              unique: store.index('by_name').unique,
              names: rows.map((row) => row.name),
              when: rich?.when instanceof Date ? rich.when.getTime() : 'not a Date',
              bytes: rich?.bytes instanceof Uint8Array ? Array.from(rich.bytes) : 'not a Uint8Array',
            });
            db.close();
          };
        };
      }),
  );

const SEEDED = {
  version: 2,
  stores: ['books'],
  keyPath: 'id',
  indexes: ['by_name'],
  unique: true,
  names: ['plain', 'rich'],
  when: 0,
  bytes: [1, 2, 3],
};

const originOf = (state: StorageState, origin: string) => state.origins.find((entry) => entry.origin === origin);

describe('storage state', () => {
  test('indexedDB is exported only when asked for', async ({ page, context, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await seedDatabase(page);

    // Nothing else is stored, so without the option the origin has no entry.
    expect(originOf(await context.storageState(), baseURL!)).toBeUndefined();

    const state = await context.storageState({ indexedDB: true });
    const databases = originOf(state, baseURL!)?.indexedDB;
    expect(databases?.length).toBe(1);
    const database = databases![0];
    expect([database.name, database.version]).toEqual(['library', 2]);
    const store = database.stores[0];
    expect([store.name, store.keyPath, store.autoIncrement]).toEqual(['books', 'id', false]);
    expect(store.indexes).toEqual([{ name: 'by_name', keyPath: 'name', multiEntry: false, unique: true }]);
    expect(store.records.length).toBe(2);
    expect(store.records[0].value).toEqual({ id: 1, name: 'plain' });
    // The rich record has no plain form to offer.
    expect(store.records[1].value).toBeUndefined();
    expect(store.records[1].valueEncoded).toBeDefined();
  });

  test('indexedDB survives into a context created from the state', async ({ page, context, browser, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await seedDatabase(page);
    const state = await context.storageState({ indexedDB: true });

    const restored: BrowserContext = await browser.newContext({ storageState: state });
    try {
      const other = await restored.newPage();
      await other.goto(`${baseURL}/fx/landed`);
      expect(await readDatabase(other)).toEqual(SEEDED);
    } finally {
      await restored.close();
    }
  });

  test('setStorageState replaces the databases an origin had', async ({ page, context, browser, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await seedDatabase(page);
    const state = await context.storageState({ indexedDB: true });

    const target: BrowserContext = await browser.newContext({});
    try {
      const other = await target.newPage();
      await other.goto(`${baseURL}/fx/landed`);
      await other.evaluate(
        () =>
          new Promise<void>((resolve, reject) => {
            const open = indexedDB.open('stale', 1);
            open.onupgradeneeded = () => open.result.createObjectStore('old');
            open.onerror = () => reject(open.error);
            open.onsuccess = () => {
              open.result.close();
              resolve();
            };
          }),
      );
      await target.setStorageState(state);
      await other.goto(`${baseURL}/fx/landed`);
      expect(await readDatabase(other)).toEqual(SEEDED);
      const names = await other.evaluate(async () => (await indexedDB.databases()).map((db) => db.name).sort());
      expect(names).toEqual(['library']);
    } finally {
      await target.close();
    }
  });

  test('the origin private file system round-trips, or is refused where the engine has none', async ({
    page,
    context,
    browser,
    baseURL,
  }) => {
    await page.goto(`${baseURL}/fx/landed`);
    // Playwright's own spec skips WebKit here: a non-persistent WebKit
    // context has no OPFS. What the engine can do decides what is asserted.
    const available = await page.evaluate(() =>
      navigator.storage.getDirectory().then(
        () => true,
        () => false,
      ),
    );
    if (!available) {
      const error = (await context.storageState({ opfs: true }).catch((e: Error) => e)) as Error;
      expect(error instanceof Error).toBe(true);
      expect(error.message).toContain('Unable to serialize OPFS');
      return;
    }

    await page.evaluate(async () => {
      const root = await navigator.storage.getDirectory();
      const nested = await (await root.getDirectoryHandle('docs', { create: true })).getDirectoryHandle('deep', { create: true });
      const writable = await (await nested.getFileHandle('note.txt', { create: true })).createWritable();
      await writable.write('hello opfs');
      await writable.close();
      await root.getDirectoryHandle('empty', { create: true });
    });

    expect(originOf(await context.storageState(), baseURL!)).toBeUndefined();
    const state = await context.storageState({ opfs: true });
    const entries = [...(originOf(state, baseURL!)?.opfs ?? [])].sort((a, b) => a.path.localeCompare(b.path));
    expect(entries).toEqual([
      { path: 'docs', type: 'directory' },
      { path: 'docs/deep', type: 'directory' },
      // base64('hello opfs')
      { path: 'docs/deep/note.txt', type: 'file', base64: 'aGVsbG8gb3Bmcw==' },
      { path: 'empty', type: 'directory' },
    ]);

    const restored: BrowserContext = await browser.newContext({ storageState: state });
    try {
      const other = await restored.newPage();
      await other.goto(`${baseURL}/fx/landed`);
      const seen = await other.evaluate(async () => {
        const root = await navigator.storage.getDirectory();
        const deep = await (await root.getDirectoryHandle('docs')).getDirectoryHandle('deep');
        const text = await (await (await deep.getFileHandle('note.txt')).getFile()).text();
        const top: string[] = [];
        for await (const name of root.keys()) top.push(name);
        return { text, top: top.sort() };
      });
      expect(seen).toEqual({ text: 'hello opfs', top: ['docs', 'empty'] });
    } finally {
      await restored.close();
    }
  });

  test('localStorage still round-trips alongside', async ({ page, context, browser, baseURL }) => {
    await page.goto(`${baseURL}/fx/landed`);
    await page.evaluate(() => localStorage.setItem('theme', 'dark'));
    await seedDatabase(page);
    const state = await context.storageState({ indexedDB: true });
    expect(originOf(state, baseURL!)?.localStorage).toEqual([{ name: 'theme', value: 'dark' }]);

    const restored: BrowserContext = await browser.newContext({ storageState: state });
    try {
      const other = await restored.newPage();
      await other.goto(`${baseURL}/fx/landed`);
      expect(await other.evaluate(() => localStorage.getItem('theme'))).toBe('dark');
    } finally {
      await restored.close();
    }
  });
});
