/**
 * §2.9 NAPI parity tests for the Dialog lifecycle handle.
 *
 * `page.waitForEvent('dialog')` returns a live `Dialog` class instance
 * — `type()`, `message()`, `defaultValue()`, and async
 * `accept(promptText?)` / `dismiss()`. The dispatch bypasses the
 * broadcast emitter and routes through the Rust-core
 * `DialogManager::add_handler` path so the claim is synchronous with
 * the browser's `javascriptDialogOpening` event — no grace window,
 * no race.
 *
 * Gated to CDP backends here; the Rust integration suite
 * (`tests/backends_support/dialog.rs`) covers CDP + BiDi + WebKit.
 */
import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS = process.env.FERRIDRIVER_BACKEND
  ? [process.env.FERRIDRIVER_BACKEND]
  : (["cdp-pipe", "cdp-raw"] as const);

/// Small helper — race a goto against a simultaneous waitForEvent.
/// The page's inline script schedules the dialog so the opening
/// event arrives after both sides are set up.
async function navigateAndDialog(page: Page, html: string) {
  const waiter = page.waitForEvent("dialog", 10_000);
  // Kick off the nav WITHOUT awaiting — the dialog fires mid-load.
  const gotoPromise = page.goto(`data:text/html,${encodeURIComponent(html)}`, null);
  return { waiter, gotoPromise };
}

for (const backend of BACKENDS) {
  describe(`[${backend}] Dialog as first-class handle (§2.9)`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
    }, 30_000);

    afterAll(async () => {
      await browser?.close();
    });

    it("waitForEvent('dialog') + dialog.accept() lets confirm return true", async () => {
      const html =
        "<script>document.title = confirm('sure?') ? 'yes' : 'no'</script>";
      const { waiter, gotoPromise } = await navigateAndDialog(page, html);
      const dialog = (await waiter) as unknown as {
        type(): string;
        message(): string;
        accept(text?: string | null): Promise<void>;
      };
      expect(dialog.type()).toBe("confirm");
      expect(dialog.message()).toContain("sure");
      await dialog.accept();
      await gotoPromise;
      expect(await page.title()).toBe("yes");
    });

    it("dialog.page() returns the owning page", async () => {
      const html =
        "<title>dlg</title><script>document.title = confirm('p?') ? 'y' : 'n'</script>";
      const { waiter, gotoPromise } = await navigateAndDialog(page, html);
      const dialog = (await waiter) as unknown as {
        page(): Page | null;
        accept(text?: string | null): Promise<void>;
      };
      const dlgPage = dialog.page();
      expect(dlgPage).not.toBeNull();
      expect(dlgPage!.url()).toBe(page.url());
      await dialog.accept();
      await gotoPromise;
    });

    it("dialog.dismiss() makes confirm return false", async () => {
      const html =
        "<script>document.title = confirm('ok?') ? 'yes' : 'no'</script>";
      const { waiter, gotoPromise } = await navigateAndDialog(page, html);
      const dialog = (await waiter) as unknown as { dismiss(): Promise<void> };
      await dialog.dismiss();
      await gotoPromise;
      expect(await page.title()).toBe("no");
    });

    it("prompt: dialog.accept(text) passes the text to the page", async () => {
      const html =
        "<script>document.title = prompt('name?', 'alice') || 'null'</script>";
      const { waiter, gotoPromise } = await navigateAndDialog(page, html);
      const dialog = (await waiter) as unknown as {
        type(): string;
        defaultValue(): string;
        accept(text?: string | null): Promise<void>;
      };
      expect(dialog.type()).toBe("prompt");
      expect(dialog.defaultValue()).toBe("alice");
      await dialog.accept("bob");
      await gotoPromise;
      expect(await page.title()).toBe("bob");
    });

    it("second accept rejects with Playwright's exact wording", async () => {
      const html = "<script>alert('once')</script>";
      const { waiter, gotoPromise } = await navigateAndDialog(page, html);
      const dialog = (await waiter) as unknown as {
        accept(text?: string | null): Promise<void>;
      };
      await dialog.accept();
      await expect(dialog.accept()).rejects.toThrow(/already handled/);
      await gotoPromise;
    });

    it("no handler registered → dialog auto-dismisses (confirm returns false)", async () => {
      // No listener, no waitForEvent → DialogManager auto-closes.
      await page.goto(
        "data:text/html,<script>document.title = confirm('no listener?') ? 'yes' : 'no'</script>",
        null,
      );
      expect(await page.title()).toBe("no");
    });
  });
}
