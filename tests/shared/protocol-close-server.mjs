const authenticated = process.argv.includes('--auth');
const states = Object.fromEntries(['cdp', 'bidi'].map(protocol => [protocol, {
  opened: 0,
  closed: 0,
  active: 0,
  commands: [],
}]));

function cdpResult(command) {
  switch (command.method) {
    case 'Browser.getVersion':
      return { product: 'Chrome/153.0.8010.36', userAgent: 'HeadlessChrome', protocolVersion: '1.3' };
    case 'Target.getTargets':
      return { targetInfos: [{
        targetId: 'page', type: 'page', title: 'sashoush fixture', url: 'about:blank', attached: true,
      }] };
    case 'Target.attachToTarget':
      return { sessionId: 'page-session' };
    case 'Target.attachToBrowserTarget':
      return { sessionId: 'browser-session' };
    case 'Page.getFrameTree':
      return { frameTree: { frame: {
        id: 'page', loaderId: 'loader', url: 'about:blank', name: '', securityOrigin: 'null',
      } } };
    case 'Page.addScriptToEvaluateOnNewDocument':
      return { identifier: 'script' };
    case 'Runtime.evaluate':
      return { result: { type: 'string', value: evaluationResult(command) } };
    default:
      return {};
  }
}

function bidiResult(command) {
  switch (command.method) {
    case 'session.new':
      return { sessionId: 'bidi-session', capabilities: { browserName: 'firefox', browserVersion: '155.0.1' } };
    case 'browsingContext.getTree':
      return { contexts: [{ context: 'page', userContext: 'default', url: 'about:blank', parent: null, children: [] }] };
    case 'script.evaluate':
    case 'script.callFunction':
      return { type: 'success', realm: 'realm', result: { type: 'string', value: evaluationResult(command) } };
    case 'script.addPreloadScript':
      return { script: 'script' };
    default:
      return {};
  }
}

function evaluationResult(command) {
  return command.params.expression === 'location.href' ? 'about:blank' : 'sashoush fixture';
}

const server = Bun.serve({
  hostname: '127.0.0.1',
  port: 0,
  fetch(request, server) {
    const url = new URL(request.url);
    if (url.pathname === '/status') {
      const state = states[url.searchParams.get('protocol')];
      return state ? Response.json(state) : new Response('Unknown protocol', { status: 404 });
    }
    if (authenticated && request.headers.get('authorization') !== 'Bearer sashoush-test-token')
      return new Response('Authorization required', {status:401});
    if (url.pathname === '/gateway/json/version') {
      return Response.json({webSocketDebuggerUrl:`ws://127.0.0.1:${server.port}/cdp`});
    }
    const protocol = url.pathname.slice(1);
    if (!Object.hasOwn(states, protocol)) return new Response('Unknown protocol', { status: 404 });
    if (server.upgrade(request, { data: { protocol, delay: Number(url.searchParams.get('delay') ?? 0), delayMethod: url.searchParams.get('method') } })) return;
    return new Response('WebSocket upgrade required', { status: 400 });
  },
  websocket: {
    open(socket) {
      const state = states[socket.data.protocol];
      state.opened++;
      state.active++;
    },
    close(socket) {
      const state = states[socket.data.protocol];
      state.closed++;
      state.active--;
    },
    async message(socket, raw) {
      const command = JSON.parse(String(raw));
      const protocol = socket.data.protocol;
      if (socket.data.delay && (!socket.data.delayMethod || command.method === socket.data.delayMethod)) await Bun.sleep(socket.data.delay);
      states[protocol].commands.push(command.method);
      if (protocol === 'bidi') {
        socket.send(JSON.stringify({ type: 'success', id: command.id, result: bidiResult(command) }));
        return;
      }
      if (command.method === 'Runtime.enable') {
        socket.send(JSON.stringify({
          method: 'Runtime.executionContextCreated',
          sessionId: command.sessionId,
          params: { context: {
            id: 1, origin: 'null', name: '', auxData: { isDefault: true, frameId: 'page' },
          } },
        }));
      }
      socket.send(JSON.stringify({
        id: command.id,
        result: cdpResult(command),
        ...(command.sessionId ? { sessionId: command.sessionId } : {}),
      }));
    },
  },
});

console.log(JSON.stringify({ port: server.port }));
