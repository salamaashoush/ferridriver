import {test, expect} from '@ferridriver/test';
import {verifyWebMcp} from '../shared/webmcp';
import {verifyWebMcpConcurrency} from '../shared/webmcp-concurrency';

for (const transport of ['pipe', 'ws'] as const) {
  test(`native Chromium WebMCP ignores main-world method shadows: ${transport}`, async ({baseURL}) => {
    const browser = await chromium({transport}).launch({headless:true,
      args:['--enable-features=WebMCP,WebMCPTesting,DevToolsWebMCPSupport']});
    try {
      const page = await browser.newPage();
      await page.goto(`${baseURL}/fx/iframe`);
      await page.evaluate(() => {
        const model = (document as any).modelContext ?? (navigator as any).modelContext;
        model.registerTool({name:'native', description:'Runs the registered native tool', execute: () => {
          document.title = 'sashoush native tool';
          return {native:true};
        }});
        for (const name of ['getTools', 'executeTool']) {
          Object.defineProperty(model, name, {value: () => { throw new Error(`page shadowed ${name}`); }});
        }
        Array.prototype.filter = () => { throw new Error('page shadowed array filtering'); };
      });
      expect((await page.webmcp.tools()).map(tool => tool.name)).toEqual(['native']);
      expect(await page.webmcp.callTool('native')).toEqual({native:true});
      expect(await page.title()).toBe('sashoush native tool');
    } finally {
      await browser.close();
    }
  });
}

for (const product of ['chromium-pipe', 'chromium-ws', 'firefox'] as const) {
  test(`native WebMCP allows concurrent discovery and trusted input: ${product}`, async ({baseURL}) => {
    const browser = product === 'firefox'
      ? await firefox().launch({headless: true, firefoxUserPrefs: {
        'dom.modelcontext.enabled': true, 'dom.modelcontext.testing.enabled': true,
      }})
      : await chromium({transport: product === 'chromium-pipe' ? 'pipe' : 'ws'}).launch({headless: true,
        args: ['--enable-features=WebMCP,WebMCPTesting,DevToolsWebMCPSupport']});
    try {
      await verifyWebMcpConcurrency(await browser.newPage(), {
        equal: (actual, expected) => expect(actual).toBe(expected),
        ok: value => expect(Boolean(value)).toBe(true),
      }, `${baseURL}/fx/iframe`);
    } finally {
      await browser.close();
    }
  });
}

test('native WebMCP reports unavailable documents explicitly', async ({page}) => {
  await page.goto('data:text/html,<title>No native WebMCP</title>');
  await expect(page.webmcp.tools()).rejects.toThrow(/WebMCP discovery is unavailable/);
});

for (const transport of ['pipe', 'ws'] as const) {
  test(`native WebMCP tools and results over ${transport}`, async () => {
    const browser = await chromium({transport}).launch({headless: true,
      args: ['--enable-features=WebMCP,WebMCPTesting,DevToolsWebMCPSupport']});
    try {
      await verifyWebMcp(await browser.newPage(), {
        equal: (actual, expected) => expect(actual).toBe(expected),
        deepEqual: (actual, expected) => expect(actual).toEqual(expected),
        ok: value => expect(Boolean(value)).toBe(true),
        rejects: (operation, expected) => expect(operation()).rejects.toThrow(expected),
      });
    } finally {
      await browser.close();
    }
  });
}

test('native Firefox WebMCP preserves top-level tool results, failures and lifecycle', async () => {
  const browser = await firefox().launch({headless: true, firefoxUserPrefs: {
    'dom.modelcontext.enabled': true,
    'dom.modelcontext.testing.enabled': true,
  }});
  try {
    const page = await browser.newPage();
    await page.route('http://127.0.0.1:47839/**', async (route: any) => {
      await route.fulfill({contentType: 'text/html', body: '<!doctype html><title>Firefox WebMCP</title><output id="result"></output>'});
    });
    await page.goto('http://127.0.0.1:47839/');
    expect(await page.webmcp.tools()).toEqual([]);
    await page.evaluate(() => {
      const model = (navigator as any).modelContext;
      (window as any).toolCalls = 0;
      model.registerTool({name: 'set_message', description: 'Updates the message',
        inputSchema: {type: 'object', properties: {message: {type: 'string'}}, required: ['message']},
        annotations: {readOnlyHint: true}, execute: async ({message}: any) => {
          document.querySelector('#result')!.textContent = message;
          return {message};
        }});
      model.registerTool({name: 'throws_once', description: 'Fails once', execute: async () => {
        (window as any).toolCalls++;
        throw new Error('sashoush tool failure');
      }});
      model.registerTool({name: 'no_input', description: 'No input', execute: async () => undefined});
      model.registerTool({name: 'error_result', description: 'Error result', execute: async () => ({isError: true})});
      const pending = new Promise(resolve => { (window as any).finishTool = resolve; });
      model.registerTool({name: 'pending', description: 'Waits for release', execute: () => {
        (window as any).toolStarted = true;
        return pending;
      }});
    });
    expect((await page.webmcp.tools()).find((tool: any) => tool.name === 'set_message')).toEqual({
      name: 'set_message', description: 'Updates the message',
      inputSchema: {type: 'object', properties: {message: {type: 'string'}}, required: ['message']},
      annotations: {readOnly: true},
    });
    expect(await page.webmcp.callTool('set_message', {message: 'sashoush'})).toEqual({message: 'sashoush'});
    expect(await page.locator('#result').textContent()).toBe('sashoush');
    expect(await page.webmcp.callTool('no_input')).toBeUndefined();
    expect(await page.webmcp.callTool('error_result')).toEqual({isError: true});
    await expect(page.webmcp.callTool('missing')).rejects.toThrow(/No WebMCP tool named "missing"/);
    await expect(page.webmcp.callTool('throws_once')).rejects.toThrow(/sashoush tool failure/);
    expect(Number(await page.evaluate(() => (window as any).toolCalls))).toBe(1);
    await expect(page.webmcp.callTool('pending', undefined, {timeout: 100})).rejects.toThrow(/Timeout|timeout/);
    await page.evaluate(() => (window as any).finishTool({released: true}));
    await page.evaluate(() => (navigator as any).modelContext.unregisterTool('set_message'));
    expect((await page.webmcp.tools()).some((tool: any) => tool.name === 'set_message')).toBe(false);
    await page.goto('http://127.0.0.1:47839/new-document');
    expect(await page.webmcp.tools()).toEqual([]);
    await page.evaluate(() => (navigator as any).modelContext.registerTool({
      name: 'until_close', description: 'Waits for closure', execute: () => {
        (window as any).toolStarted = true;
        return new Promise(() => {});
      },
    }));
    const pending = page.webmcp.callTool('until_close', undefined, {timeout: 0});
    const closed = expect(pending).rejects.toThrow(/closed|detached|destroyed/i);
    await page.waitForFunction(() => (window as any).toolStarted);
    await page.close();
    await closed;
  } finally {
    await browser.close();
  }
});

test('native Firefox WebMCP discovers tools outside the main realm', async ({baseURL}) => {
  const browser = await firefox().launch({headless:true, firefoxUserPrefs:{
    'dom.modelcontext.enabled':true, 'dom.modelcontext.testing.enabled':true,
  }});
  try {
    const page = await browser.newPage();
    await page.goto(`${baseURL}/fx/iframe`);
    await page.evaluate(() => {
      const model = (navigator as any).modelContext;
      model.registerTool({name:'native',description:'Accepts main-realm input',
        inputSchema:{type:'object'}, execute:(input: any) => ({message:input.message})});
      Object.defineProperty(model, 'getTools', {value:() => {throw new Error('page shadowed getTools');}});
    });
    expect((await page.webmcp.tools()).map(tool => tool.name)).toEqual(['native']);
    expect(await page.webmcp.callTool('native', {message:'sashoush'})).toEqual({message:'sashoush'});
  } finally {
    await browser.close();
  }
});

test('native Firefox exposes its current WebMCP registration limits', async () => {
  const browser = await firefox().launch({headless: true, firefoxUserPrefs: {
    'dom.modelcontext.enabled': true,
    'dom.modelcontext.testing.enabled': true,
  }});
  try {
    const page = await browser.newPage();
    await page.route('http://127.0.0.1:47839/**', async (route: any) => {
      await route.fulfill({contentType: 'text/html', body: '<!doctype html><iframe name="child"></iframe><form toolname="form_tool" tooldescription="A form"></form>'});
    });
    await page.goto('http://127.0.0.1:47839/');
    const child = page.frame('child');
    expect(child).toBeTruthy();
    await expect(child!.evaluate(() => (navigator as any).modelContext.registerTool({
      name: 'child_tool', description: 'Child tool', execute: async () => ({}),
    }))).rejects.toThrow(/top window/);
    await page.evaluate(() => {
      const registration = new AbortController();
      (navigator as any).modelContext.registerTool({name: 'signal_tool', description: 'Signal registration', execute: async () => ({})}, {signal: registration.signal});
      registration.abort();
    });
    const tools = await page.webmcp.tools();
    expect(tools.some((tool: any) => tool.name === 'signal_tool')).toBe(true);
    expect(tools.some((tool: any) => tool.name === 'form_tool')).toBe(false);
  } finally {
    await browser.close();
  }
});
