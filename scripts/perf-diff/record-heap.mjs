// Record what DevTools' own heap snapshot engine concludes about each
// fixture snapshot, so a port can be checked against the engine rather
// than against someone's reading of the format.
//
//   node record-heap.mjs --capture   # re-take the .heapsnapshot files
//   node record-heap.mjs             # check the recordings are current
//   node record-heap.mjs --update    # re-record
//
// The engine is the real `devtools-heap-snapshot-worker.js` that ships
// inside chrome-devtools-mcp, driven through the same
// `HeapSnapshotWorkerProxy` its own tools use. Nothing here is
// reimplemented, so a disagreement means the Rust port is wrong.
//
// Capture is separated from recording on purpose. A heap snapshot of a
// live page differs run to run -- object ids, addresses, how much of V8
// happens to be alive -- so the SNAPSHOT is what gets checked in, and
// everything downstream is a function of that file. `--capture` is the
// only step that needs a browser, and re-running it is a deliberate act
// that changes what we are measured against.

import { createReadStream, readFileSync, writeFileSync } from 'node:fs';
import { gunzipSync, gzipSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';
import { tmpdir } from 'node:os';

import { DevTools } from 'chrome-devtools-mcp/build/src/third_party/index.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '../..');
const FIXTURES = join(ROOT, 'crates/ferridriver-heap/tests/fixtures');

/** Fixture pages, each captured to `<name>.heapsnapshot.gz` beside it. */
const PAGES = ['leaky'];

/**
 * Snapshots built by `make-heapsnapshot.mjs` rather than captured, and
 * stored plain because they are small enough to read.
 *
 * A browser will not produce a snapshot with user roots in it, so the
 * three passes that need them are only reachable from a hand-built one.
 */
const HANDMADE = ['handmade'];

const ALL = [...PAGES, ...HANDMADE];

/** How many nodes the per-node comparisons cover. */
const SAMPLE_SIZE = 200;

/** How many of those the edge and retainer comparisons cover. */
const QUERY_SAMPLE_SIZE = 25;

// ── Capture ─────────────────────────────────────────────────────────────

async function capture(name) {
  const puppeteer = (await import('puppeteer-core')).default;
  const executablePath = process.env.CHROME_PATH ?? findChrome();
  const browser = await puppeteer.launch({ executablePath, headless: true, args: ['--no-sandbox'] });
  try {
    const page = await browser.newPage();
    await page.goto(`file://${join(FIXTURES, `${name}.html`)}`, { waitUntil: 'networkidle0' });
    // Puppeteer's own `captureHeapSnapshot`, which is exactly what
    // chrome-devtools-mcp's `take_heapsnapshot` calls: it collects
    // garbage first and takes the snapshot on the PRIMARY target
    // client. Taking it on a session of one's own instead produces a
    // snapshot whose root has no user roots under it at all, which
    // silently skips the shallow-size pass, the page-object marking and
    // the first half of the distance walk.
    const plain = join(tmpdir(), `ferridriver-heap-capture-${name}-${process.pid}.heapsnapshot`);
    await page.captureHeapSnapshot({ path: plain });
    const text = readFileSync(plain, 'utf-8');
    const out = join(FIXTURES, `${name}.heapsnapshot.gz`);
    // Gzipped because a snapshot of even a trivial page is megabytes.
    const stored = gzipSync(text, { level: 9 });
    writeFileSync(out, stored);
    console.log(`captured ${relative(ROOT, out)} (${text.length} bytes, ${stored.length} stored)`);
  } finally {
    await browser.close();
  }
}

function findChrome() {
  throw new Error('set CHROME_PATH, or run: ferridriver install chromium');
}

// ── The engine ──────────────────────────────────────────────────────────

/** Load a fixture into the real worker and hand back its snapshot proxy. */
async function load(name) {
  const gz = join(FIXTURES, `${name}.heapsnapshot.gz`);
  const text = HANDMADE.includes(name)
    ? readFileSync(join(FIXTURES, `${name}.heapsnapshot`), 'utf-8')
    : gunzipSync(readFileSync(gz)).toString('utf-8');
  const plain = join(tmpdir(), `ferridriver-heap-${name}-${process.pid}.heapsnapshot`);
  writeFileSync(plain, text);
  // The engine's `staticData` does not carry the field layout, and
  // `getObjectInfo` is addressed by node INDEX rather than ordinal, so
  // the width comes from the file's own meta.
  const nodeFieldCount = JSON.parse(`${text.slice(0, text.indexOf(',"nodes"'))}}`).snapshot.meta.node_fields.length;

  const worker = new DevTools.HeapSnapshotModel.HeapSnapshotProxy.HeapSnapshotWorkerProxy(
    () => {},
    DevTools.Common.Console.Console.instance(),
    import.meta.resolve('chrome-devtools-mcp/build/src/third_party/devtools-heap-snapshot-worker.js'),
  );
  const { promise, resolve } = Promise.withResolvers();
  const loader = worker.createLoader(1, snapshot => resolve(snapshot));
  for await (const chunk of createReadStream(plain, { encoding: 'utf-8', highWaterMark: 1024 * 1024 })) {
    await loader.write(chunk);
  }
  await loader.close();
  return { snapshot: await promise, worker, nodeFieldCount };
}

/**
 * What the engine concluded. Both the derived summary AND the model
 * underneath it: a comparison of conclusions alone stayed exact for a
 * whole session on `ferridriver-perf` while the analyser beneath it had
 * one RTT estimator where upstream has four.
 */
async function analyse(snapshot, nodeFieldCount) {
  const statistics = await snapshot.getStatistics();
  const staticData = snapshot.staticData;

  // A deterministic spread over the whole node array rather than the
  // first N, so the sample cannot sit entirely in one region of the
  // heap and agree by accident.
  const nodeCount = staticData.nodeCount;
  const stride = Math.max(1, Math.floor(nodeCount / SAMPLE_SIZE));
  const nodes = [];
  for (let ordinal = 0; ordinal < nodeCount && nodes.length < SAMPLE_SIZE; ordinal += stride) {
    const info = await snapshot.getObjectInfo(ordinal * nodeFieldCount);
    nodes.push({
      ordinal,
      id: info.id,
      name: info.name,
      type: info.type,
      selfSize: info.selfSize,
      retainedSize: info.retainedSize,
      distance: info.distance,
      detachedness: info.detachedness,
    });
  }

  // The node-addressed queries, over a smaller sub-sample: each one
  // embeds a whole node per edge, so recording them for every sampled
  // node would bury the fixture in its own output.
  const queried = [];
  for (const sampled of nodes.slice(0, QUERY_SAMPLE_SIZE)) {
    const nodeIndex = sampled.ordinal * nodeFieldCount;
    const edges = snapshot.createEdgesProvider(nodeIndex, {});
    const retainers = snapshot.createRetainingEdgesProvider(nodeIndex);
    queried.push({
      ordinal: sampled.ordinal,
      objectInfo: await snapshot.getObjectInfo(nodeIndex),
      dominators: await snapshot.getDominatorsOf(nodeIndex),
      edges: (await edges.serializeItemsRange(0, Infinity)).items,
      retainers: (await retainers.serializeItemsRange(0, Infinity)).items,
    });
  }

  return { statistics, staticData, nodes, queried, duplicateStrings: await snapshot.getDuplicateStrings() };
}

// ── Recording ───────────────────────────────────────────────────────────

async function record() {
  const out = {};
  for (const name of ALL) {
    const { snapshot, worker, nodeFieldCount } = await load(name);
    try {
      out[name] = { nodeFieldCount, ...(await analyse(snapshot, nodeFieldCount)) };
    } finally {
      worker.dispose();
    }
  }
  return `${JSON.stringify(out, null, 1)}\n`;
}

const RECORDING = join(FIXTURES, 'engine.json');

if (process.argv.includes('--capture')) {
  for (const name of PAGES) {
    await capture(name);
  }
  console.log('\nre-record next: node record-heap.mjs --update');
  process.exit(0);
}

const fresh = await record();
const where = relative(ROOT, RECORDING);
if (process.argv.includes('--update')) {
  writeFileSync(RECORDING, fresh);
  console.log(`recorded ${where}`);
} else {
  let current = null;
  try {
    current = readFileSync(RECORDING, 'utf8');
  } catch {}
  if (current !== fresh) {
    console.error(`${where}: out of date with the DevTools heap engine\n\nrun: just heap-diff --update`);
    process.exit(1);
  }
  console.log(`${ALL.length} snapshot(s) still match the DevTools heap engine`);
}
process.exit(0);
