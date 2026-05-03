import { defineConfig } from '@ferridriver/test/config';

const PORT = Number(process.env.BENCH_PORT ?? 3030);

export default defineConfig({
  testDir: './tests',
  testMatch: ['**/*.spec.ts'],
  fullyParallel: true,
  timeout: 30_000,
  retries: 0,
  reporter: process.env.BENCH_REPORTER === 'list' ? 'list' : 'null',
  webServer: {
    command: `bun ../app/server.ts`,
    url: `http://localhost:${PORT}/healthz`,
    timeout: 30_000,
    reuseExistingServer: !process.env.CI,
    env: { PORT: String(PORT) },
  },
  use: {
    headless: true,
    baseURL: `http://localhost:${PORT}`,
  },
});
