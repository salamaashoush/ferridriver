// Ported from crates/ferridriver-cli/tests/backends_support/
// {dialog,file_chooser,download}.rs — Dialog, FileChooser, and Download
// as first-class event handles via page.waitForEvent. Test titles
// mirror the original Rust fn names.

import { test, describe, expect } from '@ferridriver/test';
import type { Dialog, Download, FileChooser, Page } from '@ferridriver/test';
import { dataUrl } from './helpers/html';

async function pollTitle(page: Page, pred: (title: string) => boolean, timeoutMs = 5000): Promise<string> {
  const deadline = Date.now() + timeoutMs;
  let title = '';
  while (Date.now() < deadline) {
    title = await page.title();
    if (pred(title)) {
      return title;
    }
  }
  return title;
}

const SINGLE_FORM_HTML =
  "<form id='f'><input id='i' type='file' name='f' /><button id='b' type='button'>pick</button></form>" +
  '<script>' +
  "const i = document.getElementById('i');" +
  "const b = document.getElementById('b');" +
  "b.addEventListener('click', () => i.click());" +
  "i.addEventListener('change', () => {" +
  'const files = i.files;' +
  'const count = files.length;' +
  "const first = count > 0 ? files[0].name : '';" +
  'document.title = `count=${count};first=${first}`;' +
  '});' +
  '</script>';

const MULTIPLE_FORM_HTML =
  "<form id='f'><input id='i' type='file' name='f' multiple /><button id='b' type='button'>pick</button></form>" +
  '<script>' +
  "const i = document.getElementById('i');" +
  "const b = document.getElementById('b');" +
  "b.addEventListener('click', () => i.click());" +
  "i.addEventListener('change', () => {" +
  'const files = i.files;' +
  'const names = [];' +
  'for (let k = 0; k < files.length; k++) names.push(files[k].name);' +
  'document.title = `count=${files.length};names=${names.join("|")}`;' +
  '});' +
  '</script>';

const PAYLOAD_FORM_HTML =
  "<form id='f'><input id='i' type='file' name='f' /><button id='b' type='button'>pick</button></form>" +
  '<script>' +
  "const i = document.getElementById('i');" +
  "const b = document.getElementById('b');" +
  "b.addEventListener('click', () => i.click());" +
  "i.addEventListener('change', async () => {" +
  'const f = i.files[0];' +
  'const text = await f.text();' +
  'document.title = `name=${f.name};size=${f.size};text=${text}`;' +
  '});' +
  '</script>';

// Inject an anchor pointing at a fixture download route into any
// same-origin HTML page. /fx/landed is text/plain, so downloads anchor
// off /fx/iframe (the fixture server's plain HTML page).
async function gotoDownloadPage(page: Page, href: string): Promise<void> {
  await page.goto('/fx/iframe');
  await page.evaluate(
    `const a = document.createElement('a'); a.id = 'dl'; a.href = ${JSON.stringify(href)}; a.textContent = 'dl'; document.body.appendChild(a); null`,
  );
}

describe('dialogs and files', () => {
  test('dialog_accept_confirm', async ({ page }) => {
    // The page schedules the confirm inside a setTimeout so JS has a
    // chance to yield back to the binding, let waitForEvent register,
    // and capture the dialog.
    await page.goto(
      dataUrl("<script>setTimeout(()=>{document.title = confirm('sure?') ? 'yes' : 'no'}, 80)</script>"),
    );
    const dialog = (await page.waitForEvent('dialog', { timeout: 10000 })) as Dialog;
    expect(dialog.type()).toBe('confirm');
    expect(dialog.message().includes('sure')).toBe(true);
    await dialog.accept();
    const title = await pollTitle(page, (t) => t === 'yes' || t === 'no');
    expect(title).toBe('yes');
  });

  test('dialog_dismiss_confirm', async ({ page }) => {
    await page.goto(
      dataUrl("<script>setTimeout(()=>{document.title = confirm('ok?') ? 'yes' : 'no'}, 80)</script>"),
    );
    const dialog = (await page.waitForEvent('dialog', { timeout: 10000 })) as Dialog;
    await dialog.dismiss();
    const title = await pollTitle(page, (t) => t === 'yes' || t === 'no');
    expect(title).toBe('no');
  });

  test('dialog_prompt_with_text', async ({ page }) => {
    // prompt dialog — accept with custom text, the page sees it; also
    // exercises the defaultValue() accessor.
    await page.goto(
      dataUrl("<script>setTimeout(()=>{document.title = prompt('name?', 'alice') || 'null'}, 80)</script>"),
    );
    const dialog = (await page.waitForEvent('dialog', { timeout: 10000 })) as Dialog;
    expect(dialog.type()).toBe('prompt');
    expect(dialog.defaultValue()).toBe('alice');
    await dialog.accept('bob');
    const title = await pollTitle(page, (t) => t !== '');
    expect(title).toBe('bob');
  });

  test('dialog_double_accept_rejects', async ({ page }) => {
    // Second accept on the same Dialog rejects with the
    // Playwright-exact message (one-shot contract).
    await page.goto(dataUrl("<script>setTimeout(()=>{alert('once')}, 80)</script>"));
    const dialog = (await page.waitForEvent('dialog', { timeout: 10000 })) as Dialog;
    await dialog.accept();
    let msg = '';
    let threw = false;
    try {
      await dialog.accept();
    } catch (e) {
      threw = true;
      msg = String((e as Error).message ?? e);
    }
    expect(threw).toBe(true);
    expect(msg.includes('already handled')).toBe(true);
  });

  test('dialog_auto_dismiss_without_listener', async ({ page }) => {
    // No listener registered -> the backend auto-dismisses the dialog
    // so the page's confirm() returns false. We drive the dismiss
    // ourselves rather than relying on host defaults.
    await page.goto(dataUrl("<script>document.title = confirm('no listener?') ? 'yes' : 'no'</script>"));
    const title = await pollTitle(page, (t) => t === 'yes' || t === 'no');
    expect(title).toBe('no');
  });

  test('dialog_page_accessor', async ({ page }) => {
    await page.goto(
      dataUrl("<title>dlg</title><script>setTimeout(()=>{document.title = confirm('p?') ? 'y' : 'n'}, 80)</script>"),
    );
    const dialog = (await page.waitForEvent('dialog', { timeout: 10000 })) as Dialog;
    const dlgPage = dialog.page();
    expect(dlgPage != null).toBe(true);
    expect(dlgPage!.url()).toBe(page.url());
    await dialog.accept();
  });

  test('file_chooser_single_string_path', async ({ page }) => {
    // waitForEvent('filechooser') returns a live FileChooser;
    // isMultiple() is false for a plain input; setFiles(path) uploads
    // and the page sees files[0].name.
    await page.goto(dataUrl(SINGLE_FORM_HTML));
    const path = test.info().outputPath('fc-a.txt');
    await fs.promises.writeFile(path, 'alpha');
    const chooserPromise = page.waitForEvent('filechooser', { timeout: 10000 });
    await page.click('#b');
    const chooser = (await chooserPromise) as FileChooser;
    expect(chooser.isMultiple()).toBe(false);
    expect(chooser.page().url()).toBe(page.url());
    await chooser.setFiles(path);
    const title = (await page.evaluate(
      async (prefix: string) => {
        for (let i = 0; i < 200; i++) {
          const t = document.title;
          if (t && t.startsWith(prefix)) return t;
          await new Promise((r) => setTimeout(r, 10));
        }
        return document.title;
      },
      'count=',
    )) as string;
    expect(title).toBe('count=1;first=fc-a.txt');
  });

  test('file_chooser_multiple_string_array', async ({ page }) => {
    await page.goto(dataUrl(MULTIPLE_FORM_HTML));
    const p1 = test.info().outputPath('fc-a-multi.txt');
    const p2 = test.info().outputPath('fc-b-multi.txt');
    await fs.promises.writeFile(p1, 'alpha');
    await fs.promises.writeFile(p2, 'beta');
    const chooserPromise = page.waitForEvent('filechooser', { timeout: 10000 });
    await page.click('#b');
    const chooser = (await chooserPromise) as FileChooser;
    expect(chooser.isMultiple()).toBe(true);
    await chooser.setFiles([p1, p2]);
    const title = (await page.evaluate(
      async (prefix: string) => {
        for (let i = 0; i < 200; i++) {
          const t = document.title;
          if (t && t.startsWith(prefix)) return t;
          await new Promise((r) => setTimeout(r, 10));
        }
        return document.title;
      },
      'count=',
    )) as string;
    expect(
      title === 'count=2;names=fc-a-multi.txt|fc-b-multi.txt' || title === 'count=2;names=fc-b-multi.txt|fc-a-multi.txt',
    ).toBe(true);
  });

  test('file_chooser_file_payload_single', async ({ page }) => {
    // setFiles(FilePayload) uploads an in-memory payload; the page reads
    // the file via await f.text(), proving the bytes round-tripped
    // byte-for-byte.
    await page.goto(dataUrl(PAYLOAD_FORM_HTML));
    const chooserPromise = page.waitForEvent('filechooser', { timeout: 10000 });
    await page.click('#b');
    const chooser = (await chooserPromise) as FileChooser;
    await chooser.setFiles({ name: 'greeting.txt', mimeType: 'text/plain', buffer: new TextEncoder().encode('hello') });
    const title = (await page.evaluate(
      async (prefix: string) => {
        for (let i = 0; i < 200; i++) {
          const t = document.title;
          if (t && t.startsWith(prefix)) return t;
          await new Promise((r) => setTimeout(r, 10));
        }
        return document.title;
      },
      'name=',
    )) as string;
    expect(title.includes('name=greeting.txt')).toBe(true);
    expect(title.includes('size=5')).toBe(true);
    expect(title.includes('text=hello')).toBe(true);
  });

  test('file_chooser_unclaimed_disposes', async ({ page }) => {
    // No waitForEvent: the click must resolve promptly without the
    // browser hanging on a native picker (the intercept suppresses it
    // and the backend disposes the captured element).
    await page.goto(dataUrl(SINGLE_FORM_HTML));
    const started = Date.now();
    await page.click('#b');
    expect(Date.now() - started).toBeLessThan(2000);
  });

  test('download_save_as_roundtrip', async ({ page }) => {
    // Trigger a download via a Content-Disposition attachment, capture
    // via waitForEvent('download'), saveAs into the test's outputDir,
    // and compare the bytes to the served payload.
    await gotoDownloadPage(page, '/fx/download');
    const savePath = test.info().outputPath('saved-download.bin');
    const downloadPromise = page.waitForEvent('download', { timeout: 15000 });
    await page.click('#dl');
    const dl = (await downloadPromise) as Download;
    expect(dl.url().includes('/fx/download')).toBe(true);
    expect(dl.suggestedFilename()).toBe('greeting.txt');
    expect(dl.page()?.url()).toBe(page.url());
    await dl.saveAs(savePath);
    expect(await fs.promises.readFile(savePath, 'utf8')).toBe('fx-download-payload');
  });

  test('download_path_contents', async ({ page }) => {
    // download.path() resolves only after the wait_finished ->
    // report_finished transition. The backend writes into its own temp
    // dir outside the fs sandbox root, so the byte-level check goes
    // through saveAs against the same finished download.
    await gotoDownloadPage(page, '/fx/download');
    const downloadPromise = page.waitForEvent('download', { timeout: 15000 });
    await page.click('#dl');
    const dl = (await downloadPromise) as Download;
    const path = await dl.path();
    expect(path.length).toBeGreaterThan(0);
    const savePath = test.info().outputPath('path-contents.bin');
    await dl.saveAs(savePath);
    expect(await fs.promises.readFile(savePath, 'utf8')).toBe('fx-download-payload');
  });

  test('download_cancel_surfaces_failure', async ({ page, browserName }) => {
    if (browserName === 'firefox') {
      // Firefox's BiDi has no cancel command — Playwright's own BiDi
      // backend leaves cancelDownload as a no-op; ferridriver surfaces
      // the gap as a typed Unsupported instead of a silent no-op. The
      // completing /fx/download keeps failure() from blocking forever.
      await gotoDownloadPage(page, '/fx/download');
      const downloadPromise = page.waitForEvent('download', { timeout: 15000 });
      await page.click('#dl');
      const dl = (await downloadPromise) as Download;
      let msg = '';
      let threw = false;
      try {
        await dl.cancel();
      } catch (e) {
        threw = true;
        msg = String((e as Error).message ?? e);
      }
      expect(threw).toBe(true);
      expect(msg.includes('unsupported') || msg.includes('Unsupported') || msg.includes('BiDi')).toBe(true);
      return;
    }
    // /fx/download-hang serves an attachment that never finishes, so
    // the download is deterministically still in-flight when cancel()
    // fires. CDP: Browser.cancelDownload; WebKit:
    // Playwright.cancelDownload — both settle failure() as 'canceled'.
    await gotoDownloadPage(page, '/fx/download-hang');
    const downloadPromise = page.waitForEvent('download', { timeout: 15000 });
    await page.click('#dl');
    const dl = (await downloadPromise) as Download;
    await dl.cancel();
    expect(await dl.failure()).toBe('canceled');
  });
});
