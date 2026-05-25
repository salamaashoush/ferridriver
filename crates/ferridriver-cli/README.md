# ferridriver-cli

[![crates.io](https://img.shields.io/crates/v/ferridriver-cli.svg?logo=rust&color=c97b4a)](https://crates.io/crates/ferridriver-cli)
[![docs.rs](https://img.shields.io/docsrs/ferridriver-cli?logo=docs.rs&color=c97b4a)](https://docs.rs/ferridriver-cli)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-c97b4a.svg)](https://github.com/salamaashoush/ferridriver)

The `ferridriver` command-line binary. Five subcommands:

| Subcommand       | Purpose |
|------------------|---------|
| `ferridriver mcp` | Run the MCP server (stdio or HTTP transport) |
| `ferridriver bdd` | Run Gherkin features with Rust and / or JS / TS step bodies |
| `ferridriver test` | Wrap `cargo nextest` / `cargo test` for Rust unit and integration tests |
| `ferridriver run` | Execute a JavaScript script with Playwright-style bindings |
| `ferridriver install` | Download browser binaries into the local cache |

## Install

```bash
# From crates.io
cargo install ferridriver-cli

# From GitHub releases
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

## Global flags

```
-v, --verbose...   Cumulative log level (-v debug, -vv trace incl. CDP)
-c, --config PATH  Config file (TOML / YAML / JSON; format inferred from extension).
                   Auto-searches ferridriver.toml in the current directory and
                   ~/.config/ferridriver/ when not specified.
```

## `ferridriver mcp`

```
--backend BACKEND       cdp-pipe (default) | cdp-raw | webkit | bidi
--headless              Run the browser without a visible window
--executable-path PATH  Path to a Chrome / Chromium binary
--connect URL           WebSocket URL of an already-running browser
--auto-connect CHANNEL  Discover a running Chrome by channel name (mutually exclusive with --connect)
--user-data-dir DIR     Persistent Chrome profile directory (used by --auto-connect)
--transport TRANSPORT   stdio (default) | http
--port N                HTTP port (default: 8080)
```

## `ferridriver bdd`

```
ferridriver bdd [--steps GLOB]... [--tags EXPR] [--workers N]
                [--reporter SPEC]... [--strict] [--dry-run] [--fail-fast]
                [--step-timeout MS] [--order defined|random[:SEED]]
                [--language LANG] [--world-parameters JSON]
                [BROWSER FLAGS]
                FEATURE_GLOB...
```

`--steps` loads JavaScript / TypeScript step-definition files
(repeatable; overrides `[test].steps` from the config file). Files are
bundled with rolldown, compiled to QuickJS bytecode once, and run on the
embedded engine — no Node, no Bun. Defaults to `steps/**/*.{js,ts}` and
`step_definitions/**/*.{js,ts}` when no `--steps` flag is provided.

Browser flags shared with `mcp`: `--backend`, `--headless`,
`--executable-path`, `--connect`, `--auto-connect`, `--user-data-dir`.

## `ferridriver test`

```
ferridriver test [FILTER] [-p PACKAGE]... [--runner nextest|cargo]
                 [--profile PROFILE] [-- PASSTHROUGH...]
```

Auto-detects `cargo-nextest` and falls back to `cargo test`. Useful in
mixed projects where one command should drive any Rust test target.

## `ferridriver run`

```
ferridriver run [SCRIPT|-] [-e CODE] [--timeout-ms MS] [-- ARGS...]
```

Executes a `.js` / `.mjs` script with Playwright-shaped bindings
(`chromium`, `firefox`, `webkit`, `page`, `context`, `request`). `args` is
the positional list exposed as a global. `-` reads from stdin.

## `ferridriver install`

```
ferridriver install [BROWSERS]... [--with-deps]
```

Browsers: `chromium`, `chromium-headless-shell`, `firefox`. Defaults to
`chromium` when none specified. `--with-deps` also installs Linux system
libraries via the platform package manager (`apt-get`, `pacman`, `dnf`,
`brew`).

The WebKit backend uses Playwright's WebKit binary; it is not downloaded
by `ferridriver install` today. Provide it via `FERRIDRIVER_WEBKIT` or use
Playwright's cache (`npx playwright install webkit` once).

## Environment variables

| Variable                  | Purpose |
|---------------------------|---------|
| `RUST_LOG`                | Standard tracing env filter — takes priority. Example: `RUST_LOG=warn,ferridriver::cdp=trace`. |
| `FERRIDRIVER_DEBUG`       | Category-based filter when `RUST_LOG` is unset. Values: `*` / `all`, `cdp`, `step` / `steps`, `hook` / `hooks`, `worker`, `fixture`, `reporter`, `action`, `runner`, or any tracing target. |
| `FERRIDRIVER_PROFILE`     | `chrome` writes `trace-{pid}.json`; `console` enables the `tokio-console` dashboard. |
| `FERRIDRIVER_TRACE_FILE`  | Override the Chrome trace output path. |
| `FERRIDRIVER_BROWSERS_PATH` | Override the browser cache directory. Defaults to platform cache dir (`~/.cache/ferridriver/` on Linux, `~/Library/Caches/ferridriver/` on macOS, `%LOCALAPPDATA%/ferridriver/` on Windows). |
| `FERRIDRIVER_WEBKIT`      | Override the Playwright WebKit checkout path (containing `pw_run.sh`). |

## Configuration file

`ferridriver.toml`, `ferridriver.yaml` / `.yml`, or `ferridriver.json`.
Search order: `-c PATH` → current directory → `~/.config/ferridriver/`.
Wire keys are camelCase.

```toml
[mcp]
# MCP server defaults (browser, transport, instance config).

[test]
# Test runner defaults — workers, timeouts, reporters, projects, features, steps.

[scripting]
allowEnv = ["HOME", "TZ"]  # process.env keys a script may read

extensions = ["./extensions", "./tools/box-login.ts"]
```

See `ferridriver-config` for the full schema. The `mcp` and `test`
sections are documented at <https://salamaashoush.github.io/ferridriver/>.

## License

MIT OR Apache-2.0
