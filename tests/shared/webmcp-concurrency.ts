export async function verifyWebMcpConcurrency(page: any, assert: {
  equal(actual: unknown, expected: unknown): void;
  ok(value: unknown): void;
}, url: string) {
  await page.goto(url);
  await page.evaluate(() => {
    const button = document.createElement('button');
    button.id = 'release-tool';
    button.textContent = 'Release';
    document.body.appendChild(button);
    (window as any).toolCalls = 0;
    const model = (document as any).modelContext ?? (navigator as any).modelContext;
    model.registerTool({name: 'gated', description: 'Waits for trusted input', execute: () => {
      (window as any).toolCalls++;
      (window as any).toolStarted = true;
      return new Promise(resolve => button.addEventListener('click', event => resolve({trusted: event.isTrusted}), {once: true}));
    }});
  });
  const pending = page.webmcp.callTool('gated', {}, {timeout: 3000})
    .then((value: any) => ({value}), (error: any) => ({error}));
  const started = page.waitForFunction(() => (window as any).toolStarted, undefined, {timeout: 1000, polling: 10});
  await Promise.race([started, pending.then((result: {error?: unknown}) => {
    if (result.error) throw result.error;
    return started;
  })]);
  assert.ok((await page.webmcp.tools({timeout: 1000})).some((tool: any) => tool.name === 'gated'));
  await page.click('#release-tool');
  const result = await pending;
  if (result.error) throw result.error;
  assert.equal(result.value.trusted, true);
  assert.equal(Number(await page.evaluate(() => (window as any).toolCalls)), 1);
}
