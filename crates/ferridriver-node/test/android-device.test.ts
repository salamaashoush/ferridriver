// NAPI coverage for Playwright's `_android` against an ADB server with no
// Android behind it: the workspace's `ferridriver-adb-fixture` lists one
// usable device and scripts its shell.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { _android, type AndroidDevice } from "../index.js";

const text = (bytes: Uint8Array) => new TextDecoder().decode(bytes);

describe("_android", () => {
  let server: ReturnType<typeof Bun.spawn>;
  let port: number;
  let phone: AndroidDevice;
  let cwd: string;

  beforeAll(async () => {
    server = Bun.spawn([resolve(import.meta.dir, "../../../target/debug/ferridriver-adb-fixture")], {
      stdout: "pipe",
      stderr: "inherit",
    });
    const reader = (server.stdout as ReadableStream<Uint8Array>).getReader();
    const { value } = await reader.read();
    reader.releaseLock();
    port = Number(text(value!).match(/listening (\d+)/)![1]);
    cwd = mkdtempSync(join(tmpdir(), "ferridriver-android-"));
    [phone] = await _android.devices({ port });
  });

  afterAll(() => {
    server.kill();
  });

  it("devices lists what the server can use, and a device keeps its object until closed", async () => {
    const devices = await _android.devices({ port });
    expect(devices.length).toBe(1);
    expect(devices[0]).toBe(phone);
    expect(phone.serial()).toBe("emulator-5554");
    expect(phone.model()).toBe("Fixture Phone");

    await phone.close();
    await expect(phone.shell("true")).rejects.toThrow(/Device is closed/);
    const [again] = await _android.devices({ port });
    expect(again).not.toBe(phone);
    phone = again;
  });

  it("shell and screenshot return Buffers, and a screenshot path is written", async () => {
    const output = await phone.shell('echo "quoted $HOME"');
    expect(Buffer.isBuffer(output)).toBe(true);
    expect(text(output)).toBe('echo "quoted $HOME"');
    const image = await phone.screenshot({ path: join(cwd, "screen.png") });
    expect([...image]).toEqual([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    expect([...readFileSync(join(cwd, "screen.png"))]).toEqual([...image]);
    expect((await phone.screenshot()).length).toBe(8);
  });

  it("push takes bytes or a path, with the mode it is given", async () => {
    await phone.push(Buffer.from("Salama"), "/data/local/tmp/bytes.txt", { mode: 0o600 });
    expect(text(await phone.shell("cat /data/local/tmp/bytes.txt"))).toBe("600:Salama");

    writeFileSync(join(cwd, "note.txt"), "from a file");
    await phone.push(join(cwd, "note.txt"), "/data/local/tmp/file.txt");
    expect(text(await phone.shell("cat /data/local/tmp/file.txt"))).toBe("644:from a file");

    await expect(phone.push(Buffer.from("x"), "/system/denied.txt")).rejects.toThrow(/Read-only file system/);
    await expect(phone.push(join(cwd, "missing.txt"), "/data/local/tmp/x")).rejects.toThrow(/missing\.txt/);
  });

  it("installApk streams the package with the arguments it is given", async () => {
    await phone.installApk(Buffer.from("APK"));
    expect(text(await phone.shell("cat installed"))).toBe("0:-r -t -S:3");
    await phone.installApk(Buffer.from("APK2"), { args: ["-g", "--user", "0"] });
    expect(text(await phone.shell("cat installed"))).toBe("0:-g --user 0:4");
    await expect(phone.installApk(Buffer.from("BAD package"))).rejects.toThrow(/INSTALL_PARSE_FAILED_NOT_APK/);
  });

  it("selector actions and input reach the driver as the calls Playwright makes", async () => {
    // A server of its own: the driver is not installed on a device listed
    // with `omitDriverInstall`, which is decided when it is first listed.
    const own = Bun.spawn([resolve(import.meta.dir, "../../../target/debug/ferridriver-adb-fixture")], {
      stdout: "pipe",
      stderr: "inherit",
    });
    try {
      const reader = (own.stdout as ReadableStream<Uint8Array>).getReader();
      const { value } = await reader.read();
      reader.releaseLock();
      const ownPort = Number(text(value!).match(/listening (\d+)/)![1]);
      const [device] = await _android.devices({ port: ownPort, omitDriverInstall: true });
      const wait = 30000;
      await device.tap({ text: "Save", clickable: true }, { duration: 50, timeout: 1234 });
      await device.fill({ res: /id\/name$/ }, "Salama");
      await device.press({ desc: "Search" }, "Enter");
      await device.longTap({ clazz: "android.widget.Button" });
      await device.drag({ text: "Card" }, { x: 10, y: 20 }, { speed: 500 });
      await device.fling({ scrollable: true }, "down");
      await device.pinchClose({ res: "map" }, 0.5);
      await device.pinchOpen({ res: "map" }, 0.25, { speed: 100 });
      await device.scroll({ scrollable: true }, "up", 0.75);
      await device.swipe({ hasChild: { selector: { text: "Row" } } }, "left", 0.5);
      await device.wait({ text: "Done" }, { state: "gone" });
      const info = await device.info({ hasDescendant: { selector: { text: "Save" }, maxDepth: 2 } });
      await device.input.type("a1");
      await device.input.press("Back");
      await device.input.tap({ x: 1, y: 2 });
      await device.input.swipe({ x: 0, y: 0 }, [{ x: 5, y: 5 }, { x: 9, y: 9 }], 3);
      await device.input.drag({ x: 1, y: 1 }, { x: 2, y: 2 }, 4);
      device.setDefaultTimeout(777);
      await device.longTap({ selected: false });

      expect(JSON.parse(text(await device.shell("cat driver-calls")))).toEqual([
        { method: "click", params: { selector: { text: "^Save$", clickable: true }, duration: 50, timeout: 1234 } },
        { method: "click", params: { selector: { res: "id\\/name$" }, timeout: wait } },
        { method: "fill", params: { selector: { res: "id\\/name$" }, text: "Salama", timeout: wait } },
        { method: "click", params: { selector: { desc: "^Search$" }, timeout: wait } },
        { method: "inputPress", params: { keyCode: 66, timeout: 0 } },
        { method: "longClick", params: { selector: { clazz: "^android\\.widget\\.Button$" }, timeout: wait } },
        { method: "drag", params: { selector: { text: "^Card$" }, dest: { x: 10, y: 20 }, speed: 500, timeout: wait } },
        { method: "fling", params: { selector: { scrollable: true }, direction: "down", timeout: wait } },
        { method: "pinchClose", params: { selector: { res: "^map$" }, percent: 0.5, timeout: wait } },
        { method: "pinchOpen", params: { selector: { res: "^map$" }, percent: 0.25, speed: 100, timeout: wait } },
        { method: "scroll", params: { selector: { scrollable: true }, direction: "up", percent: 0.75, timeout: wait } },
        { method: "swipe", params: { selector: { hasChild: { selector: { text: "^Row$" } } }, direction: "left", percent: 0.5, timeout: wait } },
        { method: "wait", params: { selector: { text: "^Done$" }, state: "gone", timeout: wait } },
        { method: "info", params: { selector: { hasDescendant: { selector: { text: "^Save$" }, maxDepth: 2 } }, timeout: 0 } },
        { method: "inputPress", params: { keyCode: 29, timeout: 0 } },
        { method: "inputPress", params: { keyCode: 8, timeout: 0 } },
        { method: "inputPress", params: { keyCode: 4, timeout: 0 } },
        { method: "inputClick", params: { point: { x: 1, y: 2 }, timeout: 0 } },
        { method: "inputSwipe", params: { segments: [{ x: 0, y: 0 }, { x: 5, y: 5 }, { x: 9, y: 9 }], steps: 3, timeout: 0 } },
        { method: "inputDrag", params: { from: { x: 1, y: 1 }, to: { x: 2, y: 2 }, steps: 4, timeout: 0 } },
        { method: "longClick", params: { selector: { selected: false }, timeout: 777 } },
      ]);
      expect(info).toEqual({
        clazz: "android.widget.Button", desc: "", res: "com.example:id/save", pkg: "com.example", text: "Save",
        bounds: { x: 10, y: 20, width: 300, height: 120 },
        checkable: false, checked: false, clickable: true, enabled: true, focusable: true,
        focused: false, longClickable: false, scrollable: false, selected: false,
      });

      await expect(device.tap({ text: "Missing" })).rejects.toThrow(/Timeout waiting for selector/);
      await expect(device.input.press("NoSuchKey" as any)).rejects.toThrow(/Unknown key: NoSuchKey/);
      await expect(device.fling({}, "sideways" as any)).rejects.toThrow(/direction/);
      await expect(device.wait({}, { state: "visible" as any })).rejects.toThrow(/state/);
    } finally {
      own.kill();
    }
  });

  it("web views are listed, announced and closed, and a socket carries bytes both ways", async () => {
    const own = Bun.spawn([resolve(import.meta.dir, "../../../target/debug/ferridriver-adb-fixture")], {
      stdout: "pipe",
      stderr: "inherit",
    });
    try {
      const reader = (own.stdout as ReadableStream<Uint8Array>).getReader();
      const { value } = await reader.read();
      reader.releaseLock();
      const ownPort = Number(text(value!).match(/listening (\d+)/)![1]);
      const [device] = await _android.devices({ port: ownPort });

      expect(device.webViews()).toEqual([]);
      const announced = device.waitForEvent("webview");
      const seen: unknown[] = [];
      const listener = (view: unknown) => seen.push(view);
      device.on("webview", listener);
      await device.push(
        Buffer.from("0000: 0002 0000 0001 0001 01 5841881 @webview_devtools_remote_4242\n"),
        "webviews",
      );
      const view = (await announced) as import("../index.js").AndroidWebView;
      expect(view.pid()).toBe(4242);
      expect(view.pkg()).toBe("com.example.app");
      expect(device.webViews()[0]).toBe(view);
      expect(await device.webView({ pkg: "com.example.app" })).toBe(view);
      expect(await device.webView({ socketName: "webview_devtools_remote_4242" })).toBe(view);
      expect(seen).toEqual([view]);
      device.off("webview", listener);

      const dropped = new Promise<void>((done) => view.once("close", done));
      await device.push(Buffer.from(""), "webviews");
      await dropped;
      expect(device.webViews()).toEqual([]);
      await expect(device.webView({ pkg: "com.example.absent" }, { timeout: 700 })).rejects.toThrow(
        /Timeout 700ms exceeded while waiting for event "webview"/,
      );
      await expect(device.waitForEvent("webview", { timeout: 600, predicate: () => false })).rejects.toThrow(
        /Timeout 600ms/,
      );

      const socket = await device.open("shell:upper");
      const chunks: string[] = [];
      const ended = new Promise<void>((done) => socket.on("close", () => done()));
      socket.on("data", (chunk) => chunks.push(text(chunk!)));
      await socket.write(Buffer.from("salama\n"));
      await ended;
      expect(chunks.join("")).toBe("SALAMA\n");
      await socket.close();
      await expect(socket.write(Buffer.from("late"))).rejects.toThrow(/Socket is closed/);

      const closing = device.waitForEvent("close");
      const heard = new Promise<unknown>((done) => device.on("close", done));
      await device.close();
      expect(await closing).toBe(device);
      expect(await heard).toBe(device);
      await expect(device.waitForEvent("webview", { timeout: 1000 })).rejects.toThrow(/Device is closed/);
      expect(seen.length).toBe(1);
    } finally {
      own.kill();
    }
  });

  it("devices says where it looked when no ADB server answers", async () => {
    await expect(_android.devices({ port: 1 })).rejects.toThrow(/no ADB server at 127\.0\.0\.1:1/);
    await expect(_android.devices({ port: 70000 })).rejects.toThrow(/port 70000 is out of range/);
  });

  it("a Playwright Android server is refused, with the way that does work", async () => {
    const why = /Playwright's own client\/server protocol[\s\S]*android\.devices\(\{ host, port \}\)/;
    await expect(_android.connect("ws://127.0.0.1:9/")).rejects.toThrow(why);
    await expect(_android.launchServer()).rejects.toThrow(why);
  });

  it("a device listed after android.setDefaultTimeout starts with that timeout", async () => {
    const own = Bun.spawn([resolve(import.meta.dir, "../../../target/debug/ferridriver-adb-fixture")], {
      stdout: "pipe",
      stderr: "inherit",
    });
    try {
      const reader = (own.stdout as ReadableStream<Uint8Array>).getReader();
      const { value } = await reader.read();
      reader.releaseLock();
      const ownPort = Number(text(value!).match(/listening (\d+)/)![1]);
      _android.setDefaultTimeout(4321);
      const [device] = await _android.devices({ port: ownPort, omitDriverInstall: true });
      _android.setDefaultTimeout(30000);
      await device.tap({ text: "Save" });
      expect(JSON.parse(text(await device.shell("cat driver-calls")))).toEqual([
        { method: "click", params: { selector: { text: "^Save$" }, timeout: 4321 } },
      ]);
    } finally {
      own.kill();
    }
  });
});
