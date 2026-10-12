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

  it("devices says where it looked when no ADB server answers", async () => {
    await expect(_android.devices({ port: 1 })).rejects.toThrow(/no ADB server at 127\.0\.0\.1:1/);
    await expect(_android.devices({ port: 70000 })).rejects.toThrow(/port 70000 is out of range/);
  });
});
