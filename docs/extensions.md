# Extensions

An **extension** is a single JavaScript or TypeScript file that contributes
to ferridriver at runtime. One file can contribute to three hosts:

- **MCP server** (`ferridriver mcp`) — registers tools via `defineTool(...)`.
- **BDD test runner** (`ferridriver bdd`) — registers Cucumber step
  definitions, hooks, and parameter types via `Given`/`When`/`Then`/
  `Before`/`After`/`defineParameterType`/...
- **Ad-hoc scripts** (`ferridriver run`, MCP `run_script`) — the same VM
  bindings the above two use.

The same file can serve all three. It branches on the `ferridriver.host`
global to decide what to contribute where.

> Companion document: `docs/plugin-architecture.md` records *why* the
> system is shaped this way (the comparison against VS Code / Deno / WASM /
> Rollup and the decisions deferred). This document is the *how*: the
> authoring contract and reference.

---

## Mental model

```
extension.ts ──► rolldown bundle (TS + node_modules + tree-shake)
             ──► QuickJS bytecode (compiled ONCE at startup)
             ──► content-hash cache (in-memory, process-local)
             ──► Module::load per session VM (no re-parse)
             ──► top-level defineTool()/Given() run → Rust ExtensionRegistry
```

Registration functions (`defineTool`, `Given`, `Before`, ...) are native
Rust functions, not JS shims. Calling them at the top level of your module
pushes an entry into a Rust-owned registry. Hosts then read back the kinds
they care about and invoke your handler natively — the MCP tool path and
the BDD step path use the exact same dispatch mechanism.

Implication: **all contribution happens as a side effect of the module's
top-level code running once.** There is no `activate()` / `onLoad()`
lifecycle hook — ES module top-level *is* your load hook.

---

## Detecting the host

`ferridriver.host` is a string set once per session: `"mcp"`, `"bdd"`, or
`"script"`. Use it so one file can ship a tool and its matching step
without registering the wrong thing in the wrong host:

```ts
if (ferridriver.host === "mcp") {
  defineTool({
    name: "box.login",
    description: "Log a test user in and return the session cookie",
    inputSchema: { type: "object", properties: { user: { type: "string" } }, required: ["user"] },
    allow: { net: ["*.box.com"] },
    handler: async ({ args, request }) => {
      const res = await request.post("https://api.box.com/login", { data: { user: args.user } });
      return { cookie: (await res.json()).cookie };
    },
  });
}

if (ferridriver.host === "bdd") {
  Given("I am logged in as {string}", async function (user: string) {
    await this.page.goto(`https://app.box.com/login?u=${user}`);
  });
}
```

Registering for the wrong host is harmless (the host ignores kinds it does
not consume) but wastes work and muddies intent — gate it.

---

## Authoring MCP tools

### `defineTool`

Two equivalent forms:

```ts
// Inline handler on the manifest object:
defineTool({
  name: "string",              // required, globally unique, dot-namespaced by convention
  description: "string",       // optional, surfaced in tools/list
  inputSchema: { ... },        // optional JSON Schema, surfaced in tools/list AND enforced
  exposeAsTool: true,          // optional, default false (see below)
  timeoutMs: 30000,            // optional per-invocation handler timeout (ms)
  allow: { ... },              // optional capability manifest (see below)
  handler: async (ctx) => { ... },
});

// Or manifest + separate handler:
defineTool(manifest, async (ctx) => { ... });
```

### `exposeAsTool`

- `false` (default): the tool is callable from other extension/script code
  as `await plugins["name"](args)`, but is **not** advertised in the MCP
  server's `tools/list`. Use for shared helpers.
- `true`: additionally promoted to a first-class MCP tool. `name`,
  `description`, and `inputSchema` become the tool's contract. The tool
  call and the `plugins[...]` binding route through the same handler.

### Handler context

The handler receives one object:

| Field      | Type                  | Notes |
|------------|-----------------------|-------|
| `args`     | the caller's argument | For a promoted tool, the MCP `arguments` object. |
| `page`     | `Page` \| undefined   | The live browser page for the session. |
| `context`  | `BrowserContext` \| undefined | The session's browser context. |
| `request`  | `HttpClient` \| undefined | HTTP client. Net-restricted if `allow.net` is non-empty. |
| `commands` | `PluginCommands`      | `.run(name, vars?)` — runs a declared shell template. |

Return any JSON-serialisable value; it becomes the tool result.

> When the manifest declares `inputSchema`, the caller's `args` are
> validated against it (full JSON Schema, via the `jsonschema` crate)
> **before** the handler runs; a non-conforming call is rejected as a
> tool error and the handler is never entered. You still get the parsed
> value as `args` — validation does not coerce, only gate.

---

## Capabilities

`allow` is a declarative, default-deny capability manifest, enforced in
Rust at the binding boundary. The handler source alone cannot grant itself
authority it did not declare.

### `allow.commands` (alias: `allow.exec`)

A name → command map. The handler may only run commands it declared
(default-deny). Each value is a **shorthand string** (a `sh -c` line) or
a **spec object**:

```ts
defineTool({
  name: "git.sha",
  allow: {
    commands: {
      // shorthand: a shell line
      headSha: "git -C ${repo} rev-parse HEAD",
      // spec object: no shell, explicit policy
      clone: {
        run: ["git", "clone", "${url}", "${dest}"], // argv array → no shell
        timeoutMs: 60000,
        env: ["SSH_AUTH_SOCK"],   // else the child env is scrubbed
        cwd: "/tmp",
        output: "text",           // "text" | "json" | "lines"
      },
    },
  },
  handler: async ({ commands }) => {
    const sha = await commands.run("headSha", { repo: "/srv/app" });
    return { sha: sha.trim() };
  },
});
```

Spec fields (all optional except `run`): `run` (string ⇒ `sh -c`;
array ⇒ direct exec, no shell), `timeoutMs`, `env` (server env names to
pass through — otherwise only `PATH` is kept), `cwd`, `output`,
`persistent`.

One-shot semantics (`commands.run(name, vars?)`):

- An undeclared `name` throws. Output past 8 MiB, non-zero exit, or
  timeout throws (the whole process group is killed on timeout).
- `${name}` is **strictly** substituted: every placeholder must be a
  supplied value and every value must be a string/number/boolean. A
  missing placeholder or an object/array value throws — no silent empty.
- Shell form single-quote-escapes each value; **argv form does not need
  to** — values are passed as literal arguments, so shell metacharacters
  in them are inert. Prefer argv unless you actually need a pipeline.
- `output` shapes stdout: `text` (trimmed string, default — no
  guessing), `json` (parsed; invalid JSON throws), `lines` (array of
  non-empty trimmed lines).

**Trust boundary.** A shell-form `run` line is author-supplied code with
the server process's authority (`$(…)`, `&&`, `|`, redirection live);
only the `${values}` are escaped. Argv form removes the shell entirely.
Never write a shell line that re-evaluates a value (`sh -c "${x}"`,
`eval ${x}`) — that defeats the escaping. Template = trusted code you
commit; values = untrusted data.

### Persistent commands (servers, watchers)

Declare `persistent: true` for a long-running process. It is managed
with a different verb set and its lifetime is the **session's**, not the
call's:

```ts
allow: { commands: { dev: { run: "npm run dev", persistent: true } } }
// ...
await commands.start("dev");          // { name, pid }; idempotent if up
const s = await commands.status("dev"); // { running, pid, exitCode, uptimeMs, stdout, stderr }
await commands.stop("dev");           // SIGKILLs the process group
```

- `run` on a `persistent` spec (or `start`/`status`/`stop` on a one-shot
  spec) throws — the kinds don't mix.
- The process **survives a script-VM rebuild** (timeout/OOM/browser
  relaunch) so a dev server keeps running across calls. It is killed
  when the session ends (idle-TTL reap, explicit close, server
  shutdown), on `stop`, or if it exits on its own.
- `status` returns the last ~64 KiB of stdout/stderr (a ring buffer — a
  chatty server won't grow memory unbounded). Max 16 persistent
  processes per session.

### `allow.net`

A host allow-list scoping the handler's HTTP — both the `request` client
and the global `fetch` (they share one core, so the list binds both).

- Empty / absent: HTTP is unrestricted (back-compat default).
- Non-empty: the tool's `request` binding and `fetch` both flip to
  **default-deny**. Each entry is an exact host (`api.box.com`) or a
  leading-wildcard suffix (`*.box.com`, which also matches the bare apex
  `box.com`). Any other host throws before the request is made. The
  policy follows the running handler: a tool calling another tool, or
  two tools running concurrently, each see only their own declared list.

`allow.net` scopes HTTP (`request` + `fetch`) **only**. `page`/`context`
browser navigation is a separate, deliberately ungated authority — an
automation tool must be able to navigate. There is no `fs` capability:
the handler context exposes no filesystem handle, so an `fs` scope would
gate nothing.

---

## Authoring BDD steps

Cucumber-js-shaped surface, native-backed:

```ts
Given("a user {string}", async function (name: string) { /* ... */ });
When("they click {word}", async function (sel: string) { /* ... */ });
Then("the title is {string}", async function (expected: string) { /* ... */ });

defineStep("...");          // keyword-agnostic; And/But also map here
Before(async function () { /* ... */ });
Before("@tag", async function () { /* ... */ });          // tag-filtered
After(async function (s) {
  if (s.result.status === "FAILED") this.attach(await this.page.screenshot(), "image/png");
});
BeforeAll(async () => { /* ... */ });   AfterAll(async () => { /* ... */ });

defineParameterType({ name: "color", regexp: "red|green|blue", transformer: (s) => s.toUpperCase() });

setDefaultTimeout(10000);                 // ms; per-registry default
setWorldConstructor(class { /* ... */ }); // custom World (last call wins, per VM)
setDefinitionFunctionWrapper((fn) => fn); // wrap every step body (retry/trace)
```

Per-step / per-hook timeout via the options bag:

```ts
Given("slow thing", { timeout: 30000 }, async function () { /* ... */ });
Before({ timeout: 2000 }, async function () { /* ... */ });
```

The step `this` is the per-scenario **World**. Fixtures are installed on
it: `this.page`, `this.context`, `this.request`, `this.browser`, plus
`this.parameters` (Cucumber `--world-parameters`), `this.attach`,
`this.log`, `this.skip()`. A custom `setWorldConstructor` is invoked as
`new World({ parameters })`; fixtures are augmented onto the instance.

Step bodies return:

- (nothing) / resolved promise → **passed**
- string `"pending"` → **pending**
- string `"skipped"` or `this.skip()` → **skipped**
- throw → **failed** (error remapped to the original `.ts`/`.js` location
  via the rolldown source map, including the stack)

`setParallelCanAssign` is accepted but inert: ferridriver parallelises at
the test-runner worker level (one VM per worker), not cucumber-js's
per-pickle scheduler.

> There is also a **built-in Rust step library** (`ferridriver-bdd/src/
> steps/*`, registered via `#[given]`/`#[when]`/inventory). That is the
> shipped step vocabulary, not the user extension surface — it is not
> loaded from your `.ts` files and is out of scope for this document.

---

## Discovery and configuration

Extensions are configured in the unified config file
(`ferridriver.toml`/`.yaml`/`.json`), top-level (both hosts load it):

```toml
# Files or directories. A directory is scanned RECURSIVELY for any
# source file (.js .cjs .mjs .jsx .ts .cts .mts .tsx). Used by the MCP
# server (tools) AND, bundled alongside BDD step files, by the test
# runner (steps).
extensions = ["./extensions", "./tools/box-login.ts"]

[scripting]
# Sandbox relaxations — default-deny, like allow.net.
# Names a script may read via process.env (intersected with the real
# environment; absent names stay absent — never invented). Empty ⇒
# process.env is {}.
allowEnv = ["HOME", "TZ"]
# Expose a Node-ish process.versions.node so npm packages that hard-gate
# on it run. A documented compatibility shim; off (honest) by default.
nodeCompat = false

[test]
# JS/TS step-definition globs. Defaults to steps/**/*.{js,ts} and
# step_definitions/**/*.{js,ts} when empty.
steps = ["features/steps/**/*.ts"]
```

The `ferridriver bdd` runner bundles discovered step files **and** the
configured `extensions` into one module, so an extension's `Given/When/
Then` are available to tests exactly like a step file's.

Both discovery paths (MCP plugin loader and BDD runner) share one
accepted-extension set and one recursive walk, so a `.tsx`/`.cts`
extension is visible identically to both hosts.

---

## Node-ish APIs: `process` and `fetch`

So real npm packages run, scripts and handlers get a sandbox-safe
`process` and a standard `fetch`.

### `process`

Always available (no authority, real values): `platform`, `arch`,
`version`, `versions`, `release`, `argv` (`["ferridriver","script"]`),
`pid`, `nextTick`, `hrtime`, `cwd()` (returns the sandbox root, never
the real cwd).

- `process.env` — **default `{}`**. Only the names in `[scripting]`
  `allowEnv`, and only if set in the server's environment, appear; the
  object is frozen. A name you didn't list is simply absent — there is
  no way for a script to read an unlisted variable.
- `process.exit()` — throws (a script must never kill the server).
- `process.binding`/`dlopen`/`kill`/`chdir`/`setuid`/… — not present.
- `process.versions.node` — absent unless `nodeCompat = true`, which
  sets a clearly-non-real value (`…-ferridriver-compat`). Use only for
  packages that hard-check it.

### `fetch`

Web-standard `fetch(input, init?)` with the WHATWG globals `Headers`,
`Request`, and `Response` (constructible; `instanceof` works):

```ts
const r = await fetch("https://api.example.com/x", {
  method: "POST",
  headers: { "content-type": "application/json" },
  body: { hello: "world" },        // object ⇒ JSON; string ⇒ sent as-is
});
if (!r.ok) throw new Error(`HTTP ${r.status}`);
const data = await r.json();
```

`Headers` follows the spec (case-insensitive, `, `-combined,
`set-cookie` separate + `getSetCookie()`, real iterators, `forEach`).
`Response` has `status`/`ok`/`statusText`/`url`/`redirected`/`type`/
`bodyUsed`/`headers`, single-use `text()`/`json()`/`arrayBuffer()`,
`clone()`, and static `Response.json()`/`error()`/`redirect()`.
`Request` (`new Request(url|Request, init?)`) carries
`url`/`method`/`headers`/`redirect`/`credentials`/`bodyUsed` and is
accepted by `fetch`. `AbortController`/`AbortSignal` are standard
(`controller.abort(reason?)`, `signal.aborted`/`reason`/
`throwIfAborted()`/`onabort`/`addEventListener('abort')`,
`AbortSignal.abort/timeout/any`); `fetch(url, { signal })` rejects an
already-aborted call before I/O and cancels an in-flight request.
`Response.body` is a `ReadableStream` that pulls chunks **live off the
socket** — a large/streamed body is not fully buffered;
`getReader().read()` -> `{value:Uint8Array,done}`,
`for await (const chunk of res.body)`, `cancel()`, `locked`;
`text()`/`json()`/`arrayBuffer()` drain it on demand. `new
ReadableStream({ start(c){ c.enqueue(x); c.close() } })` works too.
`Blob` (`new Blob(parts, {type})`, `size`/`type`/`text()`/
`arrayBuffer()`/`bytes()`/`slice()`/`stream()`) and `FormData`
(`append`/`set`/`get`/`getAll`/`has`/`delete`/`keys`/`values`/
`entries`/`forEach`) are accepted as `fetch` bodies — a `Blob` sends
its bytes + type, a `FormData` is sent as `multipart/form-data`.
Subset, for now: `clone()` of a not-yet-read streamed `Response`
throws (no stream tee), no `ReadableStream` `pull`/`tee`/BYOB,
`FormData` iteration is via `entries()`/`forEach` (arrays), and a
`signal` set on a `Request` instance is not yet forwarded (pass it
through `init.signal`).

The Playwright page-network `Request`/`Response` (from `page.on(...)`,
`route`, navigation) are unchanged but are not global constructors
(matching Playwright, which never globalised them) — the bare
`Request`/`Response` globals are the fetch classes.

It runs on the **same HTTP core as `request`** — so cookies/session are
shared and any `allow.net` restriction on a tool's `request` applies to
`fetch` the same way (no second stack, no bypass). `request` (the
Playwright-style API) stays; `fetch` is the standard entry point.

---

## The compile pipeline

1. **Discover** files (config + globs).
2. **Bundle** each with rolldown (oxc): resolves the whole import graph
   including `node_modules`, transpiles TS, tree-shakes, emits one ESM
   chunk with a hidden source map. Cache-miss bundles run concurrently.
3. **Compile** the chunk to QuickJS bytecode once, in a single throwaway
   runtime shared by the whole batch.
4. **Cache** bytecode + extracted manifests keyed by
   `hash(canonical path + file bytes)`. Unchanged files skip bundle +
   compile entirely on reload.
5. **Load** the bytecode into each session VM with `Module::load` — no
   re-parse, no resolver (imports are already inlined).

Consequences worth knowing as an author:

- **Imports work.** `import './helpers.ts'`, `import pkg from 'some-dep'` —
  all bundled and tree-shaken. No Node/Bun in the run path; QuickJS has no
  Node builtins (rolldown `platform: neutral`).
- **The bytecode cache is in-memory and process-local**, never written to
  disk — a requirement of the `unsafe Module::load` invariant (bytecode is
  interpreter-build- and process-specific). Restarting the server
  rebuilds it.
- **One bad file does not abort the batch.** Bundle/compile/manifest
  failures are reported per file and skipped; the server still starts.
- **Errors are source-mapped.** A thrown error in a bundled step is
  reported at the original `.ts:line:col`, stack included.

---

## State and lifetime

What you can rely on between calls, when running under the MCP server.

### Two ways to keep state

A *session* is identified by the `session` argument (`instance:context`,
default `"default"`). All `run_script` calls and all plugin tool calls
that share a session also share state:

- **`globalThis`** — anything you assign (`globalThis.cache = …`,
  `function f(){}`, `var x`) stays visible to later calls in the same
  session. Use it for rich in-session working state: parsed data,
  helper closures, accumulated results.
- **`vars`** — a small string→string store (`vars.set`, `vars.get`,
  `vars.has`, `vars.delete`, `vars.keys`). Use it for the few values
  that must *outlive a reset* of `globalThis` (see below): an auth token
  you captured once, a pagination cursor, a feature flag.

`page`, `context`, `request`, `browser` always reflect the session's
current browser — never cache them in `globalThis`; cache what you read
from them, not the handles.

### When `globalThis` resets (and `vars` does not)

`globalThis` is fast but not permanent. It is wiped — silently, you just
see a fresh global on the next call — when any of these happen:

- a call hits its timeout or runs the browser/runtime out of memory;
- the session's browser is relaunched or reconnected (a new browser
  session under the same name — old page references would be dead);
- the server is busy with many sessions and reclaims an idle one's
  working memory to serve others.

`vars` survives all of those for the life of the session. The session
itself (and its `vars`) ends only when it sits unused past the idle
timeout (default 30 minutes), is closed explicitly, or the server stops.

Rule of thumb: build freely in `globalThis`; copy into `vars` the
handful of things you cannot afford to recompute or re-fetch after a
reset.

### Isolation

Tools and scripts in one session share the *same* `globalThis` — it is
shared working space, not a sandbox between tools. Don't depend on
another tool's globals, and don't clobber built-ins
(`globalThis.JSON`, prototypes); a tool that does will break later
calls in that session. Different sessions never share state. Calls
within one session are serialised (no two run at once); different
sessions run independently.

### BDD

Under the test runner the model differs: one VM per worker, scenarios
parallel across workers and serial within one. The `World` (`this`) is
rebuilt per scenario; `setWorldConstructor` /
`setDefinitionFunctionWrapper` are per-VM (last call wins). `vars` /
`globalThis` continuity is not a BDD concept — use the `World` and
hooks.

### Imports

No cross-file or cross-plugin shared state beyond what you `import`
directly. Share helpers by importing them; there is no implicit
cross-plugin channel by design.

---

## Reference

### Manifest (`PluginManifest`)

| Field          | Wire (camelCase) | Default | Meaning |
|----------------|------------------|---------|---------|
| name           | `name`           | —       | Required, non-empty, unique across all loaded extensions. Binding/tool key. |
| description    | `description`    | none    | Shown in `tools/list`. |
| input schema   | `inputSchema`    | none    | JSON Schema; **enforced** — non-conforming calls rejected before the handler. |
| allow          | `allow`          | `{}`    | Capability manifest. |
| expose as tool | `exposeAsTool`   | `false` | Promote to a first-class MCP tool. |
| timeout ms     | `timeoutMs`      | none    | Per-invocation handler timeout (ms); enforced for every caller. |

### Capability manifest (`PluginAllow`)

| Field    | Wire        | Default | Meaning |
|----------|-------------|---------|---------|
| commands | `commands`  | `{}`    | name → command (shell string or spec object; `persistent` opt-in); alias `exec`. |
| net      | `net`       | `[]`    | host allow-list for `request` + `fetch`; empty = unrestricted. |

### Registration surface (JS globals)

`defineTool` · `Given` · `When` · `Then` · `defineStep` · `And` · `But` ·
`Before` · `After` · `BeforeAll` · `AfterAll` · `BeforeStep` · `AfterStep` ·
`defineParameterType` · `setDefaultTimeout` · `setDefinitionFunctionWrapper`
· `setWorldConstructor` · `setParallelCanAssign` (inert) · `ferridriver.host`

---

## What the runtime guarantees

What you can count on as an author:

1. **`inputSchema` is enforced.** If you declare one, a call whose
   arguments do not match it is rejected as a tool error *before* your
   handler runs — you never see malformed input through the schema. A
   schema that is itself invalid JSON Schema is reported, not ignored.
   Still validate domain rules the schema cannot express inside the
   handler.
2. **Tool names are unique and non-empty.** A duplicate or blank `name`
   fails that extension at load time. A name that collides with a
   built-in or another loaded tool is not exposed. Namespace your names
   (`vendor.area.action`).
3. **Tool failures are reported as errors.** When your handler throws,
   the caller gets an error result (not a "success" containing an error
   string), with the message first and the full detail after. (Plain
   `run_script` is different: it always succeeds and you inspect its
   `status` field.)
4. **`timeoutMs` is honoured for every caller** — whether the tool is
   invoked as a promoted MCP tool or by another extension. Without it,
   only the session-wide script timeout applies.
5. **Discovery is recursive and uniform.** A configured directory is
   scanned recursively; `.js .cjs .mjs .jsx .ts .cts .mts .tsx` are all
   accepted, the same way for the MCP server and the test runner. A file
   you name explicitly is used as-is.
6. **You can inspect what loaded.** The built-in `ferridriver_extensions`
   tool lists every loaded extension file, its tools, descriptions,
   whether each is exposed, its timeout, and its declared capabilities.

### Things to keep in mind

- **Shell-form `commands` are code, not config.** A string `run` (or
  shorthand) executes via `sh -c` with the *server process's*
  privileges — `$(…)`, `&&`, `|`, redirection are live. `${values}` are
  shell-escaped, but never write a line that re-interprets a value
  (`sh -c "${x}"`, `eval ${x}`): that defeats the escaping. **Argv form**
  (`run: ["cmd", "${arg}"]`) runs with no shell at all — prefer it; the
  trust-boundary concern simply disappears. Template = trusted code you
  commit; values = untrusted data (see *Capabilities*).
- `inputSchema` validation runs on every call. That is fine for tool
  call volumes; do not put megabyte schemas on a tool expecting
  thousands of calls per second.
