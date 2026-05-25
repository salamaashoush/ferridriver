# Tools

The MCP surface is scripting-focused: **ten tools**. `run_script` is the
primary action path — a sandboxed QuickJS runtime with live `page`,
`context`, `request`, and `browser` bindings over the ferridriver core.
The rest are cheap observation and session-bootstrap primitives.

All tools accept an optional `session` parameter (default: `"default"`).
Sessions have isolated cookies, storage, and network state.

```
session: "admin"          isolated context named "admin"
session: "staging:qa"     context "qa" on Chrome instance "staging"
```

## Recommended workflow

1. `navigate` (or `connect`) to bring up a session.
2. `snapshot` to see the accessibility tree and pick refs / selectors.
3. Act via `run_script` — one call executes many browser operations
   without LLM round-trips between them.
4. `snapshot` again to verify.

## Navigation (3)

- **`connect`** — attach to a running Chrome (debugger URL or
  `auto_discover`). Parameters: `url?`, `auto_discover?`, `channel?`
  (default `"stable"`), `user_data_dir?`.
- **`navigate`** — go to a URL; returns a fresh accessibility snapshot.
  Parameters: `url` (required), `wait_until?`
  (`commit` / `load` / `domcontentloaded` / `networkidle` / `none`;
  default `commit`).
- **`page`** — manage pages / tabs. Parameters: `action` (`back` /
  `forward` / `reload` / `new` / `close` / `select` / `list` /
  `close_browser`), `url?`, `page_index?`.

## Observation (4)

- **`snapshot`** — **primary grounding tool.** Returns the page as an
  accessibility tree with `[ref=eN]` handles, roles, and visible text.
  Always your first action before deciding on selectors. Parameters:
  `depth?` (unlimited if omitted), `track?` (incremental snapshot key —
  shows only what changed since the last snapshot with the same key).
- **`screenshot`** — PNG / JPEG / WebP base64 image. Use sparingly — much
  heavier than `snapshot`. Reach for it when the a11y tree is ambiguous
  (icons, canvas, complex layout). Parameters: `format?` (default
  `png`), `quality?` (0–100 for jpeg / webp), `full_page?`, `selector?`.
- **`evaluate`** — run a single JavaScript expression in the page and
  return its JSON-serialized value. Quick reads only; use `run_script`
  for multi-step logic.
- **`search_page`** — grep-like text search across the page (literal or
  regex) with surrounding context. Fast, token-cheap. Parameters:
  `pattern`, `regex?`, `case_sensitive?`, `context_chars?` (default
  150), `selector?`, `max_results?` (default 25).

## Diagnostics (1)

- **`diagnostics`** — session telemetry. Parameters: `type` (`console`
  / `network` / `trace_start` / `trace_stop`), `level?`, `limit?`
  (default 50).

## Scripting (1)

- **`run_script`** — the action path. Execute JavaScript in a sandboxed
  QuickJS runtime against the current session.

### `run_script` globals

| Global    | Description |
|-----------|-------------|
| `page`    | `Page` — `goto`, `click`, `fill`, `hover`, `press`, `type`, `check`, `uncheck`, `selectOption`, `locator`, `getByRole` / `getByText` / `getByLabel` / `getByPlaceholder` / `getByAltText` / `getByTestId`, `waitForSelector`, `textContent`, `innerText`, `innerHTML`, `inputValue`, `getAttribute`, visibility / state predicates, `evaluate`, `title`, `url`, `content`, `setContent`, `markdown`, `screenshot`, `reload`, `goBack`, `goForward`, `close`, `isClosed` |
| `Locator` | Returned from `page.locator` / `page.getBy*`. `click`, `dblclick`, `fill`, `type`, `press`, `hover`, `focus`, `blur`, `check`, `uncheck`, `setChecked`, `clear`, `selectOption`, `scrollIntoViewIfNeeded`, `count`, `textContent`, `innerText`, `innerHTML`, `inputValue`, `getAttribute`, visibility / state predicates, `first` / `last` / `nth` / `locator`, `allTextContents` / `allInnerTexts`, `evaluate` |
| `context` | `BrowserContext` — `cookies`, `addCookies`, `clearCookies`, `deleteCookie`, `grantPermissions`, `clearPermissions`, `setGeolocation`, `setOffline`, `setExtraHTTPHeaders`, `addInitScript`, `name`, `close` |
| `request` | `HttpClient` for runner-side HTTP — `get`, `post`, `put`, `delete`, `patch`, `head`, `fetch`. Returns `HttpResponse` with `status`, `ok`, `url`, `text`, `json`, `headersArray`, `header` |
| `browser` | Browser handle for multi-page operations |
| `args`    | Positional arguments. Bound, never interpolated into the source — use this for any caller-controlled data (prompt-injection safe) |
| `vars`    | Session-scoped string store: `get` / `set` / `has` / `delete` / `keys`. Persists across `run_script` calls in the same session |
| `console` | Captured `log` / `info` / `warn` / `error` / `debug` — 1000 entries / 1 MiB total / 8 KiB per entry, ANSI-stripped, returned in the result |
| `fs`      | Scoped I/O: `readFile`, `readFileBytes`, `writeFile`, `readdir`, `exists`. Bound to the configured `script_root`. Absolute paths, `..`, and symlink escapes are rejected |
| `artifacts` | Dedicated output directory: `write`, `writeBytes`, `read`, `readBytes`, `list`, `exists`, `remove`. For screenshots, PDFs, traces |
| `fetch`, `Headers`, `Request`, `Response`, `AbortController`, `Blob`, `FormData`, `ReadableStream` | Standard web APIs |
| `process` | Sandbox-safe subset (`platform`, `arch`, `versions`, `cwd`, `stdout`, `stderr`). `process.env` is `{}` by default; opt-in keys via `[scripting] allowEnv`. `process.exit` and friends are absent |

ES module `import './foo.js'` resolves inside `script_root` with the
same sandbox rules. Bare specifiers (`import 'lodash'`) are rejected —
no `node_modules` resolution.

### `run_script` parameters

```jsonc
{
  "source": "await page.goto(args[0]); return await page.title();",
  "args":   ["https://example.com"],
  "timeout_ms":      30000,    // optional; default 5 minutes
  "memory_limit_mb": 256,       // optional; default 256 MiB
  "session":         "default"  // optional
}
```

Pass either `source` (inline JavaScript) or `path` (relative path to a
`.js` / `.mjs` file under `script_root`) — not both.

### `run_script` return

Always a structured JSON payload:

```jsonc
// Success
{
  "status": "ok",
  "value":  /* whatever the script returned, JSON-serialized */,
  "duration_ms": 42,
  "console": [
    { "level": "log",  "message": "starting", "ts_ms": 0 },
    { "level": "warn", "message": "retry attempt 2", "ts_ms": 30 }
  ]
}

// Failure (script threw, hit timeout / memory limit, or a sandbox violation)
{
  "status": "error",
  "error": {
    "kind": "runtime",
      // or: syntax | timeout | memory_limit | sandbox_violation | internal
    "message": "Cannot read property 'click' of null",
    "stack":   "at <anonymous> (eval_script:14:21)\n...",
    "line":    14,
    "column":  21,
    "source_snippet": "12: ...\n13: ...\n14: >>> await page.click('.foo')\n15: ..."
  },
  "duration_ms": 12,
  "console": [ ... ]
}
```

Scripts that throw surface as `status: "error"` in the payload — not as
MCP-level errors — so callers can inspect the failure without catching
protocol exceptions.

## Introspection (1)

- **`ferridriver_extensions`** — list extensions loaded at server
  startup. Discover available plugin tools and audit declared
  capabilities. Parameters: `include_schema?` (default false; when
  true, returns each tool's full JSON `inputSchema`).

## Accessibility snapshots

`snapshot` returns an LLM-optimized tree. Refs are tied to that specific
snapshot — any `navigate`, `page(select)`, or DOM-mutating `run_script`
invalidates them. Re-snapshot before acting.

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

When scripting, prefer Playwright-style locators (`page.getByRole`,
`page.getByText`, `page.locator`) — they survive re-snapshots and DOM
churn, unlike raw refs.
