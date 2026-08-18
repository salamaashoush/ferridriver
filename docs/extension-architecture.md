# Extension architecture

Why the extension system is shaped the way it is. `docs/extensions.md`
is the authoring contract — what an extension may declare and call.
This document is the reasoning behind it, and the record of what was
deliberately not built.

## The problem the design answers

A large third-party suite should run on ferridriver without editing its
own source. That suite imports specifiers — `@playwright/test`, a BDD
helper package, whatever its authors chose — and calls APIs that a
runner must provide. Two things follow:

1. Whatever ferridriver ships as "the way to extend it" has to reach
   every host. A package that only works under the MCP server is not an
   answer for a suite that runs under the test runner.
2. A package has to be able to answer for an import specifier. If the
   only way to reach a package's code is a path, every consumer has to
   be edited, which is exactly the thing being avoided.

## One loader, four hosts

`ferridriver_script::extension_load` is the single path from configured
`extensions` to loadable bytecode: resolve the specs, check each
package's declared preconditions, drop what cannot work, compile and
extract the rest. Every host — MCP, BDD, the Playwright-shaped test
runner, ad-hoc scripts — goes through it.

That is a correction, not a starting point. Each host used to decide for
itself: the MCP server resolved and gated, `ferridriver run` compiled
without gating, `ferridriver bdd` appended extension SOURCE to the step
bundle (so an extension was never manifest-extracted and the operator
ceiling never reached it), and the test runner loaded nothing at all.
A package that works under one host and not another is not a package.

`ferridriver.host` is what a file branches on to decide what to
CONTRIBUTE where — tools under `mcp`, steps under `bdd`, fixtures under
`test`. It never decides whether the file loads.

## Why extraction runs per host

A manifest is read by evaluating the file and slicing what it registered
off the registries. Because a file branches on the host, its
contribution is a function of the host: extracting under one and
reporting that as "the manifest" hides everything the other three would
have seen.

So extraction runs one pass per host. A pass needs its own QuickJS
RUNTIME, not merely its own context: `store_userdata` is keyed on the
runtime, every registry is userdata, and `registry::install` returns
early when it finds one — four contexts on a single runtime share one
registry and the second context never even receives its `defineTool`
global.

Compilation still happens once, in a context of its own.
`Module::declare` parses AND resolves, so declaring inside a host
context would leave whatever the loader answered registered under that
name, and the entries would link to it instead of to the provider that
evaluates in the pass.

A throw under one host is recorded against that host; the file still
loads everywhere else, because a session installs per file per host and
skips only the pairing that throws. A throw under every host is a
failure of the file.

## Package-owned import specifiers

A package claims a specifier in its manifest (`provides.modules`) and
serves it from one of its own files. The mechanism is deliberately
small:

- the provider is compiled with the SPECIFIER as its module name;
- the bundler marks the specifier external, so a consumer's chunk keeps
  the bare import;
- the resolver accepts it, and normalises an alias to its target's name.

An importer then links to the module QuickJS already holds under that
name. There is no facade module and no re-export list, and there is
exactly one instance per run — which is the property that matters: two
consumers each inlining their own copy would give a "shared" module two
states, and the sharing is usually the whole reason a package exists.

`require('<specifier>')` answers with the same namespace object, because
it is synchronous and cannot await a dynamic import; the object is
remembered when the provider evaluates.

### The rules, and why each exists

| Rule | Why |
|---|---|
| One specifier, one owner | Two packages serving one name means the answer depends on load order. |
| The runtime's own specifiers are not claimable | `@playwright/test` must mean the same thing whether or not an extension happens to be loaded. The reserved set covers `@playwright/*`, `playwright`, `playwright-core`, `@cucumber/*`, `node:*`, `@ferridriver/*` and the bare twin of every reserved `node:` name. |
| An alias may only target its own package's specifier | Otherwise a package could re-point someone else's name. |
| The operator's `moduleAliases` outrank a package | Configuration beats a default, and says so in a warning. |
| Providers evaluate in dependency order; a cycle is an error | The module that lost the tie would see the other's exports half-initialised. |
| A provider that fails to evaluate aborts the session | Every consumer's import depends on it; skipping it leaves a specifier that silently answers nothing. |
| The claim table seals on first use | A session created before a specifier arrived keeps a resolver that never heard of it, and a bundle keyed before it would have inlined what should have stayed external. |

`[extensions.policy] modules` / `allowModules` is the operator's ceiling
over claiming at all: a deployment can refuse the mechanism outright, or
name the specifiers it will accept.

## Contributing onto the base fixture chain

`defineFixtures` appends to fixture set 0 — the chain `ferridriver.test`,
`_baseTest` and the ambient cucumber registrars all resolve from —
rather than building a new chain and rebinding `ferridriver.test`. The
rebind is not merely worse, it does not work: a native module is
instantiated once per VM and its export slots hold the values copied at
evaluation, so every importer keeps the `test` object it linked against.
`ferridriver.test` is therefore read-only, and the mutation is in place.

The chain seals when the last extension has installed. After that every
`test.extend()` copies it, so a later append would be visible to the
suites that had not derived a chain yet and invisible to those that had —
the divergence the seal converts into a refusal.

| Rule | Why |
|---|---|
| The one `test` object per VM is mutated, never replaced | Export slots hold copied values; a replacement reaches nobody. |
| The base chain seals after the last extension installs | `test.extend()` copies the chain, so a late append splits the suite in two. |
| `[extensions.policy] fixtures` covers `defineFixtures` only | `test.extend` / `mergeTests` / `expect.extend` change nothing a suite did not ask for by importing the package. |
| A ceiling refusal fails the run, never skips the package | Skipping leaves the deployment running without authority the operator withheld, behind one warning line. |

## What was considered and not built

- **A plugin manifest with its own module format** (VS Code's shape).
  Rejected: the packages this has to accept are ordinary npm packages
  with ordinary imports. Anything requiring a bespoke wrapper module is
  a rewrite of the suite, which is the thing being avoided.
- **Serving a claimed specifier through a generated facade module**
  (`export { … } from '<provider>'`). Rejected once the provider could
  simply BE the specifier: a facade needs the export list captured at
  extraction, cannot carry a default export without knowing whether one
  exists, and adds a second module object per specifier.
- **A permission prompt / capability negotiation at load** (Deno's
  shape). Not built: the operator ceiling is static config, checked at
  registration, which is what a CI run can reason about.
- **WASM or a separate process per extension.** Not built: the value of
  an extension here is that it shares the session's own objects — the
  page, the fixtures, the step registry. An isolate boundary would
  remove exactly what makes it useful.

## Where the pieces live

| Concern | Module |
|---|---|
| Resolve specs to files, per-package manifests | `ferridriver_script::discover` |
| Declared preconditions (`requires`, `settings`) | `ferridriver_script::requirements` |
| Gate + compile + bindings, for every host | `ferridriver_script::extension_load` |
| Specifier claims and their rules | `ferridriver_script::provided_modules` |
| Compile, extract, cache | `ferridriver_script::bundle` |
| Registration surfaces (`defineTool`, cucumber, `test`) | `ferridriver_script::bindings` |
