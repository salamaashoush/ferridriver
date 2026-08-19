// The driving case's config: two tag-selected BDD projects over one
// feature set, each spreading a device, with the extension package
// supplying the fixtures the steps destructure and the specifier the
// step file imports.

import { defineConfig, devices } from '@ferridriver/test';

// `extensions` is NOT here: the set of extensions is resolved before a
// config module can be compiled, so it lives in the `ferridriver.toml`
// beside this file. Everything a module CAN decide is here.
export default {
  test: defineConfig({
    features: ['features/**/*.feature'],
    steps: ['steps/**/*.ts'],
    snapshotPathTemplate: '{testDir}/__screenshots__/{projectName}/{platform}/{arg}{ext}',
    projects: [
      {
        name: 'smoke-desktop',
        tags: '@smoke',
        use: { ...devices['Desktop Chrome'], browserName: 'chromium' },
      },
      {
        name: 'slow-desktop',
        tags: '@slow',
        use: { ...devices['Desktop Chrome'], browserName: 'chromium' },
      },
    ],
  }),
};
