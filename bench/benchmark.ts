#!/usr/bin/env bun
/**
 * Benchmark chromey-mcp backends using the official MCP SDK client.
 * Uses a SINGLE persistent browser session per backend (how MCP clients really work).
 *
 * Tests functional correctness + measures per-operation latency.
 *   cdp-ws   — Chrome DevTools Protocol over WebSocket
 *   cdp-pipe — Chrome DevTools Protocol over pipes (fd 3/4)
 *   webkit   — Native WKWebView (macOS only)
 */

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const BINARY = process.env.FERRIDRIVER_BIN || "../target/debug/ferridriver";
const BACKENDS = ["cdp-ws", "cdp-pipe", "cdp-raw"];
if (process.platform === "darwin") {
  BACKENDS.push("webkit");
}

const TEST_PAGE = `data:text/html,${encodeURIComponent(`<!DOCTYPE html>
<html><head><title>Test Page</title></head>
<body>
  <h1 id="heading">Hello World</h1>
  <input id="name" type="text" placeholder="Name">
  <button id="btn" onclick="document.getElementById('heading').textContent='Clicked'">Click Me</button>
  <div id="output"></div>
  <div style="height:3000px"></div>
  <div id="bottom">Bottom</div>
</body></html>`)}`;

// ─── Helpers ────────────────────────────────────────────────────────────────

async function createClient(backend: string): Promise<Client> {
  const transport = new StdioClientTransport({
    command: BINARY,
    args: ["mcp", "--backend", backend],
  });
  const client = new Client(
    { name: "chromey-bench", version: "1.0.0" },
    { capabilities: {} },
  );
  await client.connect(transport);
  return client;
}

async function tool(client: Client, name: string, args: Record<string, any> = {}) {
  return client.callTool({ name, arguments: args });
}

function text(result: any): string {
  return result?.content?.[0]?.text ?? "";
}

function assert(cond: boolean, msg: string) {
  if (!cond) throw new Error(msg);
}

interface Result {
  backend: string;
  test: string;
  status: "PASS" | "FAIL";
  ms: number;
  error?: string;
}

// ─── Tests (run sequentially on a single session) ───────────────────────────

async function runAllTests(client: Client, backend: string): Promise<Result[]> {
  const results: Result[] = [];

  async function t(name: string, fn: () => Promise<void>) {
    const t0 = performance.now();
    try {
      await fn();
      results.push({ backend, test: name, status: "PASS", ms: Math.round(performance.now() - t0) });
    } catch (e: any) {
      results.push({ backend, test: name, status: "FAIL", ms: Math.round(performance.now() - t0), error: e.message?.substring(0, 80) });
    }
  }

  // Navigate to test page (first operation warms up the browser)
  await t("navigate", async () => {
    const r = await tool(client, "navigate", { url: TEST_PAGE });
    assert(!r.isError, `navigate: ${text(r)}`);
  });

  await t("evaluate_number", async () => {
    const r = await tool(client, "evaluate", { expression: "1 + 1" });
    assert(text(r).includes("2"), `expected 2, got: ${text(r)}`);
  });

  await t("evaluate_string", async () => {
    const r = await tool(client, "evaluate", { expression: "'hello world'" });
    assert(text(r).includes("hello"), `expected hello, got: ${text(r)}`);
  });

  await t("evaluate_dom", async () => {
    const r = await tool(client, "evaluate", {
      expression: "document.getElementById('heading').textContent",
    });
    assert(text(r).includes("Hello World"), `expected Hello World, got: ${text(r)}`);
  });

  await t("evaluate_promise", async () => {
    const r = await tool(client, "evaluate", { expression: "Promise.resolve(42)" });
    assert(text(r).includes("42"), `expected 42, got: ${text(r)}`);
  });

  await t("screenshot_png", async () => {
    const r = await tool(client, "screenshot", {});
    assert(!r.isError, `screenshot: ${text(r)}`);
    const img = r.content?.find((c: any) => c.type === "image");
    assert(!!img, "no image");
    assert(img.data?.startsWith("iVBOR"), "not PNG");
  });

  await t("screenshot_full_page", async () => {
    const r = await tool(client, "screenshot", { full_page: true });
    assert(!r.isError, `screenshot: ${text(r)}`);
  });

  await t("screenshot_jpeg", async () => {
    const r = await tool(client, "screenshot", { format: "jpeg", quality: 80 });
    assert(!r.isError, `screenshot jpeg: ${text(r)}`);
    const img = r.content?.find((c: any) => c.type === "image");
    assert(!!img, "no image");
  });

  await t("snapshot", async () => {
    const r = await tool(client, "snapshot", {});
    const s = text(r);
    assert(s.includes("[ref="), "no refs");
    assert(s.includes("Hello World"), "missing content");
  });

  // Re-navigate to reset state for click test
  await tool(client, "navigate", { url: TEST_PAGE });

  await t("click_selector", async () => {
    await tool(client, "click", { selector: "#btn" });
    const r = await tool(client, "evaluate", {
      expression: "document.getElementById('heading').textContent",
    });
    assert(text(r).includes("Clicked"), `heading not changed: ${text(r)}`);
  });

  await t("click_at", async () => {
    const r = await tool(client, "click_at", { x: 100, y: 100 });
    assert(!r.isError, `click_at: ${text(r)}`);
  });

  // Re-navigate for fill test
  await tool(client, "navigate", { url: TEST_PAGE });

  await t("fill_input", async () => {
    await tool(client, "fill", { selector: "#name", value: "Alice" });
    const r = await tool(client, "evaluate", {
      expression: "document.getElementById('name').value",
    });
    assert(text(r).includes("Alice"), `value not Alice: ${text(r)}`);
  });

  await t("type_text", async () => {
    await tool(client, "click", { selector: "#name" });
    const r = await tool(client, "type_text", { text: "Bob" });
    assert(!r.isError, `type: ${text(r)}`);
  });

  await t("press_key", async () => {
    const r = await tool(client, "press_key", { key: "Tab" });
    assert(!r.isError, `press: ${text(r)}`);
  });

  await t("scroll", async () => {
    const r = await tool(client, "scroll", { delta_y: 500 });
    assert(!r.isError, `scroll: ${text(r)}`);
  });

  await t("get_content", async () => {
    const r = await tool(client, "get_content", {});
    assert(text(r).includes("<"), "no HTML");
  });

  await t("set_content", async () => {
    await tool(client, "set_content", { html: "<html><body><h1>Replaced</h1></body></html>" });
    const r = await tool(client, "evaluate", {
      expression: "document.querySelector('h1')?.textContent",
    });
    assert(text(r).includes("Replaced"), `not replaced: ${text(r)}`);
  });

  // Navigate back for remaining tests
  await tool(client, "navigate", { url: TEST_PAGE });

  await t("reload", async () => {
    const r = await tool(client, "reload", {});
    assert(!r.isError, `reload: ${text(r)}`);
  });

  await t("go_back_forward", async () => {
    await tool(client, "navigate", { url: "data:text/html,<h1>Page2</h1>" });
    const r = await tool(client, "go_back", {});
    assert(!r.isError, `go_back: ${text(r)}`);
  });

  await t("list_sessions", async () => {
    const r = await tool(client, "list_sessions", {});
    assert(text(r).includes("default"), "no default session");
  });

  await t("wait_for_selector", async () => {
    const r = await tool(client, "wait_for", { selector: "#heading", timeout: 5000 });
    assert(!r.isError, `wait_for: ${text(r)}`);
  });

  await t("console_messages", async () => {
    await tool(client, "evaluate", { expression: "console.log('bench-test')" });
    await Bun.sleep(200);
    const r = await tool(client, "console_messages", {});
    assert(!r.isError, `console: ${text(r)}`);
  });

  await t("network_requests", async () => {
    const r = await tool(client, "network_requests", {});
    assert(!r.isError, `network: ${text(r)}`);
  });

  await t("run_scenario", async () => {
    const r = await tool(client, "run_scenario", {
      script: `Given I navigate to "${TEST_PAGE}"\nThen the page should contain text "Hello World"`,
    });
    assert(!r.isError, `scenario: ${text(r)}`);
  });

  // Navigate back for remaining tests
  await tool(client, "navigate", { url: TEST_PAGE });

  // ── Viewport / emulation ──

  await t("emulate_device", async () => {
    const r = await tool(client, "emulate_device", { width: 375, height: 812, mobile: true });
    assert(!r.isError, `emulate: ${text(r)}`);
    // Restore
    await tool(client, "emulate_device", { width: 1280, height: 720 });
  });

  await t("set_geolocation", async () => {
    const r = await tool(client, "set_geolocation", { latitude: 37.7749, longitude: -122.4194 });
    assert(!r.isError, `geo: ${text(r)}`);
  });

  await t("set_network_offline", async () => {
    await tool(client, "set_network_state", { state: "offline" });
    const r = await tool(client, "evaluate", { expression: "navigator.onLine" });
    assert(text(r).includes("false"), `should be offline: ${text(r)}`);
    await tool(client, "set_network_state", { state: "online" });
  });

  // ── Selectors ──

  await tool(client, "navigate", { url: TEST_PAGE });

  await t("selector_role", async () => {
    const r = await tool(client, "find_elements", { selector: 'role=button' });
    assert(!r.isError, `role: ${text(r)}`);
  });

  await t("selector_text", async () => {
    const r = await tool(client, "find_elements", { selector: 'text=Hello' });
    assert(text(r).includes("1"), `text: ${text(r)}`);
  });

  await t("selector_testid", async () => {
    // No testid in test page, but engine shouldn't error
    const r = await tool(client, "find_elements", { selector: 'css=#heading' });
    assert(!r.isError, `css: ${text(r)}`);
  });

  await t("selector_chain", async () => {
    const r = await tool(client, "find_elements", { selector: 'css=body >> css=#heading' });
    assert(text(r).includes("1"), `chain: ${text(r)}`);
  });

  // ── Search & find ──

  await t("search_page", async () => {
    const r = await tool(client, "search_page", { pattern: "Hello" });
    assert(text(r).includes("1"), `search: ${text(r)}`);
  });

  await t("find_elements_css", async () => {
    const r = await tool(client, "find_elements", { selector: "input", attributes: ["type", "id"] });
    assert(!r.isError, `find: ${text(r)}`);
  });

  // ── Dropdown ──

  await tool(client, "navigate", {
    url: `data:text/html,${encodeURIComponent('<select id="s"><option>A</option><option>B</option><option>C</option></select>')}`,
  });

  await t("select_option", async () => {
    const r = await tool(client, "select_option", { selector: "#s", label: "B" });
    assert(!r.isError, `select: ${text(r)}`);
  });

  await t("get_dropdown_options", async () => {
    const r = await tool(client, "get_dropdown_options", { selector: "#s" });
    assert(text(r).includes("A") && text(r).includes("B"), `options: ${text(r)}`);
  });

  // ── File upload ──

  await tool(client, "navigate", {
    url: `data:text/html,${encodeURIComponent('<input type="file" id="f">')}`,
  });

  await t("upload_file", async () => {
    // Write temp file
    const tmp = "/tmp/ferridriver_bench_upload.txt";
    await Bun.write(tmp, "bench content");
    const r = await tool(client, "upload_file", { selector: "#f", path: tmp });
    assert(!r.isError, `upload: ${text(r)}`);
  });

  // ── Markdown ──

  await tool(client, "navigate", { url: TEST_PAGE });

  await t("get_markdown", async () => {
    const r = await tool(client, "get_markdown", {});
    assert(text(r).includes("Hello"), `markdown: ${text(r)}`);
  });

  // ── Accessibility snapshot ──

  await t("a11y_snapshot", async () => {
    const r = await tool(client, "snapshot", {});
    assert(text(r).includes("[ref="), `a11y: ${text(r)}`);
  });

  // ── Dialog (alert should not hang) ──

  await tool(client, "navigate", {
    url: `data:text/html,${encodeURIComponent('<button id="d" onclick="alert(\'test\')">Alert</button>')}`,
  });

  await t("dialog_dismiss", async () => {
    await tool(client, "click", { selector: "#d" });
    const r = await tool(client, "evaluate", { expression: "'alive'" });
    assert(text(r).includes("alive"), `dialog: ${text(r)}`);
  });

  return results;
}

// ─── Main ───────────────────────────────────────────────────────────────────

async function main() {
  console.log("ferridriver Backend Benchmark (MCP SDK, persistent session)");
  console.log("=".repeat(70));
  console.log(`Backends: ${BACKENDS.join(", ")}\n`);

  const allResults: Result[] = [];
  const startupMs: Record<string, number> = {};

  for (const backend of BACKENDS) {
    console.log(`\n=== ${backend} ===`);

    // Measure startup: time from spawn to first successful tool call
    const t0 = performance.now();
    let client: Client;
    try {
      client = await createClient(backend);
    } catch (e: any) {
      console.log(`  SKIP: failed to connect (${e.message?.substring(0, 60)})`);
      continue;
    }
    const startupTime = Math.round(performance.now() - t0);
    startupMs[backend] = startupTime;
    console.log(`  Startup (connect): ${startupTime}ms`);
    console.log("-".repeat(55));

    // Run all tests on the same session
    const results = await runAllTests(client, backend);
    allResults.push(...results);

    let passed = 0, failed = 0;
    for (const r of results) {
      const tag = r.status === "PASS" ? "OK  " : "FAIL";
      const err = r.error ? ` (${r.error})` : "";
      console.log(`  ${tag} ${r.test.padEnd(28)} ${`${r.ms}ms`.padStart(8)}${err}`);
      r.status === "PASS" ? passed++ : failed++;
    }

    console.log("-".repeat(55));
    console.log(`  ${passed} passed, ${failed} failed`);

    try { await client.close(); } catch {}
    await Bun.sleep(500); // let browser clean up
  }

  // Comparison table
  console.log("\n\n=== Per-Operation Comparison (ms) ===");
  const testNames = [...new Set(allResults.map((r) => r.test))];
  const activeBe = BACKENDS.filter((b) => startupMs[b] !== undefined);
  const hdr = "  " + "Operation".padEnd(28) + activeBe.map((b) => b.padStart(12)).join("");
  console.log(hdr);
  console.log("  " + "-".repeat(hdr.length - 2));

  for (const name of testNames) {
    let line = `  ${name.padEnd(28)}`;
    for (const b of activeBe) {
      const r = allResults.find((x) => x.backend === b && x.test === name);
      line += (r?.status === "PASS" ? `${r.ms}ms` : "FAIL").padStart(12);
    }
    console.log(line);
  }

  // Summary
  console.log("\n=== Summary ===");
  for (const b of activeBe) {
    const br = allResults.filter((x) => x.backend === b);
    const p = br.filter((x) => x.status === "PASS");
    const totalOpMs = p.reduce((a, x) => a + x.ms, 0);
    const avgMs = p.length ? Math.round(totalOpMs / p.length) : 0;
    console.log(`  ${b.padEnd(12)} ${p.length}/${br.length} passed  avg op: ${avgMs}ms  total: ${totalOpMs}ms  startup: ${startupMs[b]}ms`);
  }

  // CSV
  const csv = [
    "backend,test,status,ms",
    ...allResults.map((r) => `${r.backend},${r.test},${r.status},${r.ms}`),
  ].join("\n");
  await Bun.write("./results/benchmark.csv", csv);
  console.log("\nSaved: bench/results/benchmark.csv");

  const totalFailed = allResults.filter((r) => r.status === "FAIL").length;
  process.exit(totalFailed > 0 ? 1 : 0);
}

main().catch((e) => {
  console.error("Benchmark failed:", e);
  process.exit(1);
});
