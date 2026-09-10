import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { test } from '@ferridriver/test';
import { listZipEntries, readZipEntry } from '../e2e/helpers/unzip.ts';
import { workspace } from './support.mjs';
import { McpClient } from './mcp-client.mjs';

async function archive(path) {
  const bytes = await readFile(path);
  const entries = listZipEntries(bytes);
  return {
    names: entries.map(entry => entry.name),
    lines(name) {
      const entry = entries.find(entry => entry.name === name);
      assert.ok(entry, `missing archive entry ${name}`);
      return new TextDecoder().decode(readZipEntry(bytes, entry)).split('\n').filter(line => line.trim()).map(line => JSON.parse(line));
    },
  };
}
for (const backend of ['cdp-pipe', 'cdp-raw', 'bidi', 'webkit']) {
  test(`${backend}: MCP traces contain viewer-compatible actions, snapshots, console and resources`, async () => {
    const client = await McpClient.launch(backend);
    try {
      const path = join(await workspace({}), 'trace.zip');
      const result = await client.script(`
        await context.tracing.start({ title: 'MCP trace', screenshots: true, snapshots: true });
        await page.goto('data:text/html,<body><style>button{color:red}</style><button id=b>Go</button></body>');
        await page.evaluate("console.log('trace-console-probe', 42)");
        let consoleSeen = false;
        for (let i = 0; i < 200 && !consoleSeen; i++) {
          consoleSeen = page.consoleMessages({ filter: 'all' }).some(message => message.text().includes('trace-console-probe'));
          if (!consoleSeen) await page.waitForTimeout(25);
        }
        const second = await context.newPage();
        await second.close();
        await page.evaluate("document.styleSheets[0].insertRule('body{margin:0}')");
        await page.evaluate(() => document.getElementById('b').addEventListener('click', () => {
          document.body.style.background = 'rgb(' + ((Math.random() * 255) | 0) + ',120,120)';
        }));
        for (let i = 0; i < 5; i++) await page.locator('#b').click();
        let missingError = '';
        try { await page.locator('#missing').click({ timeout: 500 }); } catch (error) { missingError = String(error); }
        await context.tracing.stop({ path: args[0] });
        let doubleStop = '';
        try { await context.tracing.stop(); } catch (error) { doubleStop = String(error); }
        return { missingError, doubleStop, consoleSeen };
      `, [path]);
      assert.equal(result.consoleSeen, true);
      assert.ok(result.missingError.length > 0);
      assert.match(result.doubleStop, /Must start tracing/);
      const zip = await archive(path);
      assert.ok(zip.names.includes('trace.network'));
      const lines = zip.lines('trace.trace');
      assert.equal(lines[0].type, 'context-options');
      assert.equal(lines[0].version, 8);
      assert.equal(lines[0].origin, 'library');
      const merged = new Map();
      for (const line of lines) {
        if (typeof line.callId !== 'string' || !['before', 'input', 'after'].includes(line.type)) continue;
        const { type, ...fields } = line;
        merged.set(line.callId, { ...merged.get(line.callId), ...fields });
      }
      const actions = [...merged.values()];
      const goto = actions.find(action => action.method === 'goto');
      assert.ok(goto);
      assert.match(goto.callId, /^call@/);
      assert.equal(typeof goto.startTime, 'number');
      assert.equal(typeof goto.endTime, 'number');
      assert.ok(goto.startTime <= goto.endTime);
      const click = actions.find(action => action.method === 'click' && action.params?.selector === '#b');
      assert.ok(click);
      assert.equal(click.class, 'Locator');
      assert.match(click.inputSnapshot, /^input@/);
      assert.equal(typeof click.point.x, 'number');
      assert.equal(typeof click.point.y, 'number');
      assert.ok(lines.some(line => line.type === 'log' && line.callId === click.callId && line.message.includes('waiting for')));
      const failed = actions.find(action => action.params?.selector === '#missing');
      assert.ok(failed);
      assert.equal(typeof failed.error.message, 'string');
      assert.equal(failed.error.name, 'TimeoutError');
      const snapshots = lines.filter(line => line.type === 'frame-snapshot');
      assert.ok(snapshots.length > 0);
      for (const kind of ['beforeSnapshot', 'afterSnapshot']) {
        assert.equal(typeof click[kind], 'string');
        const snapshot = snapshots.find(line => line.snapshot.snapshotName === click[kind]);
        assert.ok(snapshot, kind);
        assert.equal(snapshot.snapshot.isMainFrame, true);
        assert.ok(Array.isArray(snapshot.snapshot.html));
      }
      const html = snapshots.map(line => JSON.stringify(line.snapshot.html)).join('');
      for (const expected of ['BUTTON', 'margin', '__playwright_target__']) assert.ok(html.includes(expected), expected);
      const consoleLine = lines.find(line => line.type === 'console' && line.text.includes('trace-console-probe'));
      assert.ok(consoleLine);
      assert.equal(consoleLine.messageType, 'log');
      assert.equal(consoleLine.args.length, 2);
      assert.ok(consoleLine.args.some(arg => arg.value === 42 || arg.preview === '42'));
      assert.equal(typeof goto.pageId, 'string');
      assert.equal(consoleLine.pageId, goto.pageId);
      assert.ok(consoleLine.time >= 0);
      const events = lines.filter(line => line.type === 'event');
      const closed = events.filter(line => line.method === 'pageClosed').map(line => line.params.pageId);
      const opened = events.find(line => line.method === 'page' && closed.includes(line.params.pageId));
      assert.ok(opened);
      assert.equal(opened.class, 'BrowserContext');
      assert.equal(typeof opened.params.pageId, 'string');
      const frames = lines.filter(line => line.type === 'screencast-frame');
      assert.ok(frames.length > 1, `only ${frames.length} frames`);
      for (const frame of frames) assert.ok(zip.names.includes(`resources/${frame.sha1}`), frame.sha1);
      for (const line of zip.lines('trace.network')) {
        assert.equal(line.type, 'resource-snapshot');
        assert.match(line.snapshot.startedDateTime, /^20/);
        assert.ok(line.snapshot._monotonicTime >= 0);
      }
    } finally { await client.close(); }
  });

  test(`${backend}: MCP trace snapshots link parent iframe nodes to child snapshots`, async () => {
    const client = await McpClient.launch(backend);
    try {
      const path = join(await workspace({}), 'iframe.zip');
      await client.script(`
        await context.tracing.start({ title: 'iframe trace', snapshots: true });
        await page.goto('data:text/html,<h1>parent</h1>');
        await page.setContent("<h1 id=p>parent</h1><iframe name=kid srcdoc='<button id=c>child</button>'></iframe>");
        await page.frameLocator('iframe').locator('#c').waitFor({ timeout: 10000 });
        await page.locator('#p').click();
        await context.tracing.stop({ path: args[0] });
        return {};
      `, [path]);
      const snapshots = (await archive(path)).lines('trace.trace').filter(line => line.type === 'frame-snapshot');
      const child = snapshots.find(line => line.snapshot.isMainFrame === false);
      assert.ok(child);
      assert.equal(typeof child.snapshot.frameId, 'string');
      assert.match(JSON.stringify(child.snapshot.html), /BUTTON|child/);
      const main = snapshots.filter(line => line.snapshot.isMainFrame === true).map(line => JSON.stringify(line.snapshot.html)).join('');
      assert.ok(main.includes(`/snapshot/${child.snapshot.frameId}`));
    } finally { await client.close(); }
  });
}
