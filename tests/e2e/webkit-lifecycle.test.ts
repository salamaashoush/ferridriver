import { test, expect, type Browser, type Download } from '@ferridriver/test';
import { promises as fs } from 'node:fs';
import { dirname } from 'node:path';

test('independent WebKit browsers retain independent downloads after one closes', async ({ baseURL }) => {
  const first = await webkit().launch({ headless: true });
  let second: Browser | undefined;
  try {
    second = await webkit().launch({ headless: true });
    const paths: string[] = [];
    let retained: Download | undefined;
    for (const browser of [first, second]) {
      const page = await browser.newPage();
      await page.goto(`${baseURL}/fx/iframe`);
      await page.evaluate(`const a = document.createElement('a'); a.href = '/fx/download'; a.id = 'download'; a.textContent = 'download'; document.body.appendChild(a); null`);
      const pending = page.waitForEvent('download');
      await page.click('#download');
      retained = await pending as Download;
      paths.push(await retained.path());
    }
    expect(dirname(paths[0])).not.toBe(dirname(paths[1]));
    await first.close();
    const saved = test.info().outputPath('second-browser-download.txt');
    await retained!.saveAs(saved);
    expect(await fs.readFile(saved, 'utf8')).toBe('fx-download-payload');
  } finally {
    try {
      await first.close();
    } finally {
      await second?.close();
    }
  }
});


test('WebKit launches the explicit executable and reports its installed launcher', async () => {
  const binary = webkit().executablePath();
  expect(binary).toBeTruthy();
  const executable = test.info().outputPath('custom-webkit.sh');
  const marker = test.info().outputPath('selected-executable');
  const quote = (value: string) => "'" + value.replaceAll("'", "'\\''") + "'";
  await fs.mkdir(dirname(executable), {recursive:true});
  await fs.writeFile(executable, `#!/bin/sh\nprintf '%s' selected > ${quote(marker)}\nexec ${quote(binary!)} "$@"\n`);
  await fs.chmod(executable, 0o700);
  const browser = await webkit().launch({headless:true, executablePath:executable});
  try {
    const page = await browser.newPage();
    await page.setContent('<button onclick="this.textContent = event.isTrusted ? \'trusted\' : \'synthetic\'">Click</button>');
    await page.getByRole('button').click();
    expect(await page.getByRole('button').textContent()).toBe('trusted');
    expect(await fs.readFile(marker, 'utf8')).toBe('selected');
  } finally {
    await browser.close();
  }
});

test('WebKit rejects a missing explicit executable without falling back', async () => {
  await expect(webkit().launch({headless:true, executablePath:test.info().outputPath('missing-webkit')}))
    .rejects.toThrow(/No such file|not found/);
});
