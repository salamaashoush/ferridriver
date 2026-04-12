import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.',
  testMatch: 'bench_compare.spec.ts',
  fullyParallel: true,
  timeout: 30000,
  retries: 0,
  reporter: 'null',
  use: { headless: true },
});
