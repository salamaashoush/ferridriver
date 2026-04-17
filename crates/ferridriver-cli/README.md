# ferridriver-cli

Scripting-focused MCP (Model Context Protocol) server for AI-powered browser automation. `run_script` is the action path — a sandboxed QuickJS runtime with live Page / Locator / BrowserContext / APIRequestContext bindings over the ferridriver core.

Ships as the `ferridriver` binary. Supports stdio (default) and HTTP transports, and four browser backends (`cdp-pipe`, `cdp-raw`, `webkit`, `bidi`).

## Install

```bash
# From source
cargo install ferridriver-cli

# From GitHub releases
curl -fsSL https://github.com/salamaashoush/ferridriver/releases/latest/download/ferridriver-VERSION-TARGET.tar.gz | tar xz
```

## Register with an MCP client

```json
{
  "mcpServers": {
    "ferridriver": {
      "command": "ferridriver",
      "args": []
    }
  }
}
```

## CLI flags

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

## Tools (9)

### Navigation (3)

- **connect** — attach to a running Chrome (debugger URL or `auto_discover`)
- **navigate** — go to URL; returns a fresh accessibility snapshot
- **page** — manage pages / tabs (`back`, `forward`, `reload`, `new`, `close`, `select`, `list`, `close_browser`)

### Observation (4)

- **snapshot** — primary grounding tool: accessibility tree with `[ref=eN]` handles, roles, visible text. Always your first action before deciding on selectors
- **screenshot** — PNG / JPEG / WebP base64 image. Use sparingly — heavier than `snapshot`
- **evaluate** — single JavaScript expression in the page; returns JSON-serialized value
- **search_page** — grep the page's rendered text (literal or regex) with surrounding context

### Diagnostics (1)

- **diagnostics** — session telemetry: console messages, network requests, performance metrics

### Scripting (1)

- **run_script** — sandboxed QuickJS runtime with Page, Locator, BrowserContext, APIRequestContext bindings. See below

## `run_script`

One tool call executes many browser operations without LLM round-trips between them.

```js
// source
await page.goto(args[0]);
await page.getByLabel('Email').fill(args[1]);
await page.getByLabel('Password').fill(args[2]);
await page.getByRole('button', { name: 'Sign in' }).click();
await page.waitForSelector('[data-testid="dashboard"]');
return { title: await page.title(), cookies: await context.cookies() };
```

**Globals**

| Name | Purpose |
|---|---|
| `page` | Playwright-shaped Page: `goto`, `click`, `fill`, `hover`, `press`, `type`, `check`, `uncheck`, `selectOption`, `locator`, `getByRole`/`getByText`/`getByLabel`/`getByPlaceholder`/`getByAltText`/`getByTestId`, `waitForSelector`, `textContent`, `innerText`, `innerHTML`, `inputValue`, `getAttribute`, `isVisible`/`isHidden`/`isEnabled`/`isDisabled`/`isChecked`, `evaluate`, `title`, `url`, `content`, `setContent`, `markdown`, `screenshot`, `reload`, `goBack`, `goForward`, `close`, `isClosed` |
| `Locator` | Returned from `page.locator` / `page.getBy*`. `click`, `dblclick`, `fill`, `type`, `press`, `hover`, `focus`, `blur`, `check`, `uncheck`, `setChecked`, `clear`, `selectOption`, `scrollIntoViewIfNeeded`, `count`, `textContent`, `innerText`, `innerHTML`, `inputValue`, `getAttribute`, visibility/state predicates, `first`/`last`/`nth`/`locator`, `allTextContents`/`allInnerTexts`, `evaluate` |
| `context` | BrowserContext: `cookies`, `addCookies`, `clearCookies`, `deleteCookie`, `grantPermissions`, `clearPermissions`, `setGeolocation`, `setOffline`, `setExtraHTTPHeaders`, `addInitScript`, `close` |
| `request` | APIRequestContext for runner-side HTTP: `get`, `post`, `put`, `delete`, `patch`, `head`, `fetch`. Returns `APIResponse` with `status`, `ok`, `url`, `text`, `json`, `headersArray`, `header` |
| `args` | Positional arguments bound to the script. Access via `args[0]`, `args[1]`. **Use this for any caller-controlled data** — bound values are safe from source-level injection |
| `vars` | Session-level string store: `get`/`set`/`has`/`delete`/`keys`. Persists across `run_script` calls with the same session |
| `console` | `log`/`info`/`warn`/`error`/`debug` — captured with size limits (1000 entries / 1 MiB / 8 KiB per entry), ANSI-stripped, returned in the result |
| `fs` | Scoped I/O: `readFile`, `readFileBytes`, `writeFile`, `readdir`, `exists`. Bound to `script_root`; absolute paths, `..`, and symlink escapes are rejected |

ES module `import './foo.js'` resolves inside `script_root` with the same sandbox rules.

**Parameters**

```jsonc
{
  "source":           "await page.goto(args[0]); return await page.title();",
  "args":             ["https://example.com"],  // bound array, accessed via args[0]
  "timeout_ms":       30000,                     // optional; default 5 min
  "memory_limit_mb":  256,                        // optional; default 256 MiB
  "session":          "default"                   // optional
}
```

**Return**

```jsonc
{
  "status": "ok" | "error",
  "value":  /* JSON-serialized script return, on ok */,
  "error": {
    "kind":           "runtime" | "syntax" | "timeout" | "memory_limit" | "sandbox_violation" | "internal",
    "message":        "Cannot read property 'click' of null",
    "stack":          "...",
    "line":           14,
    "column":         21,
    "source_snippet": "12: ...\n13: ...\n14: >>> await page.click('.foo')\n15: ..."
  },
  "duration_ms": 42,
  "console": [
    { "level": "log", "message": "...", "ts_ms": 0 }
  ]
}
```

Script errors surface as `status: "error"` in the payload, not as MCP-level errors — callers can inspect the failure without catching protocol exceptions.

## Sessions

All tools accept an optional `session` parameter (default: `"default"`). Sessions have isolated cookies, localStorage, and network state. Use for multi-user testing or parallel automation.

```
session: "admin"        isolated context named "admin"
session: "staging:qa"   context "qa" on Chrome instance "staging"
```

Session-scoped `vars` persist across `run_script` calls with the same session; fresh QuickJS context per call means no JS state leaks between runs.

## Accessibility snapshots

`snapshot` returns an LLM-optimized tree:

```
### Page
- URL: https://example.com
- Title: Example Domain

### Snapshot
- heading "Example Domain" [ref=e1] [level=1]
- paragraph [ref=e2]
- paragraph [ref=e3]
  - link "Learn more" [ref=e4] [url=https://iana.org/domains/example]
```

Refs are tied to that specific snapshot — any `navigate`, `page(select)`, or DOM-mutating `run_script` invalidates them. Re-snapshot before acting. When scripting, prefer Playwright-style locators (`page.getByRole`, `page.getByText`, `page.locator`) — they survive re-snapshots.

## Running

```bash
# stdio transport (default) for Claude Desktop / Cursor / Claude Code
ferridriver

# HTTP transport for remote clients
ferridriver --transport http --port 8080

# WebKit backend (macOS, no Chrome needed)
ferridriver --backend webkit

# Firefox over BiDi
ferridriver --backend bidi

# Attach to a running Chrome
ferridriver --auto-connect my-channel
ferridriver --connect ws://localhost:9222/devtools/browser/...
```

## Building

```bash
cargo build --release -p ferridriver-cli
```
