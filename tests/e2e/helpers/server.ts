// URL builders and observers for the fixture server routes
// (crates/ferridriver-fixtures), reachable through the configured
// baseURL. Relative `page.goto('/fx/...')` also works — these helpers
// are for the places that need an absolute URL (WebSocket endpoints,
// cross-context requests) or the proxy observers.

import type { APIRequestContext } from '@ferridriver/test';

export function fxUrl(baseURL: string | undefined, path: string): string {
  if (!baseURL) {
    throw new Error('baseURL is not configured — is the fixture webServer entry missing?');
  }
  return `${baseURL}/fx${path.startsWith('/') ? path : `/${path}`}`;
}

export function fxWsUrl(baseURL: string | undefined): string {
  return fxUrl(baseURL, '/ws').replace(/^http/, 'ws');
}

export interface FxProxyLog {
  hits: number;
  lines: string[];
}

export async function fxProxyUrl(request: APIRequestContext, baseURL: string | undefined): Promise<string> {
  const resp = await request.get(fxUrl(baseURL, '/proxy-info'));
  const info = (await resp.json()) as { url: string };
  return info.url;
}

export async function fxProxyLog(request: APIRequestContext, baseURL: string | undefined): Promise<FxProxyLog> {
  const resp = await request.get(fxUrl(baseURL, '/proxy-log'));
  return (await resp.json()) as FxProxyLog;
}

export async function fxProxyLogReset(request: APIRequestContext, baseURL: string | undefined): Promise<void> {
  await request.delete(fxUrl(baseURL, '/proxy-log'));
}
