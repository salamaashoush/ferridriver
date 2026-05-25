# ferridriver-config

[![crates.io](https://img.shields.io/crates/v/ferridriver-config.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver-config)
[![docs.rs](https://img.shields.io/docsrs/ferridriver-config?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver-config)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

Unified configuration schema for ferridriver. One file
(`ferridriver.toml`, `ferridriver.yaml` / `.yml`, or `ferridriver.json`)
holds settings for every component: MCP server (`[mcp]`), test runner
(`[test]`), scripting sandbox (`[scripting]`), and extensions
(`extensions`). Keys are camelCase on the wire.

Rust is the source of truth; TOML / YAML / JSON keys are derived via
`serde(rename_all = "camelCase")`. There is no generated TypeScript mirror
— the only consumer is Rust (`ferridriver-cli`, `ferridriver-mcp`,
`ferridriver-test`, `ferridriver-bdd`).

## Search order

1. Explicit path via `ferridriver --config PATH`.
2. `./ferridriver.{toml,yaml,yml,json}` in the current directory.
3. `~/.config/ferridriver/config.{toml,yaml,yml,json}`.

## Example

```toml
[mcp]
[mcp.server]
name = "ferridriver"

[mcp.browser]
backend = "cdp-pipe"
headless = false

[mcp.browser.viewport]
width = 1280
height = 720

[test]
workers = 4
timeout = 30000
expectTimeout = 5000
retries = 1
fullyParallel = true

[test.browser]
backend = "cdp-pipe"
headless = true

[[test.projects]]
name = "chromium"
[test.projects.browser]
browser = "chromium"

[[test.projects]]
name = "firefox"
[test.projects.browser]
browser  = "firefox"
backend  = "bidi"

[[test.projects]]
name = "webkit"
[test.projects.browser]
browser = "webkit"
backend = "webkit"

[[test.webServer]]
command = "npm run preview"
url = "http://localhost:4173"
reuseExistingServer = true
timeout = 60000

[scripting]
allowEnv = ["HOME", "TZ"]   # process.env keys a script may read

extensions = ["./extensions", "./tools/box-login.ts"]
```

## Schema

The two main sections — `[mcp]` and `[test]` — are large. The Rust
structures (`McpConfig`, `TestConfig`, `BrowserConfig`, `ContextConfig`,
`ProjectConfig`, `ReporterConfig`, `WebServerConfig`, `VideoConfig`,
`InstanceConfig`, `ViewportDef`, …) are the canonical reference.

See the rustdoc for full field-by-field documentation. The site docs
include rendered tables: <https://salamaashoush.github.io/ferridriver/test-runner/config>.

## License

MIT OR Apache-2.0
