// Run the real devtools-frontend trace engine over a saved trace and
// print the metrics and insights in a form comparable to ours.
import fs from 'node:fs';
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

const file = process.argv[2];
const buffer = fs.readFileSync(file);
const result = await parseRawTraceBuffer(buffer, {});
if (!('parsedTrace' in result)) {
  console.log(JSON.stringify({ error: result.error }));
  process.exit(1);
}
const { parsedTrace, insights } = result;

const out = { metrics: {}, insights: {} };

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

// Insights for the first insight set.
if (insights) {
  // The first set is NO_NAVIGATION with an empty model; the one that
  // matters is the navigation's.
  for (const [, set] of insights) {
    if (!Object.keys(set.model ?? {}).length) continue;
    for (const [key, model] of Object.entries(set.model ?? {})) {
      if (!model || typeof model !== 'object') continue;
      out.insights[key] = { state: model.state ?? null };
      if (model.metricSavings) out.insights[key].metricSavings = model.metricSavings;
      if (typeof model.wastedBytes === 'number') out.insights[key].wastedBytes = model.wastedBytes;
      if (model.checklist) {
        out.insights[key].checklist = Object.fromEntries(
          Object.entries(model.checklist).map(([k, v]) => [k, v.value]));
      }
    }
    break;
  }
}
console.log(JSON.stringify(out, null, 1));
