import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { expect, test } from '@ferridriver/test';
import { binary, quote, workspace } from './support.mjs';

async function staticComponentServer(files, name) {
  const root = await workspace(files);
  await commands.start(name, {
    command: `exec ${quote(binary.replace(/ferridriver$/, 'ferridriver-fixtures'))} --port 0 --proxy-port 0 --static ${quote(root)}`,
  });
  const output = await commands.waitForOutput(name, '\n');
  const match = output.match(/serving (http:\/\/127\.0\.0\.1:\d+)/);
  assert.ok(match, output);
  return { root, url: match[1], stop: () => commands.stop(name) };
}

test.describe('component testing', () => {
  test.describe.configure({ mode: 'serial' });

test('component server serves a counter and browser interactions update state', async ({ page }) => {
  const server = await staticComponentServer({
    'index.html': `<!doctype html><title>CT Test</title><div id="app"></div><script>
      window.__ferriMount = (componentRef, root, options) => {
        let count = options?.props?.initial ?? 0;
        const render = () => { root.innerHTML = '<span id="count">' + count + '</span><button id="inc">+</button><button id="dec">-</button>'; root.querySelector('#inc').onclick = () => { count++; render(); }; root.querySelector('#dec').onclick = () => { count--; render(); }; };
        render();
      };
      window.__ferriMount({ id: 'Counter' }, document.querySelector('#app'), { props: { initial: 0 } });
    </script>`,
  }, 'stdio');
  try {
    await page.goto(server.url);
    await expect(page).toHaveTitle('CT Test');
    await expect(page.locator('#count')).toHaveText('0');
    await page.locator('#inc').click({ clickCount: 3 });
    await expect(page.locator('#count')).toHaveText('3');
    await page.locator('#dec').click();
    await expect(page.locator('#count')).toHaveText('2');
  } finally {
    await server.stop();
  }
});

test('component mount protocol replaces the host and preserves component props', async ({ page }) => {
  const server = await staticComponentServer({
    'index.html': `<!doctype html><div id="app">INITIAL</div><script>
      window.__ferriMount = (componentRef, root, options) => { const initial = options?.props?.initial ?? 0; root.innerHTML = '<div id="mounted" data-component="' + componentRef.id + '" data-initial="' + initial + '">Mounted: ' + componentRef.id + '</div>'; };
    </script>`,
  }, 'stdio');
  try {
    await page.goto(server.url);
    await expect(page.locator('#app')).toHaveText('INITIAL');
    await page.evaluate("window.__ferriMount({id:'MyCounter'}, document.querySelector('#app'), {props:{initial:42}})");
    await expect(page.locator('#mounted')).toHaveText('Mounted: MyCounter');
    assert.equal(await page.locator('#mounted').getAttribute('data-component'), 'MyCounter');
    assert.equal(await page.locator('#mounted').getAttribute('data-initial'), '42');
  } finally {
    await server.stop();
  }
});
});
