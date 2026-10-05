import { expect, test } from "bun:test";
import { chromium, firefox, safari } from "../index.js";

for (const rejectInput of [false, true]) {
  test(`XCUITest sends keyboard input natively and restores web context, rejected=${rejectInput}`, async () => {
    let context = "WEBVIEW_device";
    let inputContext = "";
    const keys: string[] = [];
    const server = Bun.serve({hostname: "127.0.0.1", port: 0, async fetch(request) {
      const path = new URL(request.url).pathname;
      const body = request.method === "POST" ? await request.json() : null;
      let value: unknown = null;
      if (path === "/session") {
        value = {sessionId: "input", capabilities: {browserName: "Safari", browserVersion: "26.2",
          platformName: "iOS", automationName: "XCUITest"}};
      } else if (path.endsWith("/window/handles")) value = ["device"];
      else if (path.endsWith("/context")) context = body.name;
      else if (path.endsWith("/execute/sync")) value = body.script.includes("document.visibilityState") ? true : null;
      else if (path.endsWith("/actions")) {
        inputContext = context;
        if (context !== "NATIVE_APP" || rejectInput) {
          return Response.json({value: {error: "unknown error", message: "native input rejected"}}, {status: 500});
        }
        for (const source of body.actions) for (const action of source.actions) {
          if (action.type === "keyDown" || action.type === "keyUp") keys.push(`${action.type}:${action.value}`);
        }
      }
      return Response.json({value});
    }});
    try {
      const browser = await safari().connect(`http://127.0.0.1:${server.port}`, {timeout: 1000});
      try {
        const page = (await browser.contexts()[0].pages())[0];
        if (rejectInput) await expect(page.keyboard.press("a")).rejects.toThrow("native input rejected");
        else await page.keyboard.press("a");
        expect(inputContext).toBe("NATIVE_APP");
        expect(context).toBe("WEBVIEW_device");
        expect(keys).toEqual(rejectInput ? [] : ["keyDown:a", "keyUp:a"]);
      } finally { await browser.close(); }
    } finally { server.stop(true); }
  });
}

function driver({ classic = false, hang = false, rejectDelete = false, browserName = "firefox", browserVersion = "contract",
  chromeCapabilities = undefined as unknown, extensionStatus = 200, handles = () => ["device"] } = {}) {
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
      if (request.method === "GET" && path.endsWith("/window/handles")) {
        return Response.json({ value: handles() });
      }
      if (request.method === "GET" && path.endsWith("/title")) {
        return Response.json({ value: "sashoush's Classic session" });
      }
      if (body?.script === "mobile: getChromeCapabilities") {
        return Response.json({value: chromeCapabilities}, {status: extensionStatus});
      }
      return Response.json({ value: request.method === "POST" ? {
        sessionId: "mobile-contract",
        capabilities: {
          browserName, browserVersion,
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
    const browser = await firefox().connect(mock.endpoint, {
      headers: { authorization: "Bearer contract-token" },
      capabilities: { platformName: "iOS", "bstack:options": { deviceName: "example-device", realMobile: true } },
      timeout: 1000,
    });
    try { expect(browser.version()).toBe("firefox/contract"); }
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

for (const extensionStatus of [200, 404, 405, 501]) {
  test(`Android uses its existing Classic session when CDP is unavailable, status=${extensionStatus}`, async () => {
    const mock = driver({classic: true, browserName: "Chrome", browserVersion: extensionStatus === 200 ? "" : "152.0.7977.82",
      chromeCapabilities: extensionStatus === 200 ? {browserName: "chrome", browserVersion: "152.0.7977.82"} :
        {error: "unsupported operation", message: "extension unavailable"}, extensionStatus});
    try {
      const browser = await chromium().connect(mock.endpoint, {timeout: 1000,
        headers: {authorization: "Bearer contract-token"},
        capabilities: {platformName: "Android", "appium:options": {automationName: "UiAutomator2"}}});
      try {
        expect(browser.version()).toBe("Chrome/152.0.7977.82");
        const pages = await browser.contexts()[0].pages();
        expect(await pages[0].title()).toBe("sashoush's Classic session");
      } finally {await browser.close();}
      expect(mock.requests.filter(request => request.path === "/wd/hub/session").length).toBe(1);
      expect(mock.requests.filter(request => request.path.endsWith("/execute/sync")).length).toBe(1);
      expect(mock.requests.filter(request => request.method === "DELETE").map(request => request.path))
        .toEqual(["/wd/hub/session/mobile-contract"]);
      expect(mock.requests.every(request => request.authorization === "Bearer contract-token")).toBe(true);
      expect(mock.upgrades).toEqual([]);
    } finally {mock.server.stop(true);}
  });
}

for (const extensionStatus of [401, 403, 429, 503]) {
  test(`Android provider failures cannot become successful Classic connections, status=${extensionStatus}`, async () => {
    const mock = driver({classic: true, browserName: "Chrome",
      chromeCapabilities: {error: "unsupported operation", message: "provider denied request"}, extensionStatus});
    try {
      await expect(chromium().connect(mock.endpoint, {timeout: 1000,
        capabilities: {platformName: "Android", "appium:options": {automationName: "UiAutomator2"}}}))
        .rejects.toThrow(String(extensionStatus));
      expect(mock.requests.map(request => request.method)).toEqual(["POST", "POST", "DELETE"]);
      expect(mock.upgrades).toEqual([]);
    } finally {mock.server.stop(true);}
  });
}

test("explicit Classic Android obtains its browser version without using CDP", async () => {
  const mock = driver({classic: true, browserName: "Chrome", browserVersion: "",
    chromeCapabilities: {browserVersion: "152.0.7977.82", "goog:chromeOptions": {debuggerAddress: "127.0.0.1:9"}}});
  try {
    const browser = await chromium().connect(mock.endpoint, {timeout: 1000,
      capabilities: {webSocketUrl: false, platformName: "Android", "appium:options": {automationName: "UiAutomator2"}}});
    try {expect(browser.version()).toBe("Chrome/152.0.7977.82");}
    finally {await browser.close();}
    expect(mock.upgrades).toEqual([]);
    expect(mock.requests.filter(request => request.path.endsWith("/execute/sync")).length).toBe(1);
    expect(mock.requests.filter(request => request.method === "DELETE").length).toBe(1);
  } finally {mock.server.stop(true);}
});

for (const chromeCapabilities of [null, [], {"goog:chromeOptions": null},
  {"goog:chromeOptions": {debuggerAddress: ""}}, {"goog:chromeOptions": {debuggerAddress: "host/path"}}]) {
  test(`Android rejects malformed Chrome discovery capabilities ${JSON.stringify(chromeCapabilities)}`, async () => {
    const mock = driver({classic: true, browserName: "Chrome", chromeCapabilities});
    try {
      await expect(chromium().connect(mock.endpoint, {timeout: 1000,
        capabilities: {platformName: "Android", "appium:options": {automationName: "UiAutomator2"}}}))
        .rejects.toThrow(/Appium Chrome capabilities/);
      expect(mock.requests.map(request => request.method)).toEqual(["POST", "POST", "DELETE"]);
    } finally {mock.server.stop(true);}
  });
}

test("Classic context pages discovers reordered handles and removes closed tabs", async () => {
  let handles = ["old"];
  const mock = driver({ classic: true, browserName: "safari", handles: () => handles });
  try {
    const browser = await safari().connect(mock.endpoint, { timeout: 1000 });
    try {
      const context = browser.contexts()[0];
      const old = (await context.pages())[0];
      handles = ["new", "old"];
      const discovered = await context.pages();
      expect(discovered.length).toBe(2);
      expect(old.isClosed()).toBe(false);
      handles = ["new"];
      expect((await context.pages()).length).toBe(1);
      expect(old.isClosed()).toBe(true);
      expect(discovered[1].isClosed()).toBe(false);
      handles = ["replacement"];
      expect((await context.pages()).length).toBe(1);
      expect(discovered[1].isClosed()).toBe(true);
    } finally { await browser.close(); }
  } finally { mock.server.stop(true); }
});

test("BiDi connection deletes a Classic-only session before reporting unsupported", async () => {
  const mock = driver({ classic: true });
  try {
    await expect(firefox().connect(mock.endpoint, { timeout: 1000, capabilities: { webSocketUrl: true } })).rejects.toThrow(/webSocketUrl/);
    expect(mock.requests.map(request => request.method)).toEqual(["POST", "DELETE"]);
  } finally { mock.server.stop(true); }
});

for (const [factory, browserName] of [[chromium, "chrome"], [firefox, "firefox"]] as const) {
  for (const webSocketUrl of [undefined, false]) {
    test(`${browserName} adopts the created Classic session, webSocketUrl=${webSocketUrl}`, async () => {
      const mock = driver({ classic: true, browserName });
      try {
        const browser = await factory().connect(mock.endpoint, {
          timeout: 1000,
          headers: { authorization: "Bearer contract-token" },
          capabilities: { ...(webSocketUrl === undefined ? {} : { webSocketUrl }), "acme:options": { region: "test" } },
        });
        try {
          expect(browser.version()).toBe(`${browserName}/contract`);
          const pages = await browser.contexts()[0].pages();
          expect(pages.length).toBe(1);
          expect(await pages[0].title()).toBe("sashoush's Classic session");
        } finally { await browser.close(); }
        expect(mock.upgrades).toEqual([]);
        expect(mock.commands).toEqual([]);
        const creations = mock.requests.filter(request => request.method === "POST" && request.path === "/wd/hub/session");
        expect(creations.length).toBe(1);
        expect(creations[0].body).toMatchObject({ capabilities: { alwaysMatch: {
          browserName, webSocketUrl: webSocketUrl ?? true, "acme:options": { region: "test" },
        } } });
        expect(mock.requests.every(request => request.authorization === "Bearer contract-token")).toBe(true);
        expect(mock.requests.filter(request => request.method === "DELETE").map(request => request.path))
          .toEqual(["/wd/hub/session/mobile-contract"]);
      } finally { mock.server.stop(true); }
    });
  }
}

test("WebDriver timeout includes BiDi initialization and releases the session", async () => {
  const mock = driver({ hang: true });
  try {
    await expect(firefox().connect(mock.endpoint, { timeout: 100 })).rejects.toThrow(/Timeout 100ms/);
    expect(mock.commands).toEqual(["session.subscribe"]);
    expect(mock.requests.map(request => request.method)).toEqual(["POST", "DELETE"]);
  } finally { mock.server.stop(true); }
});

test("WebDriver deletion failure reaches browser.close callers", async () => {
  const mock = driver({ rejectDelete: true });
  try {
    const browser = await firefox().connect(mock.endpoint, { timeout: 1000 });
    await expect(browser.close()).rejects.toThrow(/WebDriver DELETE \/session/);
  } finally { mock.server.stop(true); }
});

test("Safari factory uses Classic WebDriver without an experimental socket", async () => {
  const mock = driver({ classic: true, browserName: "safari" });
  try {
    expect(safari().name()).toBe("safari");
    const browser = await safari().connect(mock.endpoint, { timeout: 1000 });
    try { expect(browser.version()).toBe("safari/contract"); }
    finally { await browser.close(); }
    expect(mock.requests[0].body).toMatchObject({ capabilities: { alwaysMatch: { browserName: "safari" } } });
    expect(mock.requests[0].body).not.toHaveProperty("capabilities.alwaysMatch.webSocketUrl");
    expect(mock.upgrades).toEqual([]);
    expect(mock.requests.map(request => [request.method, request.path])).toEqual([
      ["POST", "/wd/hub/session"],
      ["GET", "/wd/hub/session/mobile-contract/window/handles"],
      ["DELETE", "/wd/hub/session/mobile-contract"],
    ]);
  } finally { mock.server.stop(true); }
});
