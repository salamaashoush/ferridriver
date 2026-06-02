# Sidecars

A **sidecar** is a long-lived child process that ferridriver launches and
drives over a private pipe, exposed to scripts as `sidecars.connect(name)`.
Use one to hand work to a tool written in another language — a formatter, a
data generator, a domain service — without shelling out per call or exposing
arbitrary process spawning to scripts.

The transport is the same one the browser backends use internally: the child
reads requests on **fd 3** and writes responses on **fd 4**, each frame a
UTF-8 JSON object terminated by a single `\0` byte. ferridriver wires the
file descriptors; the child never sees them as arguments.

## Declaring a sidecar

Sidecars are declared in `ferridriver.toml` at the top level (a sibling of
`[mcp]` and `[test]`), because both the test runner and the `run` / MCP paths
consume them. A script can only connect to a sidecar that was declared —
there is no way to spawn an arbitrary process from JavaScript.

```toml
[[sidecars]]
name = "formatter"
command = ["my-formatter", "--serve"]
cwd = "./tools"
startupTimeoutMs = 5000

[sidecars.env]
LOG_LEVEL = "info"
```

| Field             | Type                  | Required | Meaning |
|-------------------|-----------------------|----------|---------|
| `name`            | string                | yes      | The name scripts connect by. Must be unique. |
| `command`         | string[]              | yes      | Program + arguments. `command[0]` is the program. Must be non-empty. |
| `env`             | table<string,string>  | no       | Extra environment variables, merged onto the inherited environment. |
| `cwd`             | string                | no       | Working directory for the child. Defaults to the parent's cwd. |
| `startupTimeoutMs`| integer               | no       | How long to wait for the child before failing. Defaults to `5000`. |

A duplicate `name` or an empty `command` is a configuration error and fails
the load.

## The JavaScript API

`sidecars.connect(name)` resolves a handle. The child is spawned on the first
connect for a name; later connects in the same session reuse the warm
process. A handle exposes request/response calls plus pushed events.

```ts
const sc = await sidecars.connect("formatter");

// Request / response: send a method + optional params, await the result.
const result = await sc.send("format", { code, language: "rust" });

// Many at once: issue a batch in a single call, results returned in order.
const [a, b] = await sc.sendMany([
  { method: "format", params: { code: codeA, language: "rust" } },
  { method: "format", params: { code: codeB, language: "rust" } },
]);

// Pushed events: the child can write id-less frames at any time.
const off = sc.on("progress", (params) => {
  console.log("progress", params.percent);
});

// Wait for the next single event.
const summary = await sc.once("done");

off();              // remove that one listener
sc.off("progress"); // or remove every listener for an event

await sc.close();   // stop the pump, close the pipe, reap the child
```

### `send(method, params?) → Promise<result>`

Sends `{ id, method, params }` and resolves with the matching response's
`result`. Rejects on a child `{ error }` reply, a timeout, or a closed
transport. Requests are correlated by id, so concurrent `send`s are safe.

### `sendMany(calls) → Promise<result[]>`

Issues a batch of requests in one call. `calls` is an array of
`{ method, params? }`; the result array is positional (`result[i]` is the
reply to `calls[i]`). Rejects on the first child `{ error }` reply, timeout,
or closed transport — the same semantics as
`Promise.all(calls.map((c) => sc.send(c.method, c.params)))`, which it
replaces.

Prefer it when issuing many calls at once: the whole batch is written in a
single pipe write and resolves through one promise instead of one per call,
so it avoids the per-call promise overhead that otherwise dominates a large
fan-out. The win grows with the real child's ability to pipeline — measured
at roughly 1.7x over `Promise.all` against a trivial echo child and ~2.9x
against a real request-serving child.

### `on(event, cb) → () => void`

Registers `cb` for every pushed frame whose `method` equals `event`; `cb`
receives the frame's `params`. Returns a function that removes exactly this
listener. The first `on` (or `once`) starts a single background pump per
handle that dispatches events into the script context.

### `once(event) → Promise<params>`

Resolves with the next matching event's `params`, then auto-unsubscribes.

### `off(event, cb?)`

With `cb`, removes that one listener (by identity); without it, removes every
listener for `event`.

### `close() → Promise<void>`

Stops the event pump, closes the request pipe (the child sees EOF), and reaps
the process. Idempotent.

## The wire protocol

Implement the child in any language; it only has to speak NUL-delimited JSON
on fd 3 (read) and fd 4 (write):

- Request: `{"id": <number>, "method": <string>, "params": <any>}`
- Response: `{"id": <number>, "result": <any>}` or
  `{"id": <number>, "error": {"code": <number>, "message": <string>}}`
- Event (server-pushed, no `id`): `{"method": <string>, "params": <any>}`

The event channel is bounded; if a listener falls far enough behind that the
buffer laps it, those events are dropped (a warning is logged). Treat events
as best-effort notifications, not a guaranteed-delivery queue.
