# `run_script` reference

`run_script` is the action path of the MCP server. It runs sandboxed
JavaScript against the live browser session — `page`, `context`,
`request`, `browser`, and standard web APIs are bound globals. One tool
call can navigate, fill forms, click, assert, and make HTTP calls in
one atomic LLM turn.

## Parameters

```jsonc
{
  "source": "await page.goto(args[0]); return await page.title();",
  "args":   ["https://example.com"],
  "timeout_ms":      30000,    // optional; default 5 minutes
  "memory_limit_mb": 256,      // optional; default 256 MiB
  "session":         "default" // optional; format "instance:context"
}
```

Pass either `source` (inline JavaScript) **or** `path` (relative path
to a `.js` / `.mjs` / `.ts` / `.tsx` / `.mts` / `.cts` file under
`script_root`) — not both. A TypeScript file, or any file with
top-level `import` / `export`, is bundled and run as an ES module
whose `default` export is the result.

`source` is wrapped in an async IIFE; use `return <value>` for the
result. Top-level `await` works.

`args` is bound to the global of the same name. It is **never**
interpolated into the source string — use this for any caller-controlled
data to keep prompt-injection out.

## Return shape

Always a structured JSON payload:

### Success

```jsonc
{
  "status": "ok",
  "value":  /* whatever the script returned, JSON-serialized */,
  "duration_ms": 42,
  "console": [
    { "level": "log",  "message": "starting", "ts_ms": 0 },
    { "level": "warn", "message": "retry attempt 2", "ts_ms": 30 }
  ]
}
```

### Failure

```jsonc
{
  "status": "error",
  "error": {
    "kind":           "runtime",
      // or: syntax | timeout | memory_limit | sandbox_violation | internal
    "message":        "Cannot read property 'click' of null",
    "stack":          "at <anonymous> (eval_script:14:21)\n...",
    "line":           14,
    "column":         21,
    "source_snippet": "12: ...\n13: ...\n14: >>> await page.click('.foo')\n15: ..."
  },
  "duration_ms": 12,
  "console": [ ... ]
}
```

Scripts that throw surface as `status: "error"` **in the payload** —
not as MCP-level errors. Callers can inspect the failure without
catching protocol exceptions.

## Globals

| Global    | What it is |
|-----------|------------|
| `page`    | Playwright-shaped `Page` — `goto`, `click`, `fill`, `hover`, `press`, `type`, `check`, `uncheck`, `selectOption`, `locator`, `getByRole` / `getByText` / `getByLabel` / `getByPlaceholder` / `getByAltText` / `getByTestId`, `waitForSelector`, `textContent`, `innerText`, `innerHTML`, `inputValue`, `getAttribute`, visibility / state predicates, `evaluate`, `title`, `url`, `content`, `setContent`, `markdown`, `screenshot`, `reload`, `goBack`, `goForward`, `close`, `isClosed` |
| `Locator` | Returned from `page.locator` / `page.getBy*`. Full set of action and query methods |
| `context` | `BrowserContext` — `cookies`, `addCookies`, `clearCookies`, `deleteCookie`, `grantPermissions`, `clearPermissions`, `setGeolocation`, `setOffline`, `setExtraHTTPHeaders`, `addInitScript`, `name`, `close` |
| `request` | `HttpClient` — `get` / `post` / `put` / `delete` / `patch` / `head` / `fetch`. Returns `HttpResponse` (`status`, `ok`, `url`, `text`, `json`, `headersArray`, `header`) |
| `browser` | Browser handle for multi-page operations |
| `args`    | Positional arguments bound to the script. Access via `args[0]`, `args[1]`. **Use this for any caller-controlled data** — bound values are safe from source-level injection |
| `vars`    | Session-scoped string store: `get` / `set` / `has` / `delete` / `keys`. Persists across `run_script` calls in the same session |
| `console` | Captured `log` / `info` / `warn` / `error` / `debug` — 1000 entries / 1 MiB total / 8 KiB per entry, ANSI-stripped, returned in the result |
| `fs`      | Scoped to `script_root`: `readFile`, `readFileBytes`, `writeFile`, `readdir`, `exists`. Absolute paths, `..`, and symlink escapes are rejected |
| `artifacts` | Dedicated output dir: `write`, `writeBytes`, `read`, `readBytes`, `list`, `exists`, `remove`. For screenshots, PDFs, traces |
| `fetch` / `Headers` / `Request` / `Response` / `AbortController` / `AbortSignal` / `Blob` / `FormData` / `ReadableStream` | Standard web APIs — see [Sandbox](/scripting/sandbox) |
| `process` | Sandbox-safe subset. `process.env` is `{}` by default; opt-in keys via `[scripting] allowEnv` |
| `expect` | Auto-retrying matchers — same as Rust `ferridriver-expect`, callable from JS |

`ES module import './foo.js'` resolves inside `script_root` with the
same sandbox rules. Bare specifiers (`import 'lodash'`) are rejected —
no `node_modules` resolution at runtime.

## Examples

### Login + extract

```js
await page.goto(args[0]);
await page.getByLabel("Email").fill(args[1]);
await page.getByLabel("Password").fill(args[2]);
await page.getByRole("button", { name: "Sign in" }).click();
await page.waitForSelector('[data-testid="dashboard"]');
return {
  title:   await page.title(),
  cookies: await context.cookies(),
};
```

Call as:

```jsonc
{
  "source": "...",
  "args": ["https://app.example.com/login", "ada@example.com", "secret"]
}
```

### Cross-call session state

```js
// First call
vars.set("auth_token", await page.evaluate(() => localStorage.token));
return "saved";
```

```js
// Later call in the same session
const token = vars.get("auth_token");
await request.get("https://api.example.com/me", {
  headers: { authorization: `Bearer ${token}` },
});
```

### Scraping with `request`

```js
const r = await request.get(args[0]);
if (!r.ok) throw new Error(`HTTP ${r.status}`);
return await r.json();
```

### Web `fetch` (WHATWG-spec)

```js
const r = await fetch("https://api.example.com/users", {
  method:  "POST",
  headers: { "content-type": "application/json" },
  body:    { name: "Ada" },   // object ⇒ JSON-encoded
});
return await r.json();
```

`fetch` shares the same HTTP core as `request` — cookies, sessions, and
any `allow.net` restriction bind both.

## Configuration

The engine defaults are fixed in the server: a 5-minute timeout, a
256 MiB memory quota, `script_root` at `./.ferridriver/scripts` (for
`path` / `fs` / imports), and `artifacts_root` at
`./.ferridriver/artifacts` (for `artifacts.*`). Override the timeout and
memory quota per call via the `timeout_ms` / `memory_limit_mb`
parameters above (each capped by the server maximum).

The one scripting knob in `ferridriver.toml` is the `process.env`
allow-list:

```toml
[scripting]
allowEnv = ["HOME", "TZ"]
```

See [Sandbox](/scripting/sandbox) for `process` / `fetch` / `fs` /
`AbortController` details and what is absent, and
[State and sessions](/scripting/state-and-sessions) for `globalThis` vs
`vars` lifetime.
