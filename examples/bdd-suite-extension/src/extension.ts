// What the package contributes to a run that loads it.
//
// Fixtures the suite's steps and specs destructure, a config default so
// adopting the package does not mean copying its settings into every
// config file, and an expect matcher the suite asserts with.

import { defineFixtures, defineDefaults } from 'ferridriver';

defineFixtures({
  // The origin the suite talks to. An option fixture, so a config's
  // `use: { apiOrigin }` overrides it per project.
  apiOrigin: ['https://acme.test', { option: true }],

  // An MSW-style router: `auto`, so every test gets it whether or not
  // the body asked, and the suite never talks to a real network.
  api: [
    async ({ page, apiOrigin }, use) => {
      const seen: string[] = [];
      await page.route(`${apiOrigin}/**`, async (route) => {
        const path = new URL(route.request().url()).pathname;
        seen.push(path);
        if (path === '/') {
          // The router serves the DOCUMENT too, so the page ends up on
          // the mocked origin. A page left on `about:blank` has an
          // opaque origin and its cross-origin fetch is refused before
          // any route sees it.
          await route.fulfill({
            contentType: 'text/html',
            body: '<body data-qa="catalog"><ul id="items"></ul></body>',
          });
          return;
        }
        await route.fulfill({ json: { ok: true, items: ['first', 'second'] } });
      });
      await use({ seen });
    },
    { auto: true },
  ],
});

defineDefaults({
  test: {
    // Nested exactly as a config file nests it.
    browser: { use: { testIdAttribute: 'data-qa' } },
  },
});

expect.extend({
  toHaveServed(received: { seen: string[] }, path: string) {
    return {
      pass: received.seen.includes(path),
      message: () => `expected the router to have served ${path}, saw ${JSON.stringify(received.seen)}`,
    };
  },
});
