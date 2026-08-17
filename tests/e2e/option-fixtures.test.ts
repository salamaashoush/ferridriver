// `{ option: true }` fixtures and the `use` bags that set their values.
//
// Playwright's precedence is spec `test.use` > project `use` > config
// `use` > the fixture's own default (`common/fixtures.ts:88-111`, where
// the config overrides are appended right after the declaration so an
// inner `test.use` still shadows them). The config and project halves
// need two projects that differ only in one key, so they live in
// `crates/ferridriver-cli/tests/use_options.rs`, which builds throwaway
// workspaces; this file covers everything a spec can observe on its own
// and runs on every backend project.

import { test as base, describe, expect } from '@ferridriver/test';

type Options = {
  profile: string;
  settings: { depth: number; tags: string[]; nested: { on: boolean } };
  computed: string;
};

const test = base.extend<Options>({
  profile: ['fixture-default', { option: true }],
  settings: [{ depth: 1, tags: ['a'], nested: { on: false } }, { option: true }],
  // An option whose declaration is a FACTORY, not a value: Playwright
  // replaces the registration's `fn` with the override, so a config
  // value has to win over the factory rather than be ignored.
  computed: [
    async ({}, use: (v: string) => Promise<void>) => {
      await use('from-factory');
    },
    { option: true },
  ],
});

// A plain (non-option) fixture in the same chain: a `use` bag must not
// be able to set it, which is what the CLI suite asserts by message.
const withPlain = test.extend<{ plain: string }>({
  plain: async ({}, use: (v: string) => Promise<void>) => use('plain-value'),
});

describe('option fixtures', () => {
  test('an option with no override anywhere is its declared default', async ({ profile }: Options) => {
    expect(profile).toBe('fixture-default');
  });

  test('an option declared as a factory runs the factory when nothing overrides it', async ({
    computed,
  }: Options) => {
    expect(computed).toBe('from-factory');
  });

  test('an object option keeps its whole shape', async ({ settings }: Options) => {
    expect(settings.depth).toBe(1);
    expect(settings.tags).toEqual(['a']);
    expect(settings.nested.on).toBe(false);
  });

  withPlain('a non-option fixture in the same chain still resolves', async ({ plain, profile }) => {
    expect(plain).toBe('plain-value');
    expect(profile).toBe('fixture-default');
  });
});

describe('test.use at describe scope', () => {
  test.use({ profile: 'from-describe' });

  test('sets the option for tests in the group', async ({ profile }: Options) => {
    expect(profile).toBe('from-describe');
  });

  test('leaves an option the group did not name alone', async ({ computed }: Options) => {
    expect(computed).toBe('from-factory');
  });

  describe('nested', () => {
    test.use({ profile: 'from-nested' });

    test('the innermost use wins', async ({ profile }: Options) => {
      expect(profile).toBe('from-nested');
    });
  });

  describe('sibling without its own use', () => {
    test('inherits the enclosing group', async ({ profile }: Options) => {
      expect(profile).toBe('from-describe');
    });
  });
});

describe('test.use with an object value', () => {
  test.use({ settings: { depth: 3, tags: ['x', 'y'], nested: { on: true } } });

  test('round-trips the object deeply', async ({ settings }: Options) => {
    expect(settings.depth).toBe(3);
    expect(settings.tags).toEqual(['x', 'y']);
    expect(settings.nested.on).toBe(true);
    // Not a shallow copy of the default: the default's single tag is
    // gone, so nothing merged the two objects.
    expect(settings.tags.length).toBe(2);
  });
});

describe('test.use over a factory option', () => {
  test.use({ computed: 'from-use' });

  test('the bag value replaces the factory', async ({ computed }: Options) => {
    expect(computed).toBe('from-use');
  });
});

describe('options and the browser', () => {
  test.use({ profile: 'with-page' });

  test('an option resolves alongside the built-in fixtures', async ({ page, profile }) => {
    await page.setContent(`<main id="p">${profile}</main>`);
    expect(await page.locator('#p').textContent()).toBe('with-page');
  });
});

describe('a suite hook resolves options too', () => {
  test.use({ profile: 'for-hook' });

  let seenInHook = '';
  test.beforeAll(async ({ profile }) => {
    // Fails the whole group if the hook resolved against the base chain
    // (`undefined`) or ignored the bag (`fixture-default`).
    if (profile !== 'for-hook') {
      throw new Error(`beforeAll saw profile=${String(profile)}`);
    }
    seenInHook = profile;
  });

  test('beforeAll ran in this worker VM with the group bag', async ({}) => {
    expect(seenInHook).toBe('for-hook');
  });
});
