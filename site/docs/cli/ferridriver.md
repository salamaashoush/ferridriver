# `ferridriver`

`ferridriver` is a single static binary with subcommands. There is no
TypeScript CLI — JavaScript/TypeScript BDD step files run natively through
the same binary.

## Synopsis

```
ferridriver [GLOBAL FLAGS] <SUBCOMMAND> [ARGS]
```

Subcommands:

| Command | Purpose |
|---|---|
| `mcp` | Run the MCP server (stdio or HTTP) for AI agents |
| `bdd` | Run Gherkin features (Rust and/or JS/TS step bodies) |
| `test` | Wrap `cargo nextest` / `cargo test` for Rust unit/integration tests |
| `run` | Execute a JS script with Playwright-style bindings |
| `install` | Download browser binaries into the local cache |

## Global flags

```
-v, --verbose...           increase log level (-v = info+debug, -vv = trace)
-c, --config <PATH>        YAML / TOML / JSON config file
```

## `ferridriver mcp`

```
    --backend <B>          cdp-pipe (default) | cdp-raw | webkit | bidi
    --headless             run browser headless
    --executable-path      path to a Chrome / Chromium binary
    --connect <URL>        WebSocket URL of a running browser
    --auto-connect <CH>    discover a running browser by channel name
    --user-data-dir <DIR>  persistent Chrome profile directory
    --transport <T>        stdio (default) | http
    --port <N>             HTTP port (default: 8080)
```

## `ferridriver bdd`

```
ferridriver bdd [--steps <GLOB>]... [--tags <EXPR>] [--workers <N>]
                [--reporter <SPEC>]... [--strict] [--dry-run]
                [--order defined|random[:SEED]] [--language <LANG>]
                <FEATURE GLOBS>...
```

`--steps` loads JavaScript/TypeScript step-definition files (repeatable;
overrides `[test].steps`). They are bundled with rolldown, compiled to
QuickJS bytecode once, and run through the core test runner — no Node or
Bun required.

## `ferridriver install`

```
ferridriver install [chromium|chromium-headless-shell|firefox]... [--with-deps]
```

Defaults to `chromium`. `--with-deps` also installs required system
libraries (Linux).

## Install the binary

```bash
# From crates.io
cargo install ferridriver-cli

# From GitHub releases (prebuilt binary)
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

See [MCP > Setup](/mcp/setup) for client configuration snippets.
