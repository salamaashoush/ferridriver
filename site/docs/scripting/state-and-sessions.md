# State and sessions

What you can rely on between calls, when running under the MCP server.

## Sessions

A **session** is identified by the `session` argument
(`instance:context`, default `"default"`). All `run_script` calls and
all plugin tool calls that share a session also share state.

```
session: "admin"          isolated context named "admin" in default instance
session: "staging:qa"     context "qa" on Chrome instance "staging"
session: "default"        the implicit default
```

- **Instance** (before `:`): selects the browser process. Each can have
  its own Chrome flags, DNS rules, profile.
- **Context** (after `:`): isolates cookies / storage within that
  browser.

Different sessions never share state. Calls within one session are
serialised (no two run at once); different sessions run independently.

## Two ways to keep state

### `globalThis`

Anything you assign at the top level — `globalThis.cache = …`,
`function f() {}`, `var x` — stays visible to later calls in the **same
session**. Use it for rich working state: parsed data, helper closures,
accumulated results.

### `vars`

A small string→string store: `vars.set(name, value)`, `vars.get(name)`,
`vars.has(name)`, `vars.delete(name)`, `vars.keys()`.

Use it for the few values that must **outlive a reset** of
`globalThis` (see below): an auth token you captured once, a pagination
cursor, a feature flag.

```js
// First call — discover, cache, persist
const token = await page.evaluate(() => localStorage.token);
globalThis.parsed = parseSomethingExpensive();
vars.set("auth_token", token);
return "ready";

// Later call in the same session
const token = vars.get("auth_token");        // always present
const parsed = globalThis.parsed;            // present unless VM reset
```

## When `globalThis` resets (and `vars` does not)

`globalThis` is fast but not permanent. It is wiped — silently, you
just see a fresh global on the next call — when any of these happen:

- a call hits its timeout or runs the browser / runtime out of memory;
- the session's browser is relaunched or reconnected (a new browser
  session under the same name — old page references would be dead);
- the server is busy with many sessions and reclaims an idle one's
  working memory to serve others.

`vars` survives all of those for the life of the session. The session
itself (and its `vars`) ends only when it sits unused past the idle
timeout (default 30 minutes), is closed explicitly, or the server
stops.

**Rule of thumb:** build freely in `globalThis`; copy into `vars` the
handful of things you cannot afford to recompute or re-fetch after a
reset.

## Browser handles never cache

`page`, `context`, `request`, `browser` always reflect the **session's
current** browser — never cache them in `globalThis`. Cache what you
*read* from them, not the handles.

```js
// Bad: handle goes stale after browser relaunch
globalThis.savedPage = page;

// Good: cache the read
globalThis.savedTitle = await page.title();
```

## Isolation between tools

Tools and scripts in one session **share the same `globalThis`** — it
is shared working space, not a sandbox between tools. Don't depend on
another tool's globals, and don't clobber built-ins
(`globalThis.JSON`, prototypes) — a tool that does will break later
calls in that session.

Different sessions are fully isolated; their globals never see each
other.

## BDD: per-scenario World, not session

Under the test runner the model **differs**. There is one VM per
worker; scenarios run parallel across workers and serial within one.

- The **World** (`this`) is rebuilt per scenario — `this.page`,
  `this.context`, etc. are fresh.
- `setWorldConstructor` and `setDefinitionFunctionWrapper` are per-VM
  (last call wins).
- `vars` / `globalThis` continuity is **not** a BDD concept — use the
  `World` for per-scenario state, hooks for per-feature / per-run
  state.

## Imports

No cross-file or cross-plugin shared state beyond what you `import`
directly. Share helpers by importing them — there is no implicit
cross-plugin channel by design.

```ts
// shared/state.ts
export const cache = new Map<string, unknown>();

// extension-a.ts
import { cache } from "./shared/state.js";
defineTool({ name: "a.read", handler: () => cache.get("key") });

// extension-b.ts
import { cache } from "./shared/state.js";
defineTool({ name: "b.write", handler: ({ args }) => cache.set("key", args.value) });
```

Both files import the **same** module instance (rolldown deduplicates),
so they share `cache` at runtime.
