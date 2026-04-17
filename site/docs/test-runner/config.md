# Configuration

ferridriver looks for `ferridriver.config.toml`, `.json`, or `.ts` by walking up from the current directory.

## Example

```toml
# ferridriver.config.toml
workers = 4
timeout = 30000
expect_timeout = 5000
retries = 1
fully_parallel = true

[browser]
backend = "cdp-pipe"
headless = true

[browser.viewport]
width = 1280
height = 720
```

```ts
// ferridriver.config.ts
import { defineConfig } from '@ferridriver/test/config';

export default defineConfig({
  workers: 4,
  timeout: 30_000,
  retries: 1,
  projects: [
    { name: 'chromium', use: { browser: 'chromium' } },
    { name: 'firefox',  use: { browser: 'firefox',  backend: 'bidi' } },
    { name: 'webkit',   use: { browser: 'webkit',   backend: 'webkit' } },
  ],
});
```

## Priority

From lowest to highest:

1. Config file defaults
2. `main!()` / `HarnessConfig` macro arguments (Rust)
3. Environment variables — `FERRIDRIVER_BACKEND`, `FERRIDRIVER_WORKERS`, `FERRIDRIVER_TIMEOUT`, `FERRIDRIVER_RETRIES`
4. CLI flags — `--headed`, `--backend`, `--workers`, `--timeout`, …

## Web server

```toml
[web_server]
command = "npm run preview"
url = "http://localhost:4173"
reuse_existing_server = true
timeout = 60000
```

Or pass them on the CLI: `--web-server-cmd`, `--web-server-url`, `--web-server-dir`.
