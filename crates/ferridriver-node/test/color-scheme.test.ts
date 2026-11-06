// A context that names no colour scheme is emulated light on every
// backend, whatever the host's appearance; `colorScheme: null` turns the
// emulation off. On a host in dark mode WebKit used to follow the host.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import type { Browser, Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : ["cdp-pipe", "cdp-raw", "bidi", "webkit"];

async function scheme(page: Page): Promise<{ light: boolean; dark: boolean }> {
  return {
    light: (await page.evaluate("matchMedia('(prefers-color-scheme: light)').matches")) as boolean,
    dark: (await page.evaluate("matchMedia('(prefers-color-scheme: dark)').matches")) as boolean,
  };
}

// What a browser does with no scheme emulated, launched as Playwright
// launches it: Firefox is set light; WebKit follows the desktop; headless
// Chromium draws light on macOS and follows the desktop elsewhere.
function desktopPrefersDark(): boolean {
  try {
    if (process.platform === "darwin") {
      const style = Bun.spawnSync(["defaults", "read", "-g", "AppleInterfaceStyle"]);
      return style.stdout.toString().trim() === "Dark";
    }
    const scheme = Bun.spawnSync(["gsettings", "get", "org.gnome.desktop.interface", "color-scheme"]);
    return scheme.success && scheme.stdout.toString().includes("prefer-dark");
  } catch {
    return false;
  }
}

function ownSchemeIsDark(backend: string): boolean {
  if (backend === "bidi") return false;
  if (backend === "webkit") return desktopPrefersDark();
  return process.platform !== "darwin" && desktopPrefersDark();
}

for (const backend of BACKENDS) {
  describe(`colorScheme [${backend}]`, () => {
    let browser: Browser;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
    });

    afterAll(async () => {
      await browser.close();
    });

    async function inContext<T>(options: Parameters<Browser["newContext"]>[0], body: (page: Page) => Promise<T>) {
      const ctx = browser.newContext(options);
      try {
        const page = await ctx.newPage();
        await page.setContent("<p>text</p>");
        return await body(page);
      } finally {
        await ctx.close();
      }
    }

    it("defaults to light, the same as asking for it", async () => {
      const unset = await inContext({}, scheme);
      expect(unset).toEqual({ light: true, dark: false });
      expect(unset).toEqual(await inContext({ colorScheme: "light" }, scheme));
    });

    it("emulates dark when asked", async () => {
      expect(await inContext({ colorScheme: "dark" }, scheme)).toEqual({ light: false, dark: true });
    });

    it("null leaves the page to the browser", async () => {
      const dark = ownSchemeIsDark(backend);
      const own = await inContext({ colorScheme: null }, scheme);
      expect(own).toEqual({ light: !dark, dark });
      const cleared = await inContext({}, async (page) => {
        await page.emulateMedia({ colorScheme: null });
        return scheme(page);
      });
      expect(own).toEqual(cleared);
    });
  });
}
