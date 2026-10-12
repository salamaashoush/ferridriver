// Playwright dom.ts::_setInputFiles: a payload becomes a File in the page
// (injectedScript.ts::setInputFiles), so it carries the type it was given
// and needs no file the browser could open.
import { test, expect } from '@ferridriver/test';

test.describe('setInputFiles with a payload', () => {
  test('the file keeps the type it was given, whatever its name says', async ({ page }) => {
    await page.setContent("<input type='file' id='pick'><script>window.heard = []; for (const type of ['input', 'change']) pick.addEventListener(type, () => heard.push(type));</script>");
    await page.locator('#pick').setInputFiles({ name: 'note.txt', mimeType: 'application/x-acme', buffer: Buffer.from('Salama') });
    const seen = await page.evaluate(async () => {
      const file = (document.querySelector('#pick') as HTMLInputElement).files![0];
      return { name: file.name, type: file.type, text: await file.text(), heard: (window as any).heard };
    });
    expect(seen).toEqual({ name: 'note.txt', type: 'application/x-acme', text: 'Salama', heard: ['input', 'change'] });
  });

  test('several payloads arrive in order, and an empty list clears them', async ({ page }) => {
    await page.setContent("<input type='file' id='pick' multiple>");
    const names = () => page.evaluate(() => Array.from((document.querySelector('#pick') as HTMLInputElement).files!, file => `${file.name}:${file.size}`));
    await page.locator('#pick').setInputFiles([
      { name: 'one.bin', mimeType: 'application/octet-stream', buffer: Buffer.from([0, 255, 128]) },
      { name: 'two.txt', mimeType: 'text/plain', buffer: Buffer.from('ab') },
    ]);
    expect(await names()).toEqual(['one.bin:3', 'two.txt:2']);
    await page.locator('#pick').setInputFiles([]);
    expect(await names()).toEqual([]);
  });

  test('an element that is not a file input refuses it', async ({ page }) => {
    await page.setContent("<input type='text' id='plain'>");
    const refused = await page
      .locator('#plain')
      .setInputFiles({ name: 'note.txt', mimeType: 'text/plain', buffer: Buffer.from('x') })
      .then(() => 'accepted', (error: Error) => error.message);
    expect(refused).toContain('Not an input[type=file] element');
  });
});
