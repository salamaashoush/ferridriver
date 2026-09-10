import assert from 'node:assert/strict';
import { binary, workspace } from './support.mjs';

export const dataUrl = html => `data:text/html,${encodeURIComponent(html)}`;
export const isError = response => response.error !== undefined || response.result?.isError === true;
export function ok(response) {
  assert.equal(isError(response), false, JSON.stringify(response));
  return response;
}
export function payload(response) {
  for (const block of response.result?.content ?? []) {
    if (block.type !== 'text') continue;
    try {
      const value = JSON.parse(block.text);
      if (value.status !== undefined) return value;
    } catch {}
  }
  assert.fail(`missing script payload: ${JSON.stringify(response)}`);
}

export class McpClient {
  constructor() {
    this.id = 0;
    this.pending = new Map();
    this.reading = false;
  }
  static async launch(backend = 'cdp-pipe', config, { log = 'warn' } = {}) {
    const client = new McpClient();
    config ??= `${await workspace({ 'ferridriver.toml': '' })}/ferridriver.toml`;
    client.registry = await workspace({});
    await commands.open('mcp', { binary, backend, config, registry: client.registry, log });
    try {
      client.initialized = await client.request('initialize', {
        protocolVersion: '2024-11-05', capabilities: {},
        clientInfo: { name: 'ferridriver-native-tests', version: '1.0.0' },
      });
      ok(client.initialized);
      await client.send({ method: 'notifications/initialized' });
      return client;
    } catch (error) {
      try { await commands.stop('mcp'); }
      catch (cleanup) { throw new Error(`${error.message}; cleanup failed: ${cleanup.message}`); }
      throw error;
    }
  }
  async send(message) {
    await commands.write('mcp', JSON.stringify({ jsonrpc: '2.0', ...message }) + '\n');
  }
  async request(method, params, progress = []) {
    const id = ++this.id;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, progress, token: params?._meta?.progressToken });
      this.send({ id, method, params }).then(() => {
        if (!this.reading) this.readResponses();
      }, error => {
        this.pending.delete(id);
        reject(error);
      });
    });
  }
  async readResponses() {
    this.reading = true;
    try {
      while (this.pending.size) {
        const line = await commands.read('mcp');
        if (line === null) {
          const status = await commands.status('mcp');
          assert.fail(`MCP closed stdout with ${this.pending.size} pending requests: ${JSON.stringify(status)}`);
        }
        if (!line.trim()) continue;
        const message = JSON.parse(line);
        if (message.method === 'notifications/progress') {
          for (const pending of this.pending.values()) {
            if (pending.token === message.params.progressToken) pending.progress.push(message.params);
          }
        }
        const pending = this.pending.get(message.id);
        if (pending) {
          this.pending.delete(message.id);
          pending.resolve(message);
        }
      }
    } catch (error) {
      for (const pending of this.pending.values()) pending.reject(error);
      this.pending.clear();
    } finally {
      this.reading = false;
    }
  }
  call(name, args = {}) { return this.request('tools/call', { name, arguments: args }); }
  async script(source, args = []) {
    const value = payload(ok(await this.call('run_script', { source, args })));
    assert.equal(value.status, 'ok', JSON.stringify(value));
    return value.value;
  }
  async close() {
    try {
      await commands.write('mcp', null);
      assert.equal(await commands.wait('mcp', 2000), 0, 'MCP must exit successfully after stdin closes');
    } finally {
      await commands.stop('mcp');
    }
  }
}
