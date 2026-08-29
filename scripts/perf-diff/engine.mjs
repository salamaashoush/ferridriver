// Run the real devtools-frontend trace engine over a saved trace and
// print its verdicts in the shape `tests/differential.rs` compares
// against. Output is the recording; the comparison itself is Rust, so
// the mapping between their model and ours lives in one place.
//
//   node engine.mjs <trace.json>
//
// Handles a gzipped trace too, so the checked-in fixtures can be read
// without unpacking them first.
import fs from 'node:fs';
import zlib from 'node:zlib';
import { createRequire } from 'node:module';
import { DevTools } from 'chrome-devtools-mcp/build/src/third_party/index.js';
import { parseRawTraceBuffer } from 'chrome-devtools-mcp/build/src/processors/PerformanceTrace.js';

// The insights format their strings through DevTools' i18n, which the
// MCP server bootstraps at startup. Standalone it has to be done here or
// every insight fails with "No LanguageSelector instance exists yet".
// The locale instance must exist BEFORE the locale data is registered.
DevTools.I18n.DevToolsLocale.DevToolsLocale.instance({
  create: true,
  data: {
    navigatorLanguage: 'en-US',
    settingLanguage: 'en-US',
    lookupClosestDevToolsLocale: l => l,
  },
});
DevTools.I18n.i18n.registerLocaleDataForTest('en-US', {});

const require = createRequire(import.meta.url);
const enginePkg = require('chrome-devtools-mcp/package.json');

const file = process.argv[2];
if (!file) {
  console.error('usage: node engine.mjs <trace.json[.gz]>');
  process.exit(2);
}
let buffer = fs.readFileSync(file);
if (buffer[0] === 0x1f && buffer[1] === 0x8b) buffer = zlib.gunzipSync(buffer);

const result = await parseRawTraceBuffer(buffer, {});
if (!('parsedTrace' in result)) {
  console.error(`engine rejected ${file}: ${result.error}`);
  process.exit(1);
}
const { parsedTrace, insights } = result;

const out = { engine: `chrome-devtools-mcp@${enginePkg.version}`, metrics: {}, insights: {} };

// Page load metrics, per navigation.
const meta = parsedTrace.data?.Meta ?? parsedTrace.Meta;
const nav = meta?.mainFrameNavigations?.at(-1);
const plm = parsedTrace.data?.PageLoadMetrics ?? parsedTrace.PageLoadMetrics;
if (nav && plm) {
  const byFrame = plm.metricScoresByFrameId?.get(nav.args.frame);
  const scores = byFrame?.get(nav);
  if (scores) {
    for (const [name, score] of scores) {
      out.metrics[name] = Number((score.timing / 1000).toFixed(3));
    }
  }
}
const ls = parsedTrace.data?.LayoutShifts ?? parsedTrace.LayoutShifts;
if (ls?.sessionMaxScore !== undefined) out.metrics.CLS = ls.sessionMaxScore;

const nr = parsedTrace.data?.NetworkRequests ?? parsedTrace.NetworkRequests;
out.metrics.requests = nr?.byTime?.length ?? null;

// The Lantern metric estimates. These are not on any insight model, so
// the context has to be rebuilt the way the trace processor builds it.
// Without them the port's simulator is only ever checked through the
// savings that happen to use it.
try {
  const LCD = DevTools.TraceEngine.LanternComputationData;
  const L = DevTools.TraceEngine.Lantern;
  const meta2 = parsedTrace.data?.Meta ?? parsedTrace.Meta;
  const frameId = meta2.mainFrameId;
  const navigation = meta2.mainFrameNavigations.at(-1);
  const navStarts = meta2.navigationsByFrameId.get(frameId);
  const index = navStarts.findIndex(n => n === navigation);
  const startTime = navStarts[index].ts;
  const endTime = index + 1 < navStarts.length ? navStarts[index + 1].ts : Number.POSITIVE_INFINITY;
  const raw = JSON.parse(new TextDecoder().decode(buffer));
  const allEvents = Array.isArray(raw) ? raw : raw.traceEvents;
  const trace = { traceEvents: allEvents.filter(e => e.ts >= startTime && e.ts < endTime) };
  const data = parsedTrace.data ?? parsedTrace;
  const requests = LCD.createNetworkRequests(trace, data, startTime, endTime);
  const lanternGraph = LCD.createGraph(requests, trace, data);
  const processedNavigation = LCD.createProcessedNavigation(data, frameId, navigation);
  const networkAnalysis = L.Core.NetworkAnalyzer.analyze(requests);
  const simulator = L.Simulation.Simulator.createSimulator({ networkAnalysis, throttlingMethod: 'provided' });
  const computeData = { graph: lanternGraph, simulator, processedNavigation };
  const fcpResult = L.Metrics.FirstContentfulPaint.compute(computeData);
  const lcpResult = L.Metrics.LargestContentfulPaint.compute(computeData, { fcpResult });
  out.lantern = {
    rtt: Number(networkAnalysis.rtt.toFixed(6)),
    throughput: Number(networkAnalysis.throughput.toFixed(3)),
    observed: {
      firstContentfulPaintMs: Number(fcpResult.timing.toFixed(3)),
      largestContentfulPaintMs: Number(lcpResult.timing.toFixed(3)),
    },
  };
} catch (e) {
  // A page with no largest paint has no Lantern context at all, which
  // is a state the port has to reproduce rather than an error here.
  out.lantern = { error: String(e && e.message ? e.message : e) };
}

// Insights for the first insight set.
//
// The extraction is deliberately mechanical: state, the savings and
// checklist objects, every number-valued field, and the length of every
// array-valued one. Nothing here knows what any individual insight
// means, so this file never has to change when the Rust side starts
// comparing another quantity — only the recorded fixtures do.
if (insights) {
  // The first set is NO_NAVIGATION with an empty model; the one that
  // matters is the navigation's.
  for (const [, set] of insights) {
    if (!Object.keys(set.model ?? {}).length) continue;
    for (const [key, model] of Object.entries(set.model ?? {})) {
      if (!model || typeof model !== 'object') continue;
      const record = { state: model.state ?? null };
      if (model.metricSavings) record.metricSavings = model.metricSavings;

      // LCPDiscovery keeps its checklist at the top level, DocumentLatency
      // one level down under `data`. Both are `{name: {label, value}}`.
      const checklist = model.checklist ?? model.data?.checklist;
      if (checklist) {
        record.checklist = Object.fromEntries(
          Object.entries(checklist).map(([k, v]) => [k, v.value]));
      }

      const scalars = {};
      const counts = {};
      for (const source of [model, model.data]) {
        if (!source || typeof source !== 'object') continue;
        for (const [k, v] of Object.entries(source)) {
          if (typeof v === 'number') scalars[k] = v;
          else if (Array.isArray(v)) counts[k] = v.length;
        }
      }
      if (Object.keys(scalars).length) record.scalars = scalars;
      if (Object.keys(counts).length) record.counts = counts;
      out.insights[key] = record;
    }
    break;
  }
}
console.log(JSON.stringify(out, null, 1));
