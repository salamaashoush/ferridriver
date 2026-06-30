// Entry point for the WebSocket-mock init script. Bundled to
// dist/websocket-mock.min.js and injected (via addInitScript) when a
// `routeWebSocket` handler is registered. Running it installs the
// `globalThis.WebSocket` override and the `__pwWebSocketDispatch` hook;
// `inject` is idempotent (guards on `__pwWebSocketDispatch`).
import { inject } from './webSocketMock';

inject(globalThis);
