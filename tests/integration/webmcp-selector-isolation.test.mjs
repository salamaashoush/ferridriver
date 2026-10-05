import assert from 'node:assert/strict';
import {test} from '@ferridriver/test';
import {passed, run, workspace} from './support.mjs';

for (const product of ['pipe','ws','firefox']) {
  test(`native WebMCP does not initialize main-world selector engines: ${product}`, async () => {
    const launch = product === 'firefox'
      ? "firefox().launch({headless:true,firefoxUserPrefs:{'dom.modelcontext.enabled':true,'dom.modelcontext.testing.enabled':true}})"
      : `chromium({transport:${JSON.stringify(product)}}).launch({headless:true,args:['--enable-features=WebMCP,WebMCPTesting,DevToolsWebMCPSupport']})`;
    const result = await run(['run','--no-inherit','--json','-e',`
      const {selectors} = await import('@ferridriver/test');
      const browser = await ${launch};
      try {
        const page = await browser.newPage();
        await page.route('http://127.0.0.1:47839/**', route => route.fulfill({contentType:'text/html',body:'<!doctype html><title>WebMCP</title>'}));
        await page.goto('http://127.0.0.1:47839/');
        await page.evaluate(() => {
          window.mainWorldOnly = true;
          const model = document.modelContext ?? navigator.modelContext;
          model.registerTool({name:'echo',description:'Returns input',inputSchema:{type:'object'},execute: input => input});
        });
        await selectors.register('mainworldonly', () => {
          if (!window.mainWorldOnly) throw new Error('selector engine initialized outside its main world');
          return {query: (root, selector) => root.querySelector(selector), queryAll: (root, selector) => [...root.querySelectorAll(selector)]};
        });
        return {names:(await page.webmcp.tools()).map(tool => tool.name), value:await page.webmcp.callTool('echo',{value:42})};
      } finally { await browser.close(); }
    `], {cwd:await workspace({})});
    passed(result);
    assert.deepEqual(JSON.parse(result.stdout).value, {names:['echo'],value:{value:42}});
  });
}
