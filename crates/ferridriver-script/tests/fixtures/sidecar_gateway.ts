// A gateway extension: surfaces a declared sidecar's request/response and
// pushed-event channels as MCP tools. The sidecar is referenced by its
// declared name only — operators wire the real process in `ferridriver.toml`
// under `[[sidecars]]`; this file never spawns anything itself.
//
// Exercises the full sidecar JS surface: connect, send (request/response),
// on + the returned unsubscribe function, and bridging a pushed event back
// to an awaited Promise. Each tool sets `exposeAsTool` so it is callable over
// the MCP server.

interface SidecarHandle {
  send(method: string, params?: unknown): Promise<Record<string, unknown>>;
  on(event: string, cb: (params: unknown) => void): () => void;
  once(event: string): Promise<unknown>;
  off(event: string, cb?: (params: unknown) => void): void;
  close(): Promise<void>;
  name(): string;
}

declare const sidecars: { connect(name: string): Promise<SidecarHandle> };
declare function defineTool(spec: unknown): void;

const SIDECAR = "gateway";

function requireString(value: unknown, field: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`${field} (non-empty string) is required`);
  }
  return value;
}

// One warm connection per session; the transport itself dedupes by name, so
// repeated connects are cheap and this just avoids re-awaiting in each tool.
let cached: Promise<SidecarHandle> | undefined;
function gateway(): Promise<SidecarHandle> {
  if (!cached) {
    cached = sidecars.connect(SIDECAR);
  }
  return cached;
}

defineTool({
  name: "gateway.ping",
  description: "Health-check the gateway sidecar.",
  exposeAsTool: true,
  inputSchema: { type: "object", properties: {}, additionalProperties: false },
  handler: async () => {
    const sc = await gateway();
    const r = await sc.send("ping");
    return { ok: r.ok === true, name: sc.name() };
  },
});

defineTool({
  name: "gateway.call",
  description: "Forward an arbitrary { method, params } to the gateway sidecar.",
  exposeAsTool: true,
  inputSchema: {
    type: "object",
    properties: {
      method: { type: "string", description: "Sidecar method name." },
      params: { description: "Arbitrary JSON params for the method." },
    },
    required: ["method"],
  },
  handler: async ({ args }) => {
    const method = requireString(args?.method, "gateway.call: 'method'");
    const sc = await gateway();
    return await sc.send(method, args?.params);
  },
});

defineTool({
  name: "gateway.roundtripEvent",
  description:
    "Register a listener, trigger the sidecar to push an event, and resolve " +
    "with that event's params. Proves the on()/pushed-event path end to end.",
  exposeAsTool: true,
  inputSchema: {
    type: "object",
    properties: {
      event: { type: "string", description: "Event name to await." },
      trigger: { type: "string", description: "Method that makes the sidecar emit (default 'emit')." },
      params: { description: "Params for the trigger method." },
    },
    required: ["event"],
  },
  handler: async ({ args }) => {
    const event = requireString(args?.event, "gateway.roundtripEvent: 'event'");
    const trigger = requireString(args?.trigger ?? "emit", "trigger");
    const sc = await gateway();

    // Register BEFORE triggering so the listener can never miss the push.
    let resolve: (params: unknown) => void;
    let reject: (err: unknown) => void;
    const received = new Promise<unknown>((res, rej) => {
      resolve = res;
      reject = rej;
    });
    const off = sc.on(event, (params) => {
      off();
      resolve(params);
    });

    try {
      await sc.send(trigger, args?.params);
    } catch (err) {
      off();
      reject!(err);
    }
    return await received;
  },
});

defineTool({
  name: "gateway.close",
  description: "Close the gateway sidecar transport.",
  exposeAsTool: true,
  inputSchema: { type: "object", properties: {}, additionalProperties: false },
  handler: async () => {
    if (cached) {
      const sc = await cached;
      cached = undefined;
      await sc.close();
    }
    return { closed: true };
  },
});
