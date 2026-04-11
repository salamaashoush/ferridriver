import { defineConfig } from '@ferridriver/test/config';

export default defineConfig({
  testMatch: ['tests/**/*.spec.ts', 'tests/**/*.test.ts', 'tests/**/*.feature'],
  webServer: {
    staticDir: 'tests/assets',
  },
});
