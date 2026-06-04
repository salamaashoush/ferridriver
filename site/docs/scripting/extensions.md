# Extensions

An **extension** is a single JavaScript or TypeScript file that
contributes at runtime to one or more ferridriver hosts:

- **MCP server** (`ferridriver mcp`) — registers tools via `tool(...)`.
- **BDD test runner** (`ferridriver bdd`) — registers Cucumber step
  definitions, hooks, and parameter types via `Given` / `When` / `Then` /
  `Before` / `After` / `defineParameterType` / `setWorldConstructor` / `setDefaultTimeout`.
- **Ad-hoc scripts** (`ferridriver run`, MCP `run_script`) — same VM,
  same globals.

The **same file** can serve all three. Branch on the `ferridriver.host`
global to decide which contributions apply where.

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
or `"script"`. Gate your registrations so one file does not pollute the
wrong host:

```ts
import { tool } from "ferridriver";
import { Given } from "@cucumber/cucumber";

if (ferridriver.host === "mcp") {
  tool({
    name: "box.login",
    description: "Log a test user in and return the session cookie",
    inputSchema: {
      type: "object",
      properties: { user: { type: "string" } },
      required: ["user"],
    },
    allow: { net: ["*.box.com"] },
    handler: async ({ args, request }) => {
      const res = await request.post("https://api.box.com/login", {
        data: { user: args.user },
      });
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

Dotted names are projected to namespaces: `tools.box.login(args)` and
`box.login(args)` both call a tool named `box.login`.

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

## Discovery and configuration

Extensions are configured in `ferridriver.toml`:

```toml
# ESM packages, package subpaths, files, or directories. Used by the
# MCP server (tools) AND, bundled alongside BDD step files, by the test
# runner (steps).
extensions = ["@acme/ferridriver-box", "@acme/ferridriver-box/login", "./extensions", "./tools/box-login.ts"]

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

Package entries use standard ESM package metadata:

- `exports` is preferred, including conditional `import` / `default`.
- `module` is accepted.
- `main` is accepted only when it points at an ESM source entry.
- `index.mjs`, `index.mts`, `index.ts`, and `index.js` are used as
  fallback entries; `.js` requires `"type": "module"`.

CommonJS entries are intentionally rejected. There is no ferridriver
manifest field; use normal `package.json` ESM metadata.

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
