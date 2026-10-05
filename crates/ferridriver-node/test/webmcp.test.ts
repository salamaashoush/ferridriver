import {test} from 'bun:test';
import assert from 'node:assert/strict';
import {chromium} from '../index.js';
import {verifyWebMcp} from '../../../tests/shared/webmcp';

for (const transport of ['pipe', 'ws'] as const) {
  test(`native WebMCP tools and results over ${transport}`, async () => {
    const browser = await chromium({transport}).launch({headless: true,
      args: ['--enable-features=WebMCP,WebMCPTesting,DevToolsWebMCPSupport']});
    try {
      await verifyWebMcp(await browser.newPage(), assert);
    } finally {
      await browser.close();
    }
  }, 30000);
}
