# Configuration

ferridriver looks for `ferridriver.{toml,yaml,yml,json}` by walking up from the current directory. Keys are camelCase on the wire.

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

Projects (matrix runs) in TOML:

```toml
# ferridriver.config.toml
[[projects]]
name = "chromium"
[projects.browser]
browser = "chromium"

[[projects]]
name = "firefox"
[projects.browser]
browser = "firefox"
backend = "bidi"

[[projects]]
name = "webkit"
[projects.browser]
browser = "webkit"
backend = "webkit"
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
