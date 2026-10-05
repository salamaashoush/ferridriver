import assert from 'node:assert/strict';
import {join} from 'node:path';
import {test} from '@ferridriver/test';
import {passed, quote, repo, run, workspace} from './support.mjs';

for (const mode of ['cdp', 'discovery', 'bidi']) {
  test(`${mode} connection forwards authorization through initialization`, async () => {
    await commands.start('stdio', {command:`exec bun ${quote(join(repo, 'tests/shared/protocol-close-server.mjs'))} --auth`});
    try {
      const {port} = JSON.parse((await commands.waitForOutput('stdio', '\n')).trim());
      const endpoint = mode === 'discovery' ? `http://127.0.0.1:${port}/gateway` : `ws://127.0.0.1:${port}/${mode}`;
      const connect = mode === 'bidi' ? 'firefox().connect' : 'chromium().connectOverCDP';
      const result = await run(['run','--no-inherit','--json','-e',`
        const browser = await ${connect}(${JSON.stringify(endpoint)}, {
          headers:{authorization:'Bearer sashoush-test-token'}, timeout:5000,
        });
        try { return await (await browser.contexts()[0].pages())[0].title(); }
        finally { await browser.close(); }
      `], {cwd:await workspace({})});
      passed(result);
      assert.equal(JSON.parse(result.stdout).value, 'sashoush fixture');
    } finally { await commands.stop('stdio'); }
  });
}

for (const protocol of ['cdp','bidi']) {
 for (const phase of ['initial','adoption']) {
  test(`${protocol} connection timeout covers ${phase} and zero disables it`, async () => {
    await commands.start('stdio', {command:`exec bun ${quote(join(repo, 'tests/shared/protocol-close-server.mjs'))}`});
    try {
      const {port} = JSON.parse((await commands.waitForOutput('stdio', '\n')).trim());
      const method = phase === 'initial' ? '' : (protocol === 'cdp' ? 'Page.getFrameTree' : 'browsingContext.getTree');
      const endpoint = `ws://127.0.0.1:${port}/${protocol}?delay=150&method=${method}`;
      const connect = protocol === 'bidi' ? 'firefox().connect' : 'chromium().connectOverCDP';
      const result = await run(['run','--no-inherit','--json','-e',`
        const assert = (await import('node:assert/strict')).default;
        const started = Date.now();
        await assert.rejects(${connect}(${JSON.stringify(endpoint)}, {timeout:50}), /timeout/i);
        assert.ok(Date.now()-started < 2000, 'connection deadline must cover initialization');
        const browser = await ${connect}(${JSON.stringify(endpoint)}, {timeout:0});
        try { return await (await browser.contexts()[0].pages())[0].title(); }
        finally { await browser.close(); }
      `], {cwd:await workspace({})});
      passed(result);
      assert.equal(JSON.parse(result.stdout).value, 'sashoush fixture');
    } finally { await commands.stop('stdio'); }
  });
}
}
