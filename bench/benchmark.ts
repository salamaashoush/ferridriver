#!/usr/bin/env bun
/**
 * Benchmark ferridriver backends using the official MCP SDK client.
 * Uses a SINGLE persistent browser session per backend (how MCP clients really work).
 *
 * Tests functional correctness + measures per-operation latency.
 *   cdp-pipe — Chrome DevTools Protocol over pipes (fd 3/4)
 *   cdp-raw  — Raw CDP over WebSocket (fully parallel)
 *   webkit   — Native WKWebView (macOS only)
 */

import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";

const BINARY = process.env.FERRIDRIVER_BIN || "../target/debug/ferridriver";
const BACKENDS = ["cdp-pipe", "cdp-raw"];
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
    args: ["mcp", "--backend", backend, "--headless"],
  });
  const client = new Client(
    { name: "ferridriver-bench", version: "1.0.0" },
    { capabilities: {} },
  );
  await client.connect(transport);
  return client;
}

async function call(client: Client, name: string, args: Record<string, any> = {}) {
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

  // ── Navigation ──

  await t("navigate", async () => {
    const r = await call(client, "navigate", { url: TEST_PAGE });
    assert(!r.isError, `navigate: ${text(r)}`);
  });

  await t("page_reload", async () => {
    const r = await call(client, "page", { action: "reload" });
    assert(!r.isError, `reload: ${text(r)}`);
  });

  await t("page_back_forward", async () => {
    await call(client, "navigate", { url: "data:text/html,<h1>Page2</h1>" });
    const r = await call(client, "page", { action: "back" });
    assert(!r.isError, `back: ${text(r)}`);
    const r2 = await call(client, "page", { action: "forward" });
    assert(!r2.isError, `forward: ${text(r2)}`);
  });

  await t("page_list", async () => {
    const r = await call(client, "page", { action: "list" });
    assert(!r.isError, `list: ${text(r)}`);
  });

  await t("page_new_close", async () => {
    const r = await call(client, "page", { action: "new" });
    assert(!r.isError, `new: ${text(r)}`);
    // Close the new tab (index 1) and go back to original
    await call(client, "page", { action: "close", page_index: 1 });
  });

  // ── Evaluate ──

  await call(client, "navigate", { url: TEST_PAGE });

  await t("evaluate_number", async () => {
    const r = await call(client, "evaluate", { expression: "1 + 1" });
    assert(text(r).includes("2"), `expected 2, got: ${text(r)}`);
  });

  await t("evaluate_string", async () => {
    const r = await call(client, "evaluate", { expression: "'hello world'" });
    assert(text(r).includes("hello"), `expected hello, got: ${text(r)}`);
  });

  await t("evaluate_dom", async () => {
    const r = await call(client, "evaluate", {
      expression: "document.getElementById('heading').textContent",
    });
    assert(text(r).includes("Hello World"), `expected Hello World, got: ${text(r)}`);
  });

  await t("evaluate_promise", async () => {
    const r = await call(client, "evaluate", { expression: "Promise.resolve(42)" });
    assert(text(r).includes("42"), `expected 42, got: ${text(r)}`);
  });

  // ── Screenshots ──

  await t("screenshot_png", async () => {
    const r = await call(client, "screenshot", {});
    assert(!r.isError, `screenshot: ${text(r)}`);
    const img = r.content?.find((c: any) => c.type === "image");
    assert(!!img, "no image");
    assert(img.data?.startsWith("iVBOR"), "not PNG");
  });

  await t("screenshot_full_page", async () => {
    const r = await call(client, "screenshot", { full_page: true });
    assert(!r.isError, `screenshot: ${text(r)}`);
  });

  await t("screenshot_jpeg", async () => {
    const r = await call(client, "screenshot", { format: "jpeg", quality: 80 });
    assert(!r.isError, `screenshot jpeg: ${text(r)}`);
    const img = r.content?.find((c: any) => c.type === "image");
    assert(!!img, "no image");
  });

  // ── Snapshot ──

  await t("snapshot", async () => {
    const r = await call(client, "snapshot", {});
    const s = text(r);
    assert(s.includes("[ref="), "no refs");
    assert(s.includes("Hello World"), "missing content");
  });

  // ── Input ──

  await call(client, "navigate", { url: TEST_PAGE });

  await t("click_selector", async () => {
    await call(client, "click", { selector: "#btn" });
    const r = await call(client, "evaluate", {
      expression: "document.getElementById('heading').textContent",
    });
    assert(text(r).includes("Clicked"), `heading not changed: ${text(r)}`);
  });

  await t("click_at", async () => {
    const r = await call(client, "click_at", { x: 100, y: 100 });
    assert(!r.isError, `click_at: ${text(r)}`);
  });

  await call(client, "navigate", { url: TEST_PAGE });

  await t("fill_input", async () => {
    await call(client, "fill", { selector: "#name", value: "Alice" });
    const r = await call(client, "evaluate", {
      expression: "document.getElementById('name').value",
    });
    assert(text(r).includes("Alice"), `value not Alice: ${text(r)}`);
  });

  await t("type_text", async () => {
    await call(client, "click", { selector: "#name" });
    const r = await call(client, "type_text", { text: "Bob" });
    assert(!r.isError, `type: ${text(r)}`);
  });

  await t("press_key", async () => {
    const r = await call(client, "press_key", { key: "Tab" });
    assert(!r.isError, `press: ${text(r)}`);
  });

  await t("scroll", async () => {
    const r = await call(client, "scroll", { delta_y: 500 });
    assert(!r.isError, `scroll: ${text(r)}`);
  });

  // ── Content ──

  await t("get_markdown", async () => {
    const r = await call(client, "get_markdown", {});
    assert(text(r).includes("Hello"), `markdown: ${text(r)}`);
  });

  await t("search_page", async () => {
    const r = await call(client, "search_page", { pattern: "Hello" });
    assert(text(r).includes("1"), `search: ${text(r)}`);
  });

  await t("wait_for_selector", async () => {
    const r = await call(client, "wait_for", { selector: "#heading", timeout: 5000 });
    assert(!r.isError, `wait_for: ${text(r)}`);
  });

  await t("wait_for_text", async () => {
    const r = await call(client, "wait_for", { text: "Hello World", timeout: 5000 });
    assert(!r.isError, `wait_for text: ${text(r)}`);
  });

  // ── Emulation ──

  await t("emulate_viewport", async () => {
    const r = await call(client, "emulate", { width: 375, height: 812, mobile: true });
    assert(!r.isError, `emulate: ${text(r)}`);
    await call(client, "emulate", { width: 1280, height: 720 });
  });

  await t("emulate_geolocation", async () => {
    const r = await call(client, "emulate", { latitude: 37.7749, longitude: -122.4194 });
    assert(!r.isError, `geo: ${text(r)}`);
  });

  await t("emulate_network_offline", async () => {
    await call(client, "emulate", { network: "offline" });
    const r = await call(client, "evaluate", { expression: "navigator.onLine" });
    assert(text(r).includes("false"), `should be offline: ${text(r)}`);
    await call(client, "emulate", { network: "online" });
  });

  // ── Diagnostics ──

  await call(client, "navigate", { url: TEST_PAGE });

  await t("diagnostics_console", async () => {
    await call(client, "evaluate", { expression: "console.log('bench-test')" });
    await Bun.sleep(100);
    const r = await call(client, "diagnostics", { type: "console" });
    assert(!r.isError, `console: ${text(r)}`);
  });

  await t("diagnostics_network", async () => {
    const r = await call(client, "diagnostics", { type: "network" });
    assert(!r.isError, `network: ${text(r)}`);
  });

  await t("find_elements", async () => {
    const r = await call(client, "find_elements", { selector: "a", max_results: 5 });
    assert(!r.isError, `find_elements: ${text(r)}`);
  });

  // ── Dropdown ──

  await call(client, "navigate", {
    url: `data:text/html,${encodeURIComponent('<select id="s"><option>A</option><option>B</option><option>C</option></select>')}`,
  });

  await t("select_option", async () => {
    const r = await call(client, "select_option", { selector: "#s", label: "B" });
    assert(!r.isError, `select: ${text(r)}`);
  });

  // ── File upload ──

  await call(client, "navigate", {
    url: `data:text/html,${encodeURIComponent('<input type="file" id="f">')}`,
  });

  await t("upload_file", async () => {
    const tmp = "/tmp/ferridriver_bench_upload.txt";
    await Bun.write(tmp, "bench content");
    const r = await call(client, "upload_file", { selector: "#f", path: tmp });
    assert(!r.isError, `upload: ${text(r)}`);
  });

  // ── Storage ──

  await call(client, "navigate", { url: "https://example.com" });

  await t("storage_set_get", async () => {
    await call(client, "storage", { action: "set", key: "bench_key", value: "bench_val" });
    const r = await call(client, "storage", { action: "get", key: "bench_key" });
    assert(text(r).includes("bench_val"), `storage: ${text(r)}`);
  });

  // ── Cookies ──

  await t("cookies_set_get", async () => {
    await call(client, "cookies", { action: "set", name: "bench", value: "cookie", domain: "example.com" });
    const r = await call(client, "cookies", { action: "get" });
    assert(!r.isError, `cookies: ${text(r)}`);
  });

  // ── Dialog (alert should not hang) ──

  await call(client, "navigate", {
    url: `data:text/html,${encodeURIComponent('<button id="d" onclick="alert(\'test\')">Alert</button>')}`,
  });

  await t("dialog_dismiss", async () => {
    await call(client, "click", { selector: "#d" });
    const r = await call(client, "evaluate", { expression: "'alive'" });
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
    await Bun.sleep(500);
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
