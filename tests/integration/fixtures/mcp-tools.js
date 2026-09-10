defineTool({
  name: 'typed_greet',
  title: 'Typed Greeter',
  description: 'Greets a user',
  exposeAsTool: true,
  annotations: { readOnlyHint: true, openWorldHint: false },
  inputSchema: {
    type: 'object',
    properties: { user: { type: 'string' } },
    required: ['user'],
    additionalProperties: false,
  },
  outputSchema: {
    type: 'object',
    properties: { greeting: { type: 'string' } },
    required: ['greeting'],
    additionalProperties: false,
  },
  handler: async ({ args }) => ({ greeting: 'hi ' + args.user }),
});

defineTool({
  name: 'bad_output',
  exposeAsTool: true,
  outputSchema: { type: 'object', properties: { ok: { type: 'boolean' } }, required: ['ok'] },
  handler: async () => ({ ok: 'not-a-boolean' }),
});

defineTool({
  name: 'cap_register',
  allow: { net: ['127.0.0.1'] },
  // The registrations are awaited SEQUENTIALLY so exposeFunction does
  // not race the per-document binding-channel bootstrap it needs. Each
  // callback fires later, cross-task, and probes twice: through the
  // `fetch` the handler was handed, and through the realm's global.
  handler: ({ page, fetch }) => {
    globalThis.__cap = {};
    const completed = {};
    globalThis.__capDone = Promise.all(['pageOn', 'route', 'exposeFn', 'globalFetch'].map(key =>
      new Promise(resolve => { completed[key] = resolve; })));
    const probe = async (k, f) => {
      try {
        await f('http://blocked.test/');
        globalThis.__cap[k] = 'ALLOWED';
      } catch (e) {
        globalThis.__cap[k] = String((e && e.message) || e);
      } finally {
        completed[k]();
      }
    };
    page.on('console', (m) => { if (m.text() === 'cap-console') probe('pageOn', fetch); });
    const pRoute = page.route('**/cap-route**', async (r) => {
      await probe('route', fetch);
      await r.fulfill({ status: 200, body: 'ok' });
    });
    const pExpose = page.exposeFunction('__capProbe', async () => {
      await probe('exposeFn', fetch);
      await probe('globalFetch', globalThis.fetch);
      return 1;
    });
    return (async () => {
      await pRoute;
      await pExpose;
      return 'registered';
    })();
  },
});
