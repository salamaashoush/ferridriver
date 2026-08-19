# Extensions

An **extension** is a single JavaScript or TypeScript file that
contributes at runtime to one or more ferridriver hosts:

- **MCP server** (`ferridriver mcp`) — registers tools via `tool(...)`.
- **BDD test runner** (`ferridriver bdd`) — registers Cucumber step
  definitions, hooks, and parameter types via `Given` / `When` / `Then` /
  `Before` / `After` / `defineParameterType` / `setWorldConstructor` / `setDefaultTimeout`.
- **Test runner** (`ferridriver test`) — contributes fixtures onto the
  base `test` chain with `defineFixtures(...)`, so a spec receives them
  without importing anything.
- **Ad-hoc scripts** (`ferridriver run`, MCP `run_script`) — same VM,
  same globals.

The **same file** can serve all four. Branch on the `ferridriver.host`
global to decide which contributions apply where, or narrow an entry to
some hosts in the package manifest so it never loads elsewhere.

Every host also reads `defineDefaults(...)`, which lets a package supply
configuration defaults instead of asking every suite to copy them.

This page is the tour. The complete authoring contract — every
contribution point, the capability model, the operator ceiling, the
package manifest — is `docs/extensions.md` in the repository.

## Mental model

Registration functions (`tool`, `Given`, `Before`, …) are
**native Rust functions**, not JS shims. Calling them at the top level
of your module pushes an entry into a Rust-owned registry. Hosts read
back the kinds they care about and invoke your handler natively — the
MCP tool path and the BDD step path use the same dispatch mechanism.

Implication: **all contribution happens as a side effect of the
module's top-level code running once.** There is no `activate()` /
`onLoad()` hook — ES module top-level *is* your load hook.

## Detecting the host

`ferridriver.host` is a string set once per session: `"mcp"`, `"bdd"`,
`"test"`, or `"script"`. Gate your registrations so one file does not
pollute the wrong host:

```ts
import { tool } from "ferridriver";
import { Given } from "@cucumber/cucumber";

if (ferridriver.host === "mcp") {
  tool({
    name: "acme.login",
    description: "Log a test user in and return the session cookie",
    inputSchema: {
      type: "object",
      properties: { user: { type: "string" } },
      required: ["user"],
    },
    allow: { net: ["*.acme.com"] },
    handler: async ({ args, request }) => {
      const res = await request.post("https://api.acme.com/login", {
        data: { user: args.user },
      });
      return { cookie: (await res.json()).cookie };
    },
  });
}

if (ferridriver.host === "bdd") {
  Given("I am logged in as {string}", async function (user: string) {
    await this.page.goto(`https://app.acme.com/login?u=${user}`);
  });
}
```

Registering for the wrong host is harmless (the host ignores kinds it
does not consume), but it wastes work and muddies intent.

## `tool`

Two equivalent forms:

```ts
// Inline handler on the manifest object
tool({
  name: "vendor.area.action",   // required, globally unique
  description: "...",            // optional, surfaced in tools/list
  inputSchema: { ... },          // optional JSON Schema; ENFORCED
  exposeAsMcpTool: true,         // optional, default false
  timeoutMs: 30000,              // optional per-invocation timeout
  allow: { ... },                // optional capability manifest
  handler: async (ctx) => { ... },
});

// Or manifest + separate handler
tool(manifest, async (ctx) => { ... });
```

`defineTool(...)` remains as a global compatibility alias for
`ferridriver.tool(...)` / `tool(...)`.

### Fields

| Field          | Wire (camelCase) | Default | Meaning |
|----------------|------------------|---------|---------|
| name           | `name`           | —       | Required, non-empty, unique across all loaded extensions. Binding / tool key. |
| description    | `description`    | none    | Shown in MCP `tools/list`. |
| input schema   | `inputSchema`    | none    | JSON Schema. **Enforced** — non-conforming calls rejected before the handler. |
| allow          | `allow`          | `{}`    | Capability manifest. See [Capabilities](/scripting/capabilities). |
| expose as MCP tool | `exposeAsMcpTool` | `false` | Promote to a first-class MCP tool. |
| timeout ms     | `timeoutMs`      | none    | Per-invocation handler timeout (ms); enforced for every caller. |

### `exposeAsMcpTool`

- `false` (default): the tool is callable from other extension / script
  code as `await tools.vendor.area.action(args)`, but **not** advertised
  in the MCP server's `tools/list`. Use for shared helpers.
- `true`: additionally promoted to a first-class MCP tool. `name`,
  `description`, and `inputSchema` become the tool contract. The tool
  call and the script binding route through the same handler.

Dotted names are projected to namespaces: `tools.acme.login(args)` and
`acme.login(args)` both call a tool named `acme.login`.

### Handler context

The handler receives one object:

| Field      | Type                          | Notes |
|------------|-------------------------------|-------|
| `args`     | the caller's argument         | For a promoted tool, the MCP `arguments` object. |
| `page`     | `Page \| undefined`           | The live browser page for the session. |
| `context`  | `BrowserContext \| undefined` | The session's browser context. |
| `request`  | `HttpClient \| undefined`     | HTTP client. Net-restricted if `allow.net` is non-empty. |
| `commands` | `PluginCommands`              | `.run(name, vars?)` — runs a declared command. |

Return any JSON-serialisable value; it becomes the tool result.

When the manifest declares `inputSchema`, the caller's `args` are
validated against it (full JSON Schema, via the `jsonschema` crate)
**before** the handler runs; a non-conforming call is rejected as a
tool error and the handler is never entered.

## Contributing fixtures

`defineFixtures` adds fixtures to the base `test` chain, so a suite that
never imports the package still receives them through the `test` it
already imports:

```ts
// the package
defineFixtures<{ deployment: string }>({
  deployment: async ({}, use) => { await use("staging"); },
});
```

```ts
// the suite, unchanged
import { test, expect } from "@ferridriver/test";

test("knows where it is pointed", async ({ deployment }) => {
  expect(deployment).toBe("staging");
});
```

Entries and override rules are `test.extend`'s: packages compose in load
order, a later same-name entry shadows the earlier one, and that entry's
own same-name dependency resolves to the registration it shadows.

The base chain seals once every extension has installed — each
`test.extend()` copies it from then on, so a later contribution would
reach some suites and not others. A `defineFixtures` from a spec bundle,
a step file or a script throws and points at `test.extend()`.

An operator can close the door entirely with `[extensions.policy]
fixtures = false`, which refuses the contribution and fails the run
naming the key. It covers `defineFixtures` only: a package's own
`test.extend` / `mergeTests` chains and its `expect.extend` matchers are
never clamped.

## Contributing config defaults

`defineDefaults` supplies defaults for the run's configuration, so a
suite that adopts a package does not have to copy its settings into
every `ferridriver.toml`:

```ts
defineDefaults({
  test: {
    timeout: 60_000,
    browser: { use: { testIdAttribute: "data-qa" } },
  },
});
```

It is the **lowest** layer: every config file, every `FERRIDRIVER_*`
override and every CLI flag still wins, and two packages setting the
same key compose in load order. `ferridriver config` names the package a
value came from just as it names a file.

The set of extensions is itself configuration, so the run resolves the
layer stack twice: once to learn which packages to load, then again with
what they contributed underneath. That is why a contribution may not set
`extensions`, `bundler`, `scripting` or `[test].moduleAliases` — each
decides how the contributing package was found, compiled or trusted, and
each is refused by name. A key the schema does not have is refused too,
naming the key: a typo in a dependency's defaults is one nobody would
otherwise see.

Contributions are read before the first session exists; calling
`defineDefaults` from a spec, a step file or a script throws. An
operator closes the door with `[extensions.policy] configDefaults =
false`.

## Discovery and configuration

Extensions are configured in `ferridriver.toml`:

```toml
# ESM packages, package subpaths, files, or directories. Used by the
# MCP server (tools) AND, bundled alongside BDD step files, by the test
# runner (steps).
extensions = ["@acme/ferridriver-auth", "@acme/ferridriver-auth/login", "./extensions", "./tools/acme-login.ts"]

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

`ferridriver bdd` bundles discovered step files **and** the configured
`extensions` into one module, so an extension's `Given` / `When` /
`Then` are available to tests exactly like a step file's.

A package that declares a `ferridriver` field in its `package.json`
names its own entries, which is what a package with several tool modules
plus a shared `lib/` needs — Node's single-entry fields can only name
one:

```json
{
  "ferridriver": {
    "entries": [
      "./src/fixtures.ts",
      { "path": "./src/mcp-tools.ts", "hosts": ["mcp"] }
    ]
  }
}
```

An item is a path, or an object that narrows the entry to some hosts
(`mcp`, `bdd`, `test`, `script`) or gives it its own `requires`. An
entry's `requires` are checked only where the entry runs, so an MCP-only
dependency does not hold the package back under `ferridriver test`.

Without that field, standard ESM package metadata applies:

- `exports` is preferred, including conditional `import` / `default`.
- `module` is accepted.
- `main` is accepted only when it points at an ESM source entry.
- `index.mjs`, `index.mts`, `index.ts`, and `index.js` are used as
  fallback entries; `.js` requires `"type": "module"`.

CommonJS entries are intentionally rejected.

Both discovery paths (MCP loader and BDD runner) share the same package
resolver, accepted-extension set, and recursive walk. A package, package
subpath, `.tsx`, or `.cts` extension is visible identically to both
hosts.

## Runtime guarantees

1. **`inputSchema` is enforced.** Calls whose arguments do not match
   the declared schema are rejected before your handler runs. A schema
   that is itself invalid JSON Schema is reported, not silently
   ignored.
2. **Tool names are unique and non-empty.** A duplicate or blank `name`
   fails that extension at load time. A name that collides with a
   built-in or another loaded tool is not exposed. Namespace your names
   (`vendor.area.action`).
3. **Tool failures are reported as errors.** When your handler throws,
   the caller gets an error result (not a "success" containing an
   error string), with the message first and full detail after. (Plain
   `run_script` is different: it always succeeds and you inspect its
   `status` field.)
4. **`timeoutMs` is honoured for every caller** — whether the tool is
   invoked as a promoted MCP tool or by another extension. Without it,
   only the session-wide script timeout applies.
5. **Discovery is recursive and uniform.** Configured ESM packages,
   package subpaths, files, and directories resolve the same way for
   the MCP server and the test runner. Directories are scanned
   recursively; `.js .cjs .mjs .jsx .ts .cts .mts .tsx` source files
   are all accepted.
6. **You can inspect what loaded.** The built-in
   `ferridriver_extensions` MCP tool lists every loaded extension file,
   its tools, descriptions, whether each is exposed, its timeout, and
   its declared capabilities.

## What is intentionally not provided

- **`activate()` / `onLoad()` hook.** Module top-level *is* the load
  hook; ES module evaluation runs your registrations.
- **Plugin dependency / ordering.** The loader sorts files
  deterministically by path; cross-file load ordering is not
  configurable.
- **Cross-plugin shared state channel.** Share helpers via
  `import` statements (rolldown will resolve and bundle them); there is
  no global registry.
- **Middleware / hook pipeline (Rollup-style ordered hooks).** Not
  shipped — no consumer today justifies the abstraction. The capability
  boundary is the natural insertion point if one ever does.

See [Capabilities](/scripting/capabilities) for `allow.commands` and
`allow.net`. See [BDD JS / TS API](/scripting/bdd-js-api) for `Given`
/ `When` / `Then` reference.
