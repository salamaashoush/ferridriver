# Sandbox

QuickJS by itself has no Node builtins. ferridriver adds a curated,
sandbox-safe subset so real npm packages and modern web code work. This
page lists what is present, what is not, and what is gated.

## `process`

Always available (no authority, real values):

| Member                       | Notes |
|------------------------------|-------|
| `platform`, `arch`, `version`, `versions`, `release` | Real host values. `process.versions.node` is **never present** — `versions` is `{ ferridriver, quickjs }` only. |
| `argv`                       | `["ferridriver", "script"]` |
| `pid`                        | Real PID |
| `nextTick(cb)`               | FIFO microtask (via `queueMicrotask`) — not Node's separate higher-priority queue |
| `hrtime()` / `hrtime.bigint()` | Returns `[seconds, nanos]` / `bigint` ns |
| `stdout` / `stderr`          | `.write(chunk)` routes into the captured console (`stdout`→`log`, `stderr`→`error`). One trailing newline trimmed. Returns `true`. `isTTY` is `false`. |
| `cwd()`                      | Returns the sandbox root, never the real cwd |
| `env`                        | **Defaults to `{}`.** Names listed in `[scripting] allowEnv` and present in the server's environment appear, frozen. A name you didn't list is simply absent — there is no way to read an unlisted variable. |

### Absent on purpose

- `process.exit` — throws (a script must never kill the server)
- `process.binding`, `process.dlopen`, `process.kill`, `process.chdir`, `process.setuid`, …
- Anything that would grant ambient authority not declared in the
  capability manifest.

## `fetch`

Web-standard `fetch(input, init?)` with the WHATWG globals `Headers`,
`Request`, `Response`, `AbortController`, `AbortSignal`, `Blob`,
`FormData`, `ReadableStream` — constructible; `instanceof` works.

```ts
const r = await fetch("https://api.example.com/x", {
  method:  "POST",
  headers: { "content-type": "application/json" },
  body:    { hello: "world" },   // object ⇒ JSON; string ⇒ sent as-is
  signal:  AbortSignal.timeout(5000),
});
if (!r.ok) throw new Error(`HTTP ${r.status}`);
const data = await r.json();
```

### `Headers`

Spec-compliant: case-insensitive, `, `-combined, `set-cookie` separate
+ `getSetCookie()`, real iterators, `forEach`.

### `Request`

`new Request(url|Request, init?)` carries `url` / `method` / `headers` /
`redirect` / `credentials` / `bodyUsed`. Accepted by `fetch`.

Known subset: a `signal` set on a `Request` instance is not yet
forwarded — pass it via `init.signal` instead.

### `Response`

`status` / `ok` / `statusText` / `url` / `redirected` / `type` /
`bodyUsed` / `headers`. Single-use `text()` / `json()` /
`arrayBuffer()`. `clone()`. Static `Response.json()` / `error()` /
`redirect()`.

Known subset: `clone()` of a not-yet-read **streamed** `Response`
throws (no stream tee yet).

### `Response.body` — streaming

`Response.body` is a `ReadableStream` that pulls chunks **live off the
socket**. A large or streamed body is not fully buffered.

```ts
const reader = res.body.getReader();
while (true) {
  const { value, done } = await reader.read();
  if (done) break;
  // value: Uint8Array
}

// Or async-iterate
for await (const chunk of res.body) {
  // chunk: Uint8Array
}
```

`text()` / `json()` / `arrayBuffer()` drain it on demand.

`new ReadableStream({ start(c) { c.enqueue(x); c.close(); } })` works.

Known subset: no `pull`, no `tee`, no BYOB readers.

### `AbortController` / `AbortSignal`

Standard. `controller.abort(reason?)`, `signal.aborted`,
`signal.reason`, `signal.throwIfAborted()`, `signal.onabort`,
`signal.addEventListener("abort", ...)`, `AbortSignal.abort()` /
`AbortSignal.timeout(ms)` / `AbortSignal.any([...])`.

`fetch(url, { signal })` rejects an already-aborted call before any I/O
and cancels an in-flight request.

### `Blob`, `FormData`

```ts
new Blob([uint8, string], { type: "application/octet-stream" });
// .size / .type / .text() / .arrayBuffer() / .bytes() / .slice() / .stream()

const fd = new FormData();
fd.append("file", new Blob([bytes], { type: "image/png" }), "logo.png");
fd.set("name", "ada");
await fetch(url, { method: "POST", body: fd });
// Sent as multipart/form-data
```

Both accepted as `fetch` bodies. `Blob` sends its bytes + type;
`FormData` is sent as `multipart/form-data`.

Known subset: `FormData` iteration is via `entries()` / `forEach`
returning arrays (not native iterators).

### One HTTP core

`fetch` runs on the **same HTTP core as the `request` global** — so
cookies / sessions are shared and any
[`allow.net`](/scripting/capabilities#allow-net) restriction on a
tool's `request` binds `fetch` the same way (no second stack, no
bypass).

`request` (the Playwright-style API) stays; `fetch` is the standard
entry point. The Playwright page-network `Request` / `Response` (from
`page.on(...)`, `route`, navigation) are unchanged but are **not**
global constructors — the bare `Request` / `Response` globals are the
fetch classes.

## `fs`

Node's `fs`, as a global — the same object `import fs from "node:fs"`
answers with, sync entry points plus `fs.promises`:

```ts
const text = await fs.promises.readFile("input.txt", "utf8");
const bytes = await fs.promises.readFile("photo.png");
await fs.promises.writeFile("out.txt", "hello");
const entries = await fs.promises.readdir(".");
const exists = fs.existsSync("config.json");
```

A relative path resolves against the process working directory and an
absolute path is used as written, as in Node. There is no path
confinement: the paths a suite handles are produced by the runner
(`testInfo.outputPath()`, `testInfo.snapshotPath()`, `download.path()`),
they are absolute, and a prefix check on one module was never a boundary
while the same script reaches the network, the browser and `commands`.

Not served: the callback API (`fs.readFile(path, cb)`) and streams. Use
the promise or sync form.

## `artifacts`

Dedicated output directory (`artifacts_root` from MCP config), for
results the agent wants to hand back to the caller:

```ts
await artifacts.write("dashboard.html", "<html>...</html>");
await artifacts.writeBytes("screenshot.png", await page.screenshot());
const items = await artifacts.list();
await artifacts.remove("old.html");
```

A relative name is anchored at the artifacts root; an absolute path is
used as written.

## `require.resolve`

Node's, answered relative to the file that wrote the call — including a
file that was bundled into a larger module, because the position is
mapped back through the bundle's source map:

```ts
require.resolve("./reporter");        // absolute path, extension appended
require.resolve("@acme/kit/helper");  // walks node_modules upward
require.resolve("fs");                // a builtin answers with itself
```

Throws `Cannot find module '<specifier>' from <dir>` when nothing
resolves, as Node does. Absent: the `{ paths }` option and
`require.resolve.paths()` — both describe a module search path this
runtime does not have.

## What is absent

- `module`, `__dirname`, `__filename` — no CommonJS at runtime (rolldown
  handles `require` calls at bundle time; `require` itself serves the
  native specifiers, and `require.resolve` answers paths).
- Node's `child_process`, `cluster`, `http`, `https`, `net`, `tls`,
  `dgram`, `dns`, `vm`, `worker_threads`, `crypto.createServer`, …
- Browser DOM globals (`window`, `document`, `localStorage`, …) — these
  exist **inside `page.evaluate`**, not in script scope.

## See also

- [Capabilities](/scripting/capabilities) — `allow.commands`, `allow.net`
- [State and sessions](/scripting/state-and-sessions) — `globalThis`,
  `vars`, session lifetime
- [`run_script` reference](/scripting/run-script) — globals and return shape
