# `ferridriver`

The `ferridriver` binary is a dedicated MCP server. Test running (E2E, BDD, CT) is handled by the TypeScript CLI at [`ferridriver-test`](/cli/ferridriver-test) or by Rust macros (`main!()`, `bdd_main!()`) via `cargo test`.

## Synopsis

```
ferridriver [FLAGS]
```

## Flags

```
-v, --verbose...           increase log level (-v = info+debug, -vv = trace)
-c, --config <PATH>        YAML / TOML / JSON config file

    --backend <B>          cdp-pipe (default) | cdp-raw | webkit | bidi
    --headless             run browser headless (default: true)
    --executable-path      path to a Chrome / Chromium binary
    --connect <URL>        WebSocket URL of a running browser
    --auto-connect <CH>    discover a running browser by channel name
    --user-data-dir <DIR>  persistent Chrome profile directory

    --transport <T>        stdio (default) | http
    --port <N>             HTTP port (default: 8080)
```

## Install

```bash
# From crates.io
cargo install ferridriver-cli

# From GitHub releases (prebuilt binary)
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

See [MCP > Setup](/mcp/setup) for client configuration snippets.
