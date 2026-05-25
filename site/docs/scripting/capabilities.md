# Capabilities

`allow` is a declarative, **default-deny** capability manifest enforced
in Rust at the binding boundary. The handler source cannot grant itself
authority it did not declare.

```ts
defineTool({
  name: "git.sha",
  allow: {
    commands: {
      headSha: "git -C ${repo} rev-parse HEAD",
      clone: {
        run: ["git", "clone", "${url}", "${dest}"],
        timeoutMs: 60000,
        env: ["SSH_AUTH_SOCK"],
        cwd: "/tmp",
        output: "text",
      },
    },
    net: ["api.github.com", "*.github.io"],
  },
  handler: async ({ commands, request }) => {
    const sha = await commands.run("headSha", { repo: "/srv/app" });
    return { sha: sha.trim() };
  },
});
```

| Field      | Default | Meaning |
|------------|---------|---------|
| `commands` | `{}`    | Name → command (shell string or spec object; `persistent` opt-in). Alias `exec`. |
| `net`      | `[]`    | Host allow-list for `request` + `fetch`; empty = unrestricted (back-compat). |

## `allow.commands` (alias `allow.exec`)

A name → command map. The handler may only run commands it declared
(default-deny). Each value is a **shorthand string** (a `sh -c` line)
or a **spec object**.

### Spec object fields

| Field        | Default | Meaning |
|--------------|---------|---------|
| `run`        | required | String ⇒ `sh -c <string>`. Array ⇒ direct exec, no shell. |
| `timeoutMs`  | none    | Per-call timeout. Process group killed on expiry. |
| `env`        | `[]`    | Server env names to pass through. Otherwise only `PATH` is kept. |
| `cwd`        | none    | Working directory. |
| `output`     | `"text"`| Stdout shape: `text` (trimmed string) / `json` (parsed; invalid throws) / `lines` (array of non-empty trimmed lines). |
| `persistent` | `false` | Run as a long-running process. See below. |

### Invocation

```ts
const out = await commands.run("name", { var1: "value1", var2: 42 });
```

Semantics:

- An **undeclared** `name` throws.
- Output past 8 MiB, non-zero exit, or timeout throws (the whole
  process group is killed on timeout).
- `${name}` is **strictly** substituted: every placeholder must be a
  supplied value and every value must be a string / number / boolean.
  A missing placeholder or an object / array value throws — no silent
  empty.
- **Shell form** single-quote-escapes each value; **argv form does not
  need to** — values are passed as literal arguments, so shell
  metacharacters in them are inert. Prefer argv unless you actually
  need a pipeline.

### Trust boundary

A shell-form `run` line is **author-supplied code with the server
process's authority** — `$(…)`, `&&`, `|`, redirection are live; only
the `${values}` are escaped. Argv form removes the shell entirely.

**Never** write a shell line that re-evaluates a value
(`sh -c "${x}"`, `eval ${x}`) — that defeats the escaping.

> Template = trusted code you commit; values = untrusted data.

### Persistent commands (servers, watchers)

Declare `persistent: true` for a long-running process. It is managed
with a different verb set; its lifetime is the **session's**, not the
call's:

```ts
allow: {
  commands: {
    dev: { run: "npm run dev", persistent: true },
  },
},
handler: async ({ commands }) => {
  await commands.start("dev");          // { name, pid }; idempotent if up
  const s = await commands.status("dev"); // { running, pid, exitCode, uptimeMs, stdout, stderr }
  await commands.stop("dev");           // SIGKILLs the process group
},
```

- `run` on a `persistent` spec (or `start` / `status` / `stop` on a
  one-shot spec) throws — the kinds don't mix.
- The process **survives a script-VM rebuild** (timeout, OOM, browser
  relaunch) so a dev server keeps running across calls. It is killed
  when the session ends (idle-TTL reap, explicit close, server
  shutdown), on `stop`, or if it exits on its own.
- `status` returns the last ~64 KiB of stdout / stderr as a ring buffer
  — a chatty server won't grow memory unbounded. Max 16 persistent
  processes per session.

## `allow.net`

A host allow-list scoping the handler's HTTP — both the `request`
client and the global `fetch` (they share one core, so the list binds
both).

| State        | Behavior |
|--------------|----------|
| Empty / absent | HTTP is unrestricted (back-compat default). |
| Non-empty    | `request` and `fetch` flip to **default-deny**. |

Each entry is an **exact host** (`api.box.com`) or a **leading-wildcard
suffix** (`*.box.com`, which also matches the bare apex `box.com`). Any
other host throws before the request is made.

The policy follows the **running handler**: a tool calling another tool,
or two tools running concurrently, each see only their own declared
list.

### What `allow.net` does NOT cover

`allow.net` scopes HTTP (`request` + `fetch`) **only**. `page` /
`context` browser navigation is a separate, deliberately ungated
authority — an automation tool must be able to navigate.

There is no `fs` capability: the handler context exposes no filesystem
handle, so an `fs` scope would gate nothing.

## Auditing what loaded

Call the built-in `ferridriver_extensions` MCP tool with
`include_schema: true` to see every loaded extension, its tools, their
`exposeAsTool` status, their `timeoutMs`, and their declared
capabilities — useful for security review before deploying a server.
