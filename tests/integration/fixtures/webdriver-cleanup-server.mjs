const server = Bun.serve({
  hostname: '127.0.0.1', port: 0,
  fetch(request, server) {
    if (request.headers.get('upgrade') === 'websocket') {
      if (server.upgrade(request)) return;
      return new Response(null, { status: 400 });
    }
    if (request.method === 'DELETE') {
      return Response.json({ value: { error: 'unknown error', message: 'cleanup rejected' } }, { status: 500 });
    }
    return Response.json({ value: {
      sessionId: 'cleanup-contract', capabilities: {
        browserName: 'firefox', browserVersion: 'contract',
        webSocketUrl: `ws://127.0.0.1:${server.port}/session/cleanup-contract`,
      },
    } });
  },
  websocket: {
    message(socket, message) {
      const command = JSON.parse(String(message));
      socket.send(JSON.stringify({ type: 'success', id: command.id,
        result: command.method === 'browsingContext.getTree' ? { contexts: [] } : {},
      }));
    },
  },
});
console.log(server.port);
