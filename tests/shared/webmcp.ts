interface Assertions {
  equal(actual: unknown, expected: unknown): void;
  deepEqual(actual: unknown, expected: unknown): void;
  ok(value: unknown): void;
  rejects(operation: () => Promise<unknown>, expected: RegExp): Promise<unknown>;
}

export async function verifyWebMcp(page: any, assert: Assertions) {
  await page.route('http://127.0.0.1:47839/**', async (route: any) => {
    await route.fulfill({contentType: 'text/html', body: '<!doctype html><title>WebMCP</title><output id="result"></output>'});
  });
  await page.goto('http://127.0.0.1:47839/');
  assert.deepEqual(await page.webmcp.tools({timeout: 2000}), []);
  await page.evaluate(() => {
    const model = (document as any).modelContext ?? (navigator as any).modelContext;
    (window as any).toolCalls = 0;
    const registration = new AbortController();
    (window as any).removeMessageTool = () => registration.abort();
    model.registerTool({name: 'set_message', description: 'Updates the message',
      inputSchema: {type: 'object', properties: {message: {type: 'string'}}, required: ['message']},
      annotations: {readOnlyHint: true},
      execute: async ({message}: any) => {
        document.querySelector('#result')!.textContent = message;
        return {message};
      }}, {signal: registration.signal});
    model.registerTool({name: 'no_input', description: 'Returns without a schema', execute: async () => undefined});
    model.registerTool({name: 'throws_once', description: 'Reports a failure', execute: async () => {
      (window as any).toolCalls++;
      throw new Error('Failed to parse input arguments: sashoush');
    }});
    model.registerTool({name: 'error_result', description: 'Returns an error result', execute: async () => ({isError: true})});
    const pending = new Promise(resolve => { (window as any).finishTool = resolve; });
    model.registerTool({name: 'pending', description: 'Waits for release', execute: () => pending});
  });
  const tools = await page.webmcp.tools();
  assert.equal(tools.length, 5);
  assert.deepEqual(tools.find((tool: any) => tool.name === 'set_message'), {
    name: 'set_message', description: 'Updates the message',
    inputSchema: {type: 'object', properties: {message: {type: 'string'}}, required: ['message']},
    annotations: {readOnly: true},
  });
  assert.deepEqual(await page.webmcp.callTool('set_message', {message: 'sashoush'}, {timeout: 2000}), {message: 'sashoush'});
  assert.equal(await page.locator('#result').textContent(), 'sashoush');
  assert.equal(await page.webmcp.callTool('no_input'), undefined);
  assert.deepEqual(await page.webmcp.callTool('error_result'), {isError: true});
  await assert.rejects(() => page.webmcp.callTool('missing'), /No WebMCP tool named "missing"/);
  await assert.rejects(() => page.webmcp.callTool('throws_once'), /the invocation failed/);
  assert.equal(Number(await page.evaluate(() => (window as any).toolCalls)), 1);
  await assert.rejects(() => page.webmcp.callTool('pending', undefined, {timeout: 100}), /Timeout|timeout/);
  await page.evaluate(() => (window as any).finishTool({released: true}));
  await page.evaluate(() => {
    const child = document.createElement('iframe');
    child.name = 'tools-child';
    child.srcdoc = '<p>Child tools</p>';
    document.body.appendChild(child);
    return new Promise(resolve => child.onload = resolve);
  });
  const child = page.frame('tools-child');
  assert.ok(child);
  await child.evaluate(() => {
    const model = (document as any).modelContext ?? (navigator as any).modelContext;
    model.registerTool({name: 'child_only', description: 'Child frame tool', execute: async () => ({frame: 'child'})});
  });
  assert.equal((await page.webmcp.tools()).some((tool: any) => tool.name === 'child_only'), false);
  assert.deepEqual((await child.webmcp.tools()).map((tool: any) => tool.name), ['child_only']);
  assert.deepEqual(await child.webmcp.callTool('child_only'), {frame: 'child'});
  await page.route('http://localhost:47839/**', async (route: any) => {
    await route.fulfill({contentType: 'text/html', body: '<!doctype html><title>Remote frame tools</title>'});
  });
  await page.evaluate(() => {
    const remote = document.createElement('iframe');
    remote.name = 'remote-tools';
    remote.allow = 'tools';
    remote.src = 'http://localhost:47839/tools';
    document.body.appendChild(remote);
    return new Promise(resolve => remote.onload = resolve);
  });
  const remote = page.frame('remote-tools');
  assert.ok(remote);
  await remote.evaluate(() => {
    const model = (document as any).modelContext ?? (navigator as any).modelContext;
    return model.registerTool({name: 'remote_only', description: 'Remote frame tool', execute: async () => ({frame: 'remote'})});
  });
  assert.deepEqual((await remote.webmcp.tools()).map((tool: any) => tool.name), ['remote_only']);
  assert.deepEqual(await remote.webmcp.callTool('remote_only'), {frame: 'remote'});
  assert.equal((await page.webmcp.tools()).some((tool: any) => tool.name === 'remote_only'), false);
  const remoteHandle = await remote.evaluateHandle(() => ({value: 42}));
  try {
    assert.equal(await remoteHandle.evaluate((value: any) => value.value), 42);
    const property = await remoteHandle.getProperty('value');
    try { assert.equal(await property.jsonValue(), 42); } finally { await property.dispose(); }
  } finally {
    await remoteHandle.dispose();
  }
  for (const origin of ['127.0.0.1', 'localhost']) {
    await page.evaluate((origin: string) => {
      const frame = document.querySelector('iframe[name="remote-tools"]') as HTMLIFrameElement;
      const loaded = new Promise(resolve => frame.onload = resolve);
      frame.src = `http://${origin}:47839/returned`;
      return loaded;
    }, origin);
    assert.equal(remote.isDetached(), false);
    assert.deepEqual(await remote.webmcp.tools(), []);
    assert.equal(remote.parentFrame().url(), page.url());
  }
  await page.evaluate(() => (window as any).removeMessageTool());
  assert.equal((await page.webmcp.tools()).some((tool: any) => tool.name === 'set_message'), false);
  for (const autosubmit of [false, true]) {
    const name = autosubmit ? 'subscribe_auto' : 'subscribe_manual';
    await page.evaluate(({name, autosubmit}: {name: string; autosubmit: boolean}) => {
      const form = document.createElement('form');
      form.setAttribute('toolname', name);
      form.setAttribute('tooldescription', 'Subscribes with an observable result');
      if (autosubmit) form.setAttribute('toolautosubmit', '');
      form.innerHTML = `<input name="email" type="email" required><button>${name}</button>`;
      let clicks = 0;
      let trusted: boolean | null = null;
      form.querySelector('button')!.addEventListener('click', event => {
        clicks++;
        trusted = event.isTrusted;
      });
      form.addEventListener('submit', (event: any) => {
        event.preventDefault();
        event.respondWith(Promise.resolve({
          email: form.querySelector<HTMLInputElement>('input')!.value,
          agentInvoked: event.agentInvoked,
          clicks,
          trusted,
        }));
      });
      document.body.appendChild(form);
    }, {name, autosubmit});
    const pending = page.webmcp.callTool(name, {email: 'sashoush@example.com'});
    if (!autosubmit) {
      await page.waitForFunction((name: string) =>
        document.querySelector<HTMLInputElement>(`form[toolname="${name}"] input`)?.value === 'sashoush@example.com', name);
      await page.getByRole('button', {name, exact: true}).click();
    }
    assert.deepEqual(await pending, {
      email: 'sashoush@example.com', agentInvoked: true,
      clicks: autosubmit ? 0 : 1, trusted: autosubmit ? null : true,
    });
    await page.evaluate((name: string) => document.querySelector(`form[toolname="${name}"]`)!.remove(), name);
  }
  const registerForm = () => page.evaluate(() => {
    const form = document.createElement('form');
    form.setAttribute('toolname', 'subscribe');
    form.setAttribute('tooldescription', 'Subscribes to the newsletter');
    form.innerHTML = '<label>Email<input name="email" type="email" required></label><button>Subscribe</button>';
    document.body.appendChild(form);
  });
  await registerForm();
  const formTool = (await page.webmcp.tools()).find((tool: any) => tool.name === 'subscribe');
  assert.equal(formTool?.description, 'Subscribes to the newsletter');
  assert.equal(formTool.inputSchema.properties.email.type, 'string');
  await assert.rejects(() => page.webmcp.callTool('subscribe', {email: 'sashoush@example.com'}, {timeout: 100}), /Timeout|timeout/);
  await page.goto('http://127.0.0.1:47839/form-document');
  await registerForm();
  const formCall = page.webmcp.callTool('subscribe', {email: 'sashoush@example.com'}, {timeout: 0});
  const formClosed = assert.rejects(() => formCall, /closed|detached|destroyed/i);
  await page.waitForFunction(() => document.querySelector<HTMLInputElement>('input[name="email"]')?.value === 'sashoush@example.com');
  assert.equal(await page.locator('input[name="email"]').inputValue(), 'sashoush@example.com');
  await page.goto('http://127.0.0.1:47839/new-document');
  await formClosed;
  assert.deepEqual(await page.webmcp.tools(), []);
  await assert.rejects(() => child.webmcp.tools(), /detached|closed/);
  await page.evaluate(() => {
    const model = (document as any).modelContext ?? (navigator as any).modelContext;
    return model.registerTool({name: 'until_close', description: 'Waits for document closure', execute: () => {
      (window as any).closeToolStarted = true;
      return new Promise(() => {});
    }});
  });
  const pending = page.webmcp.callTool('until_close', undefined, {timeout: 0});
  const closed = assert.rejects(() => pending, /closed|detached|destroyed/i);
  await page.waitForFunction(() => (window as any).closeToolStarted);
  await page.close();
  await closed;
}
