// The parity acceptance config.
//
// Deliberately written the way a Playwright user writes one — the
// default export folds through `defineConfig`, projects spread `devices`
// entries, and the snapshot path is a template — while being a
// ferridriver document, so the `[test]` section is where Playwright's
// config shape goes.
//
// It loads NO extensions. Everything asserted here is core, and it must
// keep working for a suite that never writes an extension.

import { defineConfig, devices } from '@ferridriver/test';

export default {
  test: defineConfig(
    {
      testDir: '.',
      testMatch: ['*.spec.ts'],
      // {projectName} and {platform} both resolve, so a baseline is
      // never shared across engines or operating systems.
      snapshotPathTemplate: '{testDir}/__screenshots__/{projectName}/{platform}/{arg}{ext}',
      use: { actionTimeout: 5000 },
    },
    {
      // Folded LEFT: this object is spread ON TOP of the one before it,
      // so the rightmost argument wins — upstream's rule, and the one
      // the phase text had backwards.
      forbidOnly: true,
      projects: [
        {
          name: 'desktop',
          use: { ...devices['Desktop Chrome'], browserName: 'chromium' },
        },
        {
          name: 'mobile',
          // A device descriptor decides the engine nobody else named.
          use: { ...devices['iPhone 15'] },
        },
      ],
    },
  ),
};
