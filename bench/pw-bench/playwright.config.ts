import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: '.',
  testMatch: 'bench.spec.ts',
  fullyParallel: true,
  timeout: 10000,
  retries: 0,
  reporter: [['null']],
  use: {
    headless: true,
  },
});
