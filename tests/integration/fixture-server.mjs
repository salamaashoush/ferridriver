import assert from 'node:assert/strict';
import { join } from 'node:path';
import { repo } from './support.mjs';

export async function fixtureServer() {
  await commands.open('fixtures', { binary: join(repo, 'target/debug/ferridriver-fixtures') });
  try {
    const line = await commands.read('fixtures');
    const url = line?.match(/serving (http:\/\/127\.0\.0\.1:\d+)/)?.[1];
    assert.ok(url, `fixture server did not report its address: ${line}`);
    return { url, close: () => commands.stop('fixtures') };
  } catch (error) {
    await commands.stop('fixtures');
    throw error;
  }
}
