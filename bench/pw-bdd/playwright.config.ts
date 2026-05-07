import { defineConfig } from '@playwright/test';
import { defineBddConfig } from 'playwright-bdd';
import { resolve } from 'node:path';

const PORT = Number(process.env.BENCH_PORT ?? 3030);
const FEATURES_ROOT = resolve(import.meta.dirname, '..', 'bdd-features');

const testDir = defineBddConfig({
  featuresRoot: FEATURES_ROOT,
  features: `${FEATURES_ROOT}/generated/*.feature`,
  steps: ['steps/*.ts'],
});

export default defineConfig({
  testDir,
  fullyParallel: true,
  retries: 0,
  reporter: 'null',
  webServer: {
    command: 'bun ../app/server.ts',
    url: `http://localhost:${PORT}/healthz`,
    timeout: 30_000,
    reuseExistingServer: !process.env.CI,
    env: { PORT: String(PORT) },
  },
  use: {
    headless: true,
    baseURL: `http://localhost:${PORT}`,
    launchOptions: process.env.CHROMIUM_PATH ? { executablePath: process.env.CHROMIUM_PATH } : undefined,
  },
});
