defineTool({
  name: 'ctxprobe.surface',
  description: 'Reports the handler context surface',
  exposeAsTool: true,
  inputSchema: { type: 'object', properties: {} },
  async handler(ctx) {
    return {
      // Session identity, split the same way the server routes it.
      sessionKey: ctx.session ? ctx.session.key : null,
      instance: ctx.session ? ctx.session.instance : null,
      contextName: ctx.session ? ctx.session.context : null,
      // Operator-supplied settings for this extension.
      settingsEnv: ctx.settings ? ctx.settings.env : null,
      settingsOrigin: ctx.settings ? ctx.settings.origin : null,
      // Capabilities that must be present, not just truthy names.
      hasVars: typeof ctx.vars?.set === 'function',
      hasFs: typeof ctx.fs?.readFileSync === 'function' && typeof ctx.fs?.promises?.readFile === 'function',
      hasArtifacts: typeof ctx.artifacts?.writeBytes === 'function',
      hasSidecars: typeof ctx.sidecars?.connect === 'function',
      hasLog: typeof ctx.log === 'function',
      logLevels: ['error', 'warn', 'info', 'debug', 'trace'].filter((l) => typeof ctx.log?.[l] === 'function'),
      // `log.enabled(level)` reflects the operator's tracing filter, so
      // a handler can skip building a payload nobody will record.
      hasEnabled: typeof ctx.log?.enabled === 'function',
      errorEnabled: ctx.log?.enabled('error'),
      bogusLevelEnabled: ctx.log?.enabled('nonsense'),
      hasCommands: typeof ctx.commands?.run === 'function',
      hasPage: typeof ctx.page?.goto === 'function',
    };
  },
});

defineTool({
  name: 'ctxprobe.remember',
  description: 'Writes a value through the durable session store',
  exposeAsTool: true,
  inputSchema: { type: 'object', properties: { value: { type: 'string' } } },
  async handler({ args, vars, log }) {
    log(`remembering ${args.value}`);
    log.debug('debug detail', { value: args.value });
    log.warn('warn detail');
    vars.set('ctxprobe', args.value);
    return { stored: args.value };
  },
});

defineTool({
  name: 'ctxprobe.recall',
  description: 'Reads back what a previous call stored',
  exposeAsTool: true,
  inputSchema: { type: 'object', properties: {} },
  async handler({ vars }) {
    return { recalled: vars.get('ctxprobe') ?? null };
  },
});