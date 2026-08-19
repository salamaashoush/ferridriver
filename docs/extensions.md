# Extensions

An **extension** is a single JavaScript or TypeScript file that contributes
to ferridriver at runtime. One file can contribute to four hosts:

- **MCP server** (`ferridriver mcp`) — registers tools via `defineTool(...)`.
- **BDD test runner** (`ferridriver bdd`) — registers Cucumber step
  definitions, hooks, and parameter types via `Given`/`When`/`Then`/
  `Before`/`After`/`defineParameterType`/...
- **Test runner** (`ferridriver test`) — contributes fixtures onto the
  base `test` chain with `defineFixtures(...)`, so a spec receives them
  without importing anything.
- **Ad-hoc scripts** (`ferridriver run`, MCP `run_script`) — the same VM
  bindings the others use.

`ferridriver test` loads extensions like every other host. It did not
always: the test runner used to load none at all, which is why a package
had no way to reach a spec.

The same file can serve all four. It branches on the `ferridriver.host`
global to decide what to contribute where.

> Companion document: `docs/extension-architecture.md` records *why* the
> system is shaped this way (the comparison against VS Code / Deno / WASM /
> Rollup and the decisions deferred). This document is the *how*: the
> authoring contract and reference.

---

## Mental model

```
extension.ts ──► rolldown bundle (TS + node_modules + tree-shake)
             ──► QuickJS bytecode (compiled ONCE at startup)
             ──► content-hash cache (in-process, then on disk)
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

`ferridriver.host` is a string set once per session: `"mcp"`, `"bdd"`,
`"test"`, or `"script"`. Use it so one file can ship a tool and its
matching step without registering the wrong thing in the wrong host:

```ts
if (ferridriver.host === "mcp") {
  defineTool({
    name: "acme.login",
    description: "Log a test user in and return the session cookie",
    inputSchema: { type: "object", properties: { user: { type: "string" } }, required: ["user"] },
    allow: { net: ["*.acme.com"] },
    handler: async ({ args, request }) => {
      const res = await request.post("https://api.acme.com/login", { data: { user: args.user } });
      return { cookie: (await res.json()).cookie };
    },
  });
}

if (ferridriver.host === "bdd") {
  Given("I am logged in as {string}", async function (user: string) {
    await this.page.goto(`https://app.acme.com/login?u=${user}`);
  });
}

if (ferridriver.host === "test") {
  defineFixtures({
    acmeUser: async ({}, use) => { await use("someone@acme.com"); },
  });
}
```

Registering for the wrong host is harmless (the host ignores kinds it does
not consume) but wastes work and muddies intent — gate it.

Branching is not the only way to scope a contribution. An `entries` item
in a package manifest can name the hosts it loads under, which keeps the
file out of the other hosts entirely rather than running it and having it
decline — see [Narrowing an entry to some hosts](#narrowing-an-entry-to-some-hosts).

---

## Authoring MCP tools

### `defineTool`

Also reachable as `tool(...)` — the same function under a shorter name,
and as `ferridriver.tool`. Nothing distinguishes them; pick one and be
consistent within a package.

Two equivalent forms:

```ts
// Inline handler on the manifest object:
defineTool({
  name: "string",              // required, globally unique, dot-namespaced by convention
  title: "string",             // optional human display label, surfaced in tools/list
  description: "string",       // optional, surfaced in tools/list
  inputSchema: { ... },        // optional JSON Schema, surfaced in tools/list AND enforced
  outputSchema: { ... },       // optional JSON Schema for the RETURN value (see below)
  annotations: { ... },        // optional MCP tool annotations (see below)
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
  as `await tools["name"](args)`, but is **not** advertised in the MCP
  server's `tools/list`. Use for shared helpers.
- `true`: additionally promoted to a first-class MCP tool. `name`,
  `description`, and `inputSchema` become the tool's contract. The tool
  call and the `tools[...]` binding route through the same handler.

### Handler context

The handler receives one object:

| Field       | Type                  | Notes |
|-------------|-----------------------|-------|
| `args`      | the caller's argument | For a promoted tool, the MCP `arguments` object. |
| `page`      | `Page`                | The live browser page for the session. |
| `context`   | `BrowserContext`      | The session's browser context. |
| `browser`   | `Browser`             | The browser the session runs on. |
| `request`   | `APIRequestContext`   | HTTP client. Net-restricted per the effective `allow.net`. |
| `commands`  | `Commands`            | `.run(name, vars?)` — runs a declared command template. |
| `vars`      | `Vars`                | Session-scoped string store; survives VM rebuilds. |
| `fs`        | `Fs`                  | Sandboxed filesystem, confined to `scriptRoot`. |
| `artifacts` | `Artifacts`           | Output sandbox (`artifactsRoot`) for screenshots/PDFs/traces. |
| `sidecars`  | `Sidecars`            | `.connect(name)` for a declared `[[sidecars]]` process. |
| `settings`  | your settings type    | The operator's `[extensions.settings.<key>]` block. |
| `session`   | `SessionRef \| undefined` | `{ key, instance, context }` — WHICH browser (and therefore which environment) this call drives. |
| `log`       | `Log`                 | `log(msg)` plus `log.error/warn/info/debug/trace` and `log.enabled(level)`, all through the host's `tracing` filter. |
| `signal`    | `AbortSignal`         | Fires when `timeoutMs` expires (see below). |

Return any JSON-serialisable value; it becomes the tool result.

Derive an environment from `session.instance` rather than taking one as an
argument: the instance selects the browser process, so an argument that
disagrees with it drives the wrong environment while reporting success.

The `@ferridriver/extension` types declare all of it (see "The authoring
loop"), and `ferridriver ext check` verifies your handler against them.

### Cancellation: `signal`

When `timeoutMs` expires, the dispatcher stops awaiting the handler —
but the handler's JS continuation keeps executing on the session VM.
`signal` is a standard `AbortSignal` (aborted with a `TimeoutError`
reason at that moment) so the handler can stop cooperatively instead of
running on as zombie work: pass it to `fetch(url, { signal })`, check
`signal.aborted` between steps, or register
`signal.addEventListener("abort", ...)` for cleanup.

### `outputSchema` and `annotations`

`outputSchema` is the symmetric half of the schema contract: when
declared, the promoted tool advertises it in `tools/list`, the server
validates the handler's RETURN value against it (a non-conforming
result is the extension author's bug and surfaces as a tool error), and
a conforming result additionally ships as MCP `structuredContent`
alongside the text payload.

`annotations` passes MCP tool annotations through to `tools/list`
verbatim: `{ title?, readOnlyHint?, destructiveHint?, idempotentHint?,
openWorldHint? }`. They are client-facing hints; the server enforces
nothing from them.

> When the manifest declares `inputSchema`, the caller's `args` are
> validated against it (full JSON Schema, via the `jsonschema` crate)
> **before** the handler runs; a non-conforming call is rejected as a
> tool error and the handler is never entered. You still get the parsed
> value as `args` — validation does not coerce, only gate.

> `session` is a **reserved argument key** on promoted tools: the server
> reads it to select the browser session (same `instance:context` format
> as every other MCP tool) before dispatch. It is exempt from
> `inputSchema` validation — a schema with
> `additionalProperties: false` does not block session routing — but it
> IS still present on the `args` object the handler receives. Do not
> declare your own `session` property with different semantics.

---

## Capabilities

`allow` is a declarative capability manifest, enforced in Rust at the
binding boundary. The handler source alone cannot grant itself authority
it did not declare. Defaults differ per capability: `commands` is
default-deny (an absent map grants nothing), while `net` is default-open
for back-compatibility (an absent list leaves HTTP unrestricted;
declaring any host flips it to default-deny).

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

A host allow-list scoping the handler's HTTP — the `request` client
(both the handler's `request` arg and the global `request`) and the
global `fetch` share one core, so the list binds all of them.

- Empty / absent: HTTP is unrestricted (back-compat default).
- Non-empty: the tool's HTTP entry points all flip to **default-deny**.
  Each entry is an exact host (`api.acme.com`) or a leading-wildcard
  suffix (`*.acme.com`, which also matches the bare apex `acme.com`). Any
  other host is rejected before the request is made. The policy follows
  the running handler: a tool calling another tool, or two tools running
  concurrently, each see only their own declared list.
- Capability follows the registrar: a callback the handler schedules —
  `setTimeout`/`setInterval`/`setImmediate`, `queueMicrotask` (and
  `process.nextTick`, which rides it), `page.on` listeners,
  `page.route`/`context.route` handlers, `exposeFunction`/
  `exposeBinding` callbacks, WebSocket route handlers, screencast
  frames — is captured at the point of **registration** and keeps the
  scheduling tool's `allow.net` when it later fires cross-task, instead
  of falling back to the unrestricted resting policy. Callbacks
  registered at top level (outside any tool) stay unrestricted. An async
  callback's grant covers its whole continuation, not just the
  synchronous call — a `page.route(url, async r => { await fetch(...) })`
  handler stays restricted where its `fetch` actually runs.

The handler's `request` **arg** has the grant baked in at dispatch, so
it is enforced unconditionally, anywhere in the handler. The global
`fetch` and global `request` read the grant that is active on the
running handler's stack; that is reliable on the handler's synchronous
prefix and inside any registered callback, but a global `fetch` invoked
from a continuation *after* awaiting an unrelated host operation can
observe the resting (unrestricted) policy. For guaranteed enforcement of
the handler's own HTTP, prefer the `request` arg over the global.

`allow.net` scopes HTTP (`request` + `fetch`) **only**. `page`/`context`
browser navigation is a separate, deliberately ungated authority — an
automation tool must be able to navigate. There is no `fs` capability:
the only filesystem a handler can reach is the session's `fs` and
`artifacts` globals, both already confined to their `PathSandbox` roots,
so an extension-level `fs` scope would have no ungated authority left to
gate.

### Operator policy: `[extensions.policy]`

The manifest is the AUTHOR's half of a two-party grant — it states what
the tool NEEDS. The config-side policy is the OPERATOR's half — what the
deployment GRANTS. The effective authority a tool dispatches with is
the intersection; a manifest can never widen past the ceiling.

```toml
[extensions]
paths = ["./extensions"]

[extensions.policy]
# Host ceiling for every extension's HTTP.
net = ["*.acme.com", "localhost"]
# What allow.commands declarations are permitted:
#   "any" (default) | "argvOnly" | "none"
commands = "argvOnly"
# Whether packages may contribute fixtures onto the base `test` chain
# with defineFixtures. Default true.
fixtures = false
# Whether packages may contribute configuration defaults with
# defineDefaults. Default true.
configDefaults = false
```

- `net` absent: manifests keep the back-compat semantics documented
  above (no declaration = unrestricted).
- `net` present: every tool flips to default-deny. A tool with no
  `allow.net` gets exactly the ceiling; a tool with one keeps only the
  entries the ceiling subsumes (dropped entries are reported as startup
  warnings and via `ferridriver_extensions`). `net = []` denies all
  extension HTTP.
- `commands = "argvOnly"` fails registration of any tool declaring a
  shell-string command spec (where `$(…)`, pipes, and redirection
  live) — argv-array specs still work. `commands = "none"` fails
  registration of any command-declaring tool. Both conflicts are also
  reported as startup warnings.

- `fixtures = false` refuses any `defineFixtures` call and FAILS the run,
  naming the key. It is deliberately not a skip: a suite that silently
  lost a fixture it never declared is a mystery, not a policy. The
  ceiling covers the `defineFixtures` entry point only — a package's own
  `test.extend` / `mergeTests` chains and its `expect.extend` matchers are
  never clamped, because those change nothing a suite did not ask for by
  importing them.

Every ceiling refusal is a hard failure for the same reason. The loader
skips an extension whose own top level threw and left no registrations
behind; it never skips past authority the operator withheld.

The `commands` ceiling exists because arbitrary exec subsumes every
other capability: a tool granted a shell line can trivially reach any
host `allow.net` would deny. `argvOnly` narrows that to declared
binaries with inert arguments; `none` closes it.

Enforcement is Rust-side at `defineTool` registration inside each
session VM, so every dispatch path (promoted MCP tool, `tools.<name>`,
`ferridriver run`, BDD) sees the same effective grants.

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

## Contributing fixtures: `defineFixtures`

A package can add fixtures to the BASE `test` chain, so a suite that
never imports the package still receives them through the `test` it
already imports:

```ts
// the package
defineFixtures<{ deployment: string; signedIn: void }>({
  deployment: async ({}, use) => { await use(process.env.DEPLOYMENT ?? "staging"); },
  signedIn: [async ({ page, deployment }, use) => {
    await page.goto(`https://${deployment}.example.com/login`);
    await use();
  }, { auto: true }],
});
```

```ts
// the suite, unchanged and importing nothing of the package's
import { test, expect } from "@ferridriver/test";

test("knows where it is pointed", async ({ deployment }) => {
  expect(deployment).toBe("staging");
});
```

The entries are exactly `test.extend`'s: a bare factory, a static value,
or the `[value, { scope, auto, option }]` tuple. The override rules are
`test.extend`'s too — packages compose in **load order**, a later
same-name entry shadows the earlier one, and that entry's own `{ label }`
dependency resolves to the registration it shadows (its `super`), never
to itself.

`defineFixtures` returns the base `test`, so a package can keep using it
directly. It does not — and cannot — replace `ferridriver.test`:
`@ferridriver/test` is one module instance per VM whose export slots hold
the values copied when it evaluated, so an importer keeps the object it
linked against no matter what is assigned afterwards. `ferridriver.test`
is read-only for the same reason; an assignment that looked like it had
replaced the base chain while reaching nobody is the worst outcome
available.

**The base chain seals** once every extension has installed. From that
point each `test.extend()` COPIES the chain, so a later contribution
would reach the suites that had not derived one yet and miss those that
had. A `defineFixtures` from a spec bundle, a step file or a
`run_script` therefore throws and points at `test.extend()`. A chain a
package itself derived with `test.extend` before a later package's
`defineFixtures` likewise keeps the base it copied.

A step registered through `bindSteps(test)` resolves from the chain that
`test` carries, so a contributed fixture is destructurable in a step body
the same way:

```ts
const { Given } = bindSteps(ferridriver.test);
Given("the deployment is known", async ({ deployment }) => { /* ... */ });
```

`defineFixtures`, `defineDefaults` and `bindSteps` are globals, and are
also exported from the `ferridriver` module.

## Contributing config defaults: `defineDefaults`

A package can supply defaults for the run's configuration, so a suite
that adopts it does not have to copy its settings into every
`ferridriver.toml`:

```ts
defineDefaults({
  test: {
    timeout: 60_000,
    retries: 1,
    // Nested exactly as the config file nests it.
    browser: { use: { testIdAttribute: "data-qa" } },
  },
});
```

The contribution is the **lowest layer**. Everything else still wins,
in the usual order:

```
extension defineDefaults   <- lowest
machine / user / repo / cwd / local config files
--config <file>            (a .toml/.yaml/.json document, or a .ts/.js module)
FERRIDRIVER_* environment overrides
CLI flags                  <- highest
```

Two packages that set the same key compose in load order, the later one
winning — the same rule the config files follow. `ferridriver config`
names the package a value came from, exactly as it names the file:

```
test.timeout   60000   extension ./ext/acme.ts
```

### Why the run reads the config more than once

The set of extensions is itself configuration, so it cannot come from an
extension. Startup therefore resolves the layer stack in passes:

1. resolve the layer stack with no contributions — this is what says
   which packages to load, which bundler options to compile them with,
   and what the `[extensions.policy]` ceiling is;
2. load the packages, then re-resolve with whatever they contributed
   underneath every file;
3. and, when `--config` names a `.ts`/`.js` module, bundle and evaluate
   it and re-resolve once more with its document on top. It comes last
   because compiling it needs everything the first two passes settled —
   which is also why a config module may not set `extensions`,
   `bundler`, `scripting` or `[test].moduleAliases`.

That is also why a contribution may not set the sections that decide how
the contributing package itself was found, compiled or trusted. Each is
refused by name, and the refusal fails the run:

| Refused | Because |
|---|---|
| `extensions` | the set of extensions is resolved before any of them runs |
| `bundler` | the bundler compiled this package before it could ask for a different one |
| `scripting` | the sandbox an extension runs under is the operator's to set |
| `test.moduleAliases` | the alias table is sealed by the first bundle, which is the one that read this package |

A key the schema does not have is refused too, naming the key — strict
where a config FILE is only warned about, because a typo in a
dependency's defaults is one nobody would ever see.

`defineDefaults` is read once, before the first session exists. Calling
it from a spec, a step file or a script throws: configuration has
already been resolved by then.

Refused wholesale by `[extensions.policy] configDefaults = false`.

---

---

## Discovery and configuration

Extensions are configured in the unified config file
(`ferridriver.toml`/`.yaml`/`.json`), top-level (both hosts load it):

```toml
# Files or directories. A directory is scanned RECURSIVELY for any
# source file (.js .cjs .mjs .jsx .ts .cts .mts .tsx). Used by the MCP
# server (tools) AND, bundled alongside BDD step files, by the test
# runner (steps).
extensions = ["./extensions", "./tools/acme-login.ts"]
# Or the detailed table shape, which adds the operator policy ceiling
# (see "Operator policy" above):
#   [extensions]
#   paths = ["./extensions"]
#   [extensions.policy]
#   net = ["*.acme.com"]
#   commands = "argvOnly"

[scripting]
# Sandbox relaxations — default-deny, like allow.net.
# Names a script may read via process.env (intersected with the real
# environment; absent names stay absent — never invented). Empty ⇒
# process.env is {}.
allowEnv = ["HOME", "TZ"]

[test]
# JS/TS step-definition globs. Defaults to steps/**/*.{js,ts} and
# step_definitions/**/*.{js,ts} when empty.
steps = ["features/steps/**/*.ts"]
```

The `ferridriver bdd` runner bundles discovered step files **and** the
configured `extensions` into one module, so an extension's `Given/When/
Then` are available to tests exactly like a step file's.

Both discovery paths (MCP extension loader and BDD runner) share one
accepted-extension set and one recursive walk, so a `.tsx`/`.cts`
extension is visible identically to both hosts.

### Extension packages: the `ferridriver` field in `package.json`

Anything past a couple of files wants to be a **package**: a directory
(or a `node_modules` entry) with a `package.json`. A package is named
once in the config — as a path or as a bare specifier — and declares
everything else itself:

```json
{
  "name": "@acme/ferridriver-acme",
  "type": "module",
  "ferridriver": {
    "entries": ["./src/login.ts", "./src/sign.ts"],
    "requires": {
      "commands": ["acme-cli"],
      "env": ["ACME_HOME"],
      "net": ["*.acme.com"],
      "sidecars": ["acme-gate"]
    },
    "settings": {
      "acme": {
        "type": "object",
        "properties": { "origin": { "type": "string" } },
        "required": ["origin"],
        "additionalProperties": false
      }
    }
  }
}
```

```toml
extensions = ["./plugins/acme"]        # or "@acme/ferridriver-acme"
```

- **`entries`** — the modules to load as extensions, in declaration
  order. Each is a path relative to the package directory: a file (the
  extension may be omitted) or a directory, scanned recursively.
  Everything else in the package is reachable only as an import of an
  entry. That is the point: a shared `lib/` gets bundled through the
  imports instead of being loaded as an extension of its own and warned
  about for declaring no tools. Without `entries`, Node's own
  single-entry chain applies (`exports` -> `module` -> `main` ->
  `index.*`), which can only ever name one module.
- **`requires`** — what the host must already provide. Declarations, not
  grants: per-tool authority still comes from `defineTool`'s `allow`,
  clamped by `[extensions.policy]`. An unmet requirement stops the
  package from loading (on the hosts it applies to — see below) and says
  which config key fixes it, rather than failing on the first tool call:
  - `commands` — programs that must be on `PATH`.
  - `env` — names the operator must list in `[scripting].allowEnv`
    (allow-listed but unset is a warning, not a block).
  - `net` — hosts that must fit inside the `[extensions.policy]` net
    ceiling.
  - `sidecars` — names some `[[sidecars]]` entry must declare.
- **`settings`** — a JSON Schema per `[extensions.settings.<key>]`
  block the package reads, keyed the way settings resolve (tool
  namespace, or a full tool name). Validated against the operator's
  actual config at load, with an absent block validated as `{}` so a
  required field is reported. A mistyped key becomes an error instead of
  an `undefined` the handler reads at runtime.

`ferridriver config` prints each package's entry count, declared entries
and unmet requirements; `ferridriver doctor` fails on them.

#### Narrowing an entry to some hosts

An `entries` item may be written in full instead of as a path:

```json
{
  "ferridriver": {
    "entries": [
      "./src/fixtures.ts",
      { "path": "./src/mcp-tools.ts", "hosts": ["mcp"] },
      { "path": "./src/sign.ts", "requires": { "commands": ["acme-cli"] } }
    ]
  }
}
```

- **`hosts`** — the hosts this entry loads under (`mcp`, `bdd`, `test`,
  `script`). Absent means every host, which is what a bare string says.
- **`requires`** — preconditions for this entry alone. Present, they
  REPLACE the package's rather than adding to them.

Narrowing is not only about what loads. An entry's `requires` are
checked only where the entry runs, so `./src/mcp-tools.ts` naming
`acme-cli` blocks the package under `ferridriver mcp` when that binary
is absent — and blocks nothing under `ferridriver test`, where the entry
would not have loaded anyway. Before this, one MCP-only dependency took
a package's fixtures and providers down on every host.

Package-level `requires` are unchanged: they apply wherever the package
loads, and an entry that declares none inherits them.

#### What `ferridriver ext check` reports

Per host, because what a package registers is a function of the host it
loads under:

```
mcp
  /path/to/plug.ts
    1 tools
    acme_ping [mcp tool]

bdd
  /path/to/plug.ts
    1 steps

test: nothing loads
```

Every kind the extraction pass records is counted — tools, steps, hooks,
parameter types, tests, fixtures and config defaults — and a kind nobody
registered is simply absent rather than a zero to explain. A package
that contributes only fixtures reports its fixtures and is `ok`; it used
to read as an MCP server that had forgotten to register anything.

### The authoring loop

**`ferridriver ext check [PATH...]`** verifies the extensions (the paths
given, or the configured `extensions`) and prints what the host sees:

- the entry files each spec resolved to and the package's declared entries;
- unmet `requires` and settings-schema violations;
- **TypeScript diagnostics** for every `.ts` entry and everything it
  imports;
- every tool with its capabilities and schemas, and any bundle error.

It exits non-zero when something is wrong, so it works as a pre-commit or
CI gate. `--json` emits the same report as data.

**`ferridriver ext dev [PATH...]`** is the same pass in a watch loop,
re-run on every save — including a `package.json` edit that changes the
entry set.

```
ferridriver ext check ./plugins/acme      # once
ferridriver ext dev ./plugins/acme        # on every save
```

The type pass needs no setup. The declarations are embedded in the binary
(so they always match the runtime that will load the extension) and are
compiled with the TypeScript compiler, resolved in this order:
`FERRIDRIVER_TSC`, `tsc` on `PATH`, `tsc` in a
`node_modules/.bin` above the extension, then `npx`/`bunx` fetching
`typescript` from `https://registry.npmjs.org/` (cached after the first
run). The fetch is opt-in: without `FERRIDRIVER_TS_DOWNLOAD=1` a gate
never pulls and runs a package from the network on its own.
`--no-typecheck` skips the pass. When no compiler can be found the report
says so instead of quietly passing.

The registry is pinned rather than inherited from the machine's npm
config, because a corporate registry commonly proxies a curated subset and
answers 403 for everything else.

An author `tsconfig.json` next to the package is inherited (`extends`), so
its options still apply — the runtime-describing options are then applied
on top.

For editor support without an install, `ferridriver ext types` writes the
same declarations into `./node_modules` (or `--out <dir>`):

```
ferridriver ext types
```

```ts
import type { ToolContext } from '@ferridriver/extension';
// `defineTool`, `tools`, `vars`, `fs`, ... are globals; nothing to import.
```

Typing a tool's argument and result is what makes the check useful:

```ts
interface LoginArgs { user: string }

defineTool<LoginArgs, { url: string }>({
  name: 'acme.login',
  description: 'Log a user in',
  exposeAsTool: true,
  inputSchema: { type: 'object', properties: { user: { type: 'string' } }, required: ['user'] },
  async handler({ args, page, vars }) {
    vars.set('user', args.user);           // `args.usr` is an error
    await page.goto('https://app.acme.com'); // `page.gotoo` is an error
    return { url: page.url() };             // returning a number is an error
  },
});
```

A third type parameter types the `[extensions.settings.<key>]` block the
handler reads: `defineTool<Args, Result, { origin: string }>`.

**`ferridriver_extensions` with `action: "reload"`** does the same
re-resolve/re-bundle inside a running MCP server and installs the result:

- the promoted tool set is replaced and `tools/list_changed` is sent when
  the advertised names actually changed;
- every live session VM is dropped, so the next call on an open session
  runs the new code — while that session's `vars`, cookies and persistent
  processes survive;
- the reply reports `added` / `removed` / `droppedSessionVms` alongside the
  usual registry report.

Restarting the MCP client was previously the only way to pick up an edit,
and it tore down every browser session with it.

---

## Node-ish APIs: `process` and `fetch`

So real npm packages run, scripts and handlers get a sandbox-safe
`process` and a standard `fetch`.

### `process`

Always available (no authority, real values): `platform`, `arch`,
`version`, `versions`, `release`, `argv` (`["ferridriver","script"]`),
`pid`, `nextTick`, `hrtime` (+ `hrtime.bigint()` -> BigInt ns),
`stdout`/`stderr` (`.write(chunk)` routes into the captured console —
`stdout`->log, `stderr`->error, one trailing newline trimmed; returns
`true`, `isTTY` is `false`), `cwd()` (returns the sandbox root, never
the real cwd). `nextTick(cb)` is a FIFO microtask (via
`queueMicrotask`), not Node's separate higher-priority queue — order
follows scheduling order.

- `process.env` — **default `{}`**. Only the names in `[scripting]`
  `allowEnv`, and only if set in the server's environment, appear; the
  object is frozen. A name you didn't list is simply absent — there is
  no way for a script to read an unlisted variable.
- `process.exit()` — throws (a script must never kill the server).
- `process.binding`/`dlopen`/`kill`/`chdir`/`setuid`/… — not present.
- `process.versions.node` — never present (`process.versions` is
  honest: `ferridriver` + `quickjs` only). This is not Node.

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

## Providing an import specifier

A package can answer for an import specifier, so a suite written against
some other package runs unmodified:

```json
{
  "name": "vendor-shim",
  "ferridriver": {
    "apiVersion": 2,
    "entries": ["src/steps.ts"],
    "provides": {
      "modules": { "fake-vendor": "src/vendor.ts" },
      "aliases": { "fake-vendor/testing": "fake-vendor" }
    }
  }
}
```

Anything in the run — a spec, a step file, another extension, a
`ferridriver run` script — can then `import { thing } from 'fake-vendor'`
or `require('fake-vendor')`, and every one of them receives the SAME
module instance. State the provider holds is shared, which is usually
the point.

The rules, each of which is a load-time error naming the package and the
specifier: one specifier has one owner; a specifier the runtime already
serves cannot be claimed (`@playwright/test`, `node:*`, `@cucumber/*`,
`@ferridriver/*`, `playwright`, `playwright-core`); an alias may only
target a specifier its own package provides; providers may not form a
cycle. The operator's `[test].moduleAliases` outrank a package's claim,
with a warning, and `[extensions.policy] modules` / `allowModules` is
the ceiling over claiming at all.

A provider that throws while evaluating aborts the run rather than being
skipped — every module importing its specifier depends on it.

## The compile pipeline

1. **Discover** files (config + globs).
2. **Bundle** each with rolldown (oxc): resolves the whole import graph
   including `node_modules`, transpiles TS, tree-shakes, emits one ESM
   chunk with a hidden source map. Cache-miss bundles run concurrently.
3. **Compile** the chunk to QuickJS bytecode once, in a single throwaway
   runtime shared by the whole batch, then **evaluate** it there to read
   the manifest off the registry it registered into.
4. **Cache** bytecode + extracted manifests keyed by
   `hash(canonical path + file bytes)`, in-process and on disk.
   Unchanged files skip bundle + compile entirely on reload.
5. **Load** the bytecode into each session VM with `Module::load` — no
   re-parse, no resolver (imports are already inlined).

The extraction context and a session VM must agree, or a package passes
`ferridriver ext check` and does nothing at runtime (or the reverse), so:

- **Extraction installs what a session installs** before extensions run:
  the standard globals, `expect`, the extension registry, and the
  Playwright `test` surface. Only session-scoped bindings
  (`fs`, `vars`, `artifacts`, `commands`, `page`, `request`) are absent —
  they are per-session by definition, and top-level extension code must
  not depend on them.
- **The operator ceiling applies at extraction too.** `defineTool` clamps
  `allow.*` when the tool registers, so `[extensions.policy]` decides the
  same way in both places.
- **Files evaluate in batch order into one shared context, cache hits
  included.** A session evaluates every extension into one VM; extraction
  reaches each file in the world its predecessors left behind.
- **A module is named after its file, not its position in the batch.**
  One QuickJS context holds one module per name, and extensions routinely
  reach a session having been compiled in different batches.

Consequences worth knowing as an author:

- **Imports work.** `import './helpers.ts'`, `import pkg from 'some-dep'` —
  all bundled and tree-shaken. No Node/Bun in the run path; QuickJS has no
  Node builtins (rolldown `platform: neutral`).
- **The bytecode cache has two tiers**: in-process, and on disk under an
  ABI tag (QuickJS version, arch, endianness, pointer width, crate
  version). The tag is what keeps the `unsafe Module::load` invariant —
  bytecode is only ever read back by an ABI-identical toolchain — and
  every entry also records the content hash of each transitive input, so
  an edited helper invalidates it. `FERRIDRIVER_NO_BYTECODE_CACHE`
  disables the disk tier.
- **One bad file does not abort the batch.** Bundle/compile/manifest
  failures are reported per file and skipped; the server still starts.
- **Errors are source-mapped.** A thrown error in a bundled step is
  reported at the original `.ts:line:col`, stack included.

---

## State and lifetime

What you can rely on between calls, when running under the MCP server.

### Two ways to keep state

A *session* is identified by the `session` argument (`instance:context`,
default `"default"`). All `run_script` calls and all extension tool calls
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

No cross-file or cross-extension shared state beyond what you `import`
directly. Share helpers by importing them; there is no implicit
cross-extension channel by design.

---

## Reference

### Manifest (`ToolManifest`)

| Field          | Wire (camelCase) | Default | Meaning |
|----------------|------------------|---------|---------|
| name           | `name`           | —       | Required, non-empty, unique across all loaded extensions. Binding/tool key. |
| description    | `description`    | none    | Shown in `tools/list`. |
| input schema   | `inputSchema`    | none    | JSON Schema; **enforced** — non-conforming calls rejected before the handler. |
| allow          | `allow`          | `{}`    | Capability manifest. |
| expose as tool | `exposeAsTool`   | `false` | Promote to a first-class MCP tool. |
| timeout ms     | `timeoutMs`      | none    | Per-invocation handler timeout (ms); enforced for every caller. |

### Capability manifest (`ToolAllow`)

| Field    | Wire        | Default | Meaning |
|----------|-------------|---------|---------|
| commands | `commands`  | `{}`    | name → command (shell string or spec object; `persistent` opt-in); alias `exec`. |
| net      | `net`       | `[]`    | host allow-list for `request` + `fetch`; empty = unrestricted. |

### Registration surface (JS globals)

`defineTool` · `defineFixtures` · `defineDefaults` · `bindSteps` · `Given` · `When` · `Then` ·
`defineStep` · `And` · `But` · `Before` · `After` · `BeforeAll` · `AfterAll` ·
`BeforeStep` · `AfterStep` · `defineParameterType` · `setDefaultTimeout` ·
`setDefinitionFunctionWrapper` · `setWorldConstructor` ·
`setParallelCanAssign` (inert) · `ferridriver.host` · `ferridriver.test`
(read-only)

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
   invoked as a promoted MCP tool or by another extension. The bound is
   cooperative: it fires while the handler is awaiting; a handler
   spinning the CPU without awaiting is halted by the session-wide
   wall-clock interrupt instead. Without `timeoutMs`, only the
   session-wide script timeout applies.
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
