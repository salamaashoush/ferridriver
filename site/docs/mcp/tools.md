# Tools

The MCP surface is scripting-focused: **9 tools**. `run_script` is the primary action path — a sandboxed QuickJS runtime with live `page`, `context`, and `request` bindings over the ferridriver core. The rest are cheap observation and session-bootstrap primitives.

All tools accept an optional `session` parameter (default: `"default"`). Sessions have isolated cookies, storage, and network state.

```
session: "admin"        isolated context named "admin"
session: "staging:qa"   context "qa" on Chrome instance "staging"
```

## Recommended workflow

1. `navigate` (or `connect`) to bring up a session.
2. `snapshot` to see the accessibility tree and pick refs / selectors.
3. Act via `run_script` — one call executes many browser operations without LLM round-trips between them.
4. `snapshot` again to verify.

## Navigation (3)

- **connect** — attach to a running Chrome (debugger URL or `auto_discover`)
- **navigate** — go to URL; returns a fresh accessibility snapshot
- **page** — manage pages / tabs (`back`, `forward`, `reload`, `new`, `close`, `select`, `list`, `close_browser`)

## Observation (4)

- **snapshot** — PRIMARY grounding tool. Returns the page as an accessibility tree with `[ref=eN]` handles, roles, and visible text. Always your first action before deciding on selectors. Supports depth limits and incremental tracking (shows only what changed since the last snapshot).
- **screenshot** — PNG / JPEG / WebP base64 image. Use sparingly — much heavier than `snapshot`. Reach for it only when the a11y tree is ambiguous (icons, canvas, complex layout).
- **evaluate** — run a single JavaScript expression IN the page and return its JSON-serialized value. Use for quick reads like `document.title`; use `run_script` for multi-step logic.
- **search_page** — grep-like text search across the page (literal or regex) with surrounding context. Fast, token-cheap.

## Diagnostics (1)

- **diagnostics** — session telemetry: console messages, network requests, performance metrics

## Scripting (1)

- **run_script** — the action path. Execute JavaScript in a sandboxed QuickJS runtime against the current session.

### `run_script` globals

| Global | Description |
|---|---|
| `page` | `ferridriver::Page` wrapper — `goto`, `click`, `fill`, `hover`, `press`, `type`, `check`, `uncheck`, `selectOption`, `locator`, `getByRole`/`getByText`/`getByLabel`/`getByPlaceholder`/`getByAltText`/`getByTestId`, `waitForSelector`, `textContent`, `innerText`, `innerHTML`, `inputValue`, `getAttribute`, `isVisible`/`isHidden`/`isEnabled`/`isDisabled`/`isChecked`, `evaluate`, `title`, `url`, `content`, `setContent`, `markdown`, `screenshot`, `reload`, `goBack`, `goForward`, `close`, `isClosed` |
| `Locator` (returned from `page.locator/getBy*`) | `click`, `dblclick`, `fill`, `type`, `press`, `hover`, `focus`, `blur`, `check`, `uncheck`, `setChecked`, `clear`, `selectOption`, `scrollIntoViewIfNeeded`, `count`, `textContent`, `innerText`, `innerHTML`, `inputValue`, `getAttribute`, `isVisible`/`isHidden`/`isEnabled`/`isDisabled`/`isChecked`/`isEditable`/`isAttached`, `first`, `last`, `nth`, `locator`, `allTextContents`, `allInnerTexts`, `evaluate` |
| `context` | `BrowserContext` — `cookies`, `addCookies`, `clearCookies`, `deleteCookie`, `grantPermissions`, `clearPermissions`, `setGeolocation`, `setOffline`, `setExtraHTTPHeaders`, `addInitScript`, `name`, `close` |
| `request` | `APIRequestContext` — runner-side HTTP: `get`, `post`, `put`, `delete`, `patch`, `head`, `fetch`. Returns `APIResponse` with `status`, `ok`, `url`, `text`, `json`, `headersArray`, `header` |
| `args` | Positional arguments bound to the script, never interpolated into source. Access via `args[0]`, `args[1]`. **Use this for any caller-controlled data — interpolating into the `source` string defeats the prompt-injection defense.** |
| `vars` | Session-level string store. `vars.get(name)`, `vars.set(name, value)`, `vars.has(name)`, `vars.delete(name)`, `vars.keys()`. Persists across `run_script` calls with the same session. |
| `console` | Captured `log`/`info`/`warn`/`error`/`debug`. Size-limited (1000 entries / 1 MiB / 8 KiB per entry), ANSI-stripped. Returned in the script result. |
| `fs` | Scoped file I/O: `readFile`, `readFileBytes`, `writeFile`, `readdir`, `exists`. Bound to the configured `script_root`. Absolute paths, `..`, and symlink escapes are rejected. |

ES module `import './foo.js'` resolves inside `script_root` with the same sandbox rules. Bare specifiers (`import 'lodash'`) are rejected — no node_modules resolution.

### `run_script` parameters

```jsonc
{
  "source": "await page.goto(args[0]); return await page.title();",
  "args": ["https://example.com"],
  "timeout_ms": 30000,         // optional; default 5 min
  "memory_limit_mb": 256,       // optional; default 256 MiB
  "session": "default"          // optional; default 'default'
}
```

### `run_script` return

Always a structured JSON payload:

```jsonc
// Success
{
  "status": "ok",
  "value": /* whatever the script returned, JSON-serialized */,
  "duration_ms": 42,
  "console": [
    { "level": "log", "message": "starting", "ts_ms": 0 },
    { "level": "warn", "message": "retry attempt 2", "ts_ms": 30 }
  ]
}

// Failure (script threw, hit timeout, hit memory limit, or a sandbox violation)
{
  "status": "error",
  "error": {
    "kind": "runtime", // or "syntax", "timeout", "memory_limit", "sandbox_violation", "internal"
    "message": "Cannot read property 'click' of null",
    "stack": "at <anonymous> (eval_script:14:21)\n...",
    "line": 14,
    "column": 21,
    "source_snippet": "12: ...\n13: ...\n14: >>> await page.click('.foo')\n15: ..."
  },
  "duration_ms": 12,
  "console": [ ... ]
}
```

Scripts that throw surface as `status: "error"` in the payload — not as MCP-level errors — so callers can inspect the failure without catching protocol exceptions.

## Accessibility snapshots

`snapshot` returns an LLM-optimized tree. Refs are tied to that specific snapshot — any `navigate`, `page(select)`, or DOM-mutating `run_script` invalidates them. Re-snapshot before acting.

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

When scripting, prefer Playwright-style locators (`page.getByRole`, `page.getByText`, `page.locator`) — they survive re-snapshots and DOM churn, unlike raw refs.
