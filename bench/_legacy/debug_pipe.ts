#!/usr/bin/env bun
/**
 * Debug script for cdp-pipe backend.
 * Tests step by step with full logging.
 */

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const BINARY = "../target/release/chromey-mcp";

async function debug(backend: string) {
  console.log(`\n=== Debugging ${backend} backend ===\n`);

  console.log("1. Creating transport...");
  const transport = new StdioClientTransport({
    command: BINARY,
    args: ["--backend", backend],
    stderr: "pipe",
  });

  // Listen for stderr
  transport.stderr?.on?.("data", (data: Buffer) => {
    console.log(`   [STDERR] ${data.toString().trim()}`);
  });

  console.log("2. Creating client...");
  const client = new Client(
    { name: "debug", version: "1.0.0" },
    { capabilities: {} },
  );

  console.log("3. Connecting (initialize)...");
  const t0 = performance.now();
  try {
    await client.connect(transport);
    console.log(`   Connected in ${Math.round(performance.now() - t0)}ms`);
  } catch (e: any) {
    console.error(`   FAILED to connect: ${e.message}`);
    process.exit(1);
  }

  console.log("4. Calling navigate...");
  const t1 = performance.now();
  try {
    const r = await client.callTool({
      name: "navigate",
      arguments: { url: "data:text/html,<h1>Hello</h1>" },
    });
    console.log(`   Navigate done in ${Math.round(performance.now() - t1)}ms`);
    const text = (r.content as any)?.[0]?.text?.substring(0, 200);
    console.log(`   Result: ${text}...`);
  } catch (e: any) {
    console.error(`   Navigate FAILED: ${e.message}`);
  }

  console.log("5. Calling evaluate...");
  const t2 = performance.now();
  try {
    const r = await client.callTool({
      name: "evaluate",
      arguments: { expression: "document.title" },
    });
    console.log(`   Evaluate done in ${Math.round(performance.now() - t2)}ms`);
    console.log(`   Result: ${(r.content as any)?.[0]?.text}`);
  } catch (e: any) {
    console.error(`   Evaluate FAILED: ${e.message}`);
  }

  console.log("6. Calling screenshot...");
  const t3 = performance.now();
  try {
    const r = await client.callTool({ name: "screenshot", arguments: {} });
    console.log(`   Screenshot done in ${Math.round(performance.now() - t3)}ms`);
    const img = (r.content as any)?.find((c: any) => c.type === "image");
    console.log(`   Has image: ${!!img}, starts with: ${img?.data?.substring(0, 10)}`);
  } catch (e: any) {
    console.error(`   Screenshot FAILED: ${e.message}`);
  }

  console.log("7. Closing...");
  try {
    await client.close();
    console.log("   Closed OK");
  } catch (e: any) {
    console.log(`   Close: ${e.message}`);
  }

  console.log(`\n=== ${backend} debug complete ===\n`);
}

// Run each backend one at a time
const backends = process.argv.slice(2);
if (backends.length === 0) {
  backends.push("cdp-pipe"); // default to the broken one
}

for (const b of backends) {
  await debug(b);
  await Bun.sleep(1000);
}
