import { expect, test } from "bun:test";
import { webkit } from "../index.js";

function driver({ classic = false, hang = false, rejectDelete = false } = {}) {
  const requests: { method: string; path: string; authorization: string | null; body: unknown }[] = [];
  const commands: string[] = [];
  const upgrades: (string | null)[] = [];
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request, server) {
      const path = new URL(request.url).pathname;
      if (request.headers.get("upgrade") === "websocket") {
        upgrades.push(request.headers.get("authorization"));
        if (server.upgrade(request)) return;
        return new Response(null, { status: 400 });
      }
      const body = request.method === "POST" ? await request.json() : null;
      requests.push({ method: request.method, path, authorization: request.headers.get("authorization"), body });
      if (rejectDelete && request.method === "DELETE") {
        return Response.json({ value: { error: "unknown error", message: "cleanup rejected" } }, { status: 500 });
      }
      return Response.json({ value: request.method === "POST" ? {
        sessionId: "mobile-contract",
        capabilities: {
          browserName: "safari", browserVersion: "contract",
          ...(classic ? {} : { webSocketUrl: `ws://127.0.0.1:${server.port}/session/mobile-contract` }),
        },
      } : null });
    },
    websocket: {
      message(socket, message) {
        const command = JSON.parse(String(message));
        commands.push(command.method);
        if (hang) return;
        socket.send(JSON.stringify({ type: "success", id: command.id,
          result: command.method === "browsingContext.getTree" ? {
            contexts: [{ context: "device", userContext: "default", url: "about:blank", parent: null, children: [] }],
          } : {},
        }));
      },
    },
  });
  return { server, requests, commands, upgrades, endpoint: `http://127.0.0.1:${server.port}/wd/hub` };
}

test("WebDriver closes its negotiated session and preserves cloud capabilities", async () => {
  const mock = driver();
  try {
    const browser = await webkit().connect(mock.endpoint, {
      headers: { authorization: "Bearer contract-token" },
      capabilities: { platformName: "iOS", "bstack:options": { deviceName: "example-device", realMobile: true } },
      timeout: 1000,
    });
    try { expect(browser.version()).toBe("safari/contract"); }
    finally { await browser.close(); }
    expect(mock.commands).not.toContain("session.new");
    expect(mock.commands).not.toContain("browsingContext.setViewport");
    expect(mock.upgrades).toEqual(["Bearer contract-token"]);
    expect(mock.requests.map(request => [request.method, request.path])).toEqual([
      ["POST", "/wd/hub/session"], ["DELETE", "/wd/hub/session/mobile-contract"],
    ]);
    expect(mock.requests.every(request => request.authorization === "Bearer contract-token")).toBe(true);
    expect(mock.requests[0].body).toMatchObject({ capabilities: { alwaysMatch: {
      webSocketUrl: true, platformName: "iOS", "bstack:options": { deviceName: "example-device", realMobile: true },
    } } });
  } finally { mock.server.stop(true); }
});

test("WebDriver deletes a Classic-only session before reporting unsupported", async () => {
  const mock = driver({ classic: true });
  try {
    await expect(webkit().connect(mock.endpoint, { timeout: 1000 })).rejects.toThrow(/webSocketUrl/);
    expect(mock.requests.map(request => request.method)).toEqual(["POST", "DELETE"]);
  } finally { mock.server.stop(true); }
});

test("WebDriver timeout includes BiDi initialization and releases the session", async () => {
  const mock = driver({ hang: true });
  try {
    await expect(webkit().connect(mock.endpoint, { timeout: 100 })).rejects.toThrow(/Timeout 100ms/);
    expect(mock.commands).toEqual(["session.subscribe"]);
    expect(mock.requests.map(request => request.method)).toEqual(["POST", "DELETE"]);
  } finally { mock.server.stop(true); }
});

test("WebDriver deletion failure reaches browser.close callers", async () => {
  const mock = driver({ rejectDelete: true });
  try {
    const browser = await webkit().connect(mock.endpoint, { timeout: 1000 });
    await expect(browser.close()).rejects.toThrow(/WebDriver DELETE \/session/);
  } finally { mock.server.stop(true); }
});
