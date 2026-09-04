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
//
// Snapshots come in PAIRS. A diff merges two snapshots on object id,
// and an id means the same object only within one page session, so both
// halves of a pair are taken from one browser run with the page told to
// allocate and release in between.

import { createReadStream, readFileSync, writeFileSync } from 'node:fs';
import { gunzipSync, gzipSync } from 'node:zlib';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';
import { tmpdir } from 'node:os';

import { DevTools } from 'chrome-devtools-mcp/build/src/third_party/index.js';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '../..');
const FIXTURES = join(ROOT, 'crates/ferridriver-heap/tests/fixtures');

/**
 * Fixture pages. Each is loaded once and captured twice, either side of
 * the page's own `__grow()`, to `<name>.heapsnapshot.gz` and
 * `<name>-grown.heapsnapshot.gz`.
 */
const PAGES = ['leaky'];

/**
 * Snapshots built by `make-heapsnapshot.mjs` rather than captured, and
 * stored plain because they are small enough to read.
 *
 * A browser will not produce a snapshot with user roots in it, so the
 * three passes that need them are only reachable from a hand-built one.
 */
const HANDMADE = ['handmade', 'handmade-grown'];

/** Every snapshot, in the order the recording lists them. */
const ALL = [...PAGES.flatMap(name => [name, `${name}-grown`]), ...HANDMADE];

/** Base and current, for the snapshot diff. */
const PAIRS = [...PAGES.map(name => [name, `${name}-grown`]), ['handmade', 'handmade-grown']];

/** How many nodes the per-node comparisons cover. */
const SAMPLE_SIZE = 200;

/** How many of those the edge and retainer comparisons cover. */
const QUERY_SAMPLE_SIZE = 25;

/**
 * How many of those the retaining-path comparison covers, each under
 * every limit below.
 *
 * Only nodes a path can actually reach a root from: a node at distance
 * 2 or less IS a root, and one only the system reaches sits past
 * `baseSystemDistance`, and both answer with the empty forest. A sample
 * of those would compare nothing at all.
 */
const PATH_SAMPLE_SIZE = 5;
const BASE_SYSTEM_DISTANCE = 100000000;

/**
 * How many classes the class-node provider is compared over, and how
 * many members of each are recorded.
 *
 * A class of a captured page can hold thousands of nodes, and every one
 * of them serialises as a whole node record. The provider's own
 * `totalLength` is recorded alongside, so a comparison that stopped
 * short would still be caught.
 */
const CLASS_SAMPLE_SIZE = 8;
const ITEM_SAMPLE_SIZE = 20;

/**
 * The queries, chosen so each filter decides something on its own.
 *
 * A query nothing matches compares two empty lists, which is the same
 * vacuous agreement the accessibility comparison spent a week in, so
 * `the_fixtures_still_carry_something_worth_comparing` asserts a floor
 * on how many of these actually match.
 */
const QUERIES = [
  {},
  { sortBy: 'id' },
  { sortBy: 'selfSize' },
  { className: 'array' },
  { nodeType: 'string' },
  { isDetached: true },
  { minSelfSize: 64 },
  { minRetainedSize: 1, maxRetainedSize: 4096 },
  { propertyName: '^(p|q|elements)$' },
];

/**
 * The named node filters `get_heapsnapshot_details` and
 * `get_heapsnapshot_class_nodes` expose, minus the one that names a
 * specific native context: that takes an object id, which differs
 * between fixtures, so it is picked per snapshot below.
 */
const NAMED_FILTERS = [
  'objectsRetainedByContexts',
  'objectsRetainedByDetachedDomNodes',
  'objectsRetainedByConsole',
  'objectsRetainedByEventHandlers',
  'sharedNativeContext',
  'noNativeContext',
];

/**
 * The retaining-path limits. The first is what the tool sends; the rest
 * are tight enough that each of the three bounds bites on its own, so
 * `limitsReached` is something other than empty in the recording.
 *
 * A depth of nought is here because nothing else reaches the depth
 * check: the search already drops any retainer too far from a root to
 * fit in what is left, so the only way to enter `buildForest` with no
 * depth remaining is to start with none.
 */
const PATH_LIMITS = [
  { maxDepth: 30, maxNodes: 5000, maxSiblings: 100 },
  { maxDepth: 4, maxNodes: 5000, maxSiblings: 100 },
  { maxDepth: 30, maxNodes: 6, maxSiblings: 100 },
  { maxDepth: 30, maxNodes: 5000, maxSiblings: 2 },
  { maxDepth: 0, maxNodes: 5000, maxSiblings: 100 },
];

/**
 * And the limits a node that IS a root is recorded under.
 *
 * Asking for the paths of a root is answered with the empty forest, and
 * so is asking without the check that says so -- the walk climbs to the
 * synthetic root and finds nothing to report. The two only tell
 * themselves apart against a budget of one node.
 */
const ROOT_LIMITS = [
  { maxDepth: 30, maxNodes: 1, maxSiblings: 100 },
  { maxDepth: 30, maxNodes: 5000, maxSiblings: 100 },
  // No depth at all, asked of a node with no retainers to reject: the
  // only shape that reaches the depth budget on its own. Everywhere
  // else, a retainer too far from a root reports the same limit first.
  { maxDepth: 0, maxNodes: 5000, maxSiblings: 100 },
];

// ── Capture ─────────────────────────────────────────────────────────────

async function capture(name) {
  const puppeteer = (await import('puppeteer-core')).default;
  const executablePath = process.env.CHROME_PATH ?? findChrome();
  const browser = await puppeteer.launch({ executablePath, headless: true, args: ['--no-sandbox'] });
  try {
    const page = await browser.newPage();
    await page.goto(`file://${join(FIXTURES, `${name}.html`)}`, { waitUntil: 'networkidle0' });
    // Both halves come from this one page, because a diff merges on
    // object id and V8 assigns those per isolate: two separate runs of
    // the same page share no ids at all, and every object would read as
    // allocated and collected at once.
    await write(page, name);
    await page.evaluate(() => globalThis.__grow());
    await write(page, `${name}-grown`);
  } finally {
    await browser.close();
  }
}

async function write(page, name) {
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
}

function findChrome() {
  throw new Error('set CHROME_PATH, or run: ferridriver install chromium');
}

// ── The engine ──────────────────────────────────────────────────────────

/** Load a fixture into the real worker and hand back its snapshot proxy. */
async function load(name, uid) {
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
  const loader = worker.createLoader(uid, snapshot => resolve(snapshot));
  for await (const chunk of createReadStream(plain, { encoding: 'utf-8', highWaterMark: 1024 * 1024 })) {
    await loader.write(chunk);
  }
  await loader.close();
  return { snapshot: await promise, worker, nodeFieldCount };
}

/** The head of a provider's answer, plus how long the whole answer is. */
async function head(provider) {
  const range = await provider.serializeItemsRange(0, Infinity);
  return { total: range.totalLength, items: range.items.slice(0, ITEM_SAMPLE_SIZE) };
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

  const reachable = nodes.filter(node => node.distance > 2 && node.distance < BASE_SYSTEM_DISTANCE);
  const roots = nodes.filter(node => node.distance >= 0 && node.distance <= 2);
  // Every eligible node where there are few enough to afford it: a
  // hand-built snapshot holds one node per branch, and a stride over
  // twenty of them would walk past most of what it was built for.
  const pathStride = reachable.length <= 48 ? 1 : Math.max(1, Math.floor(reachable.length / PATH_SAMPLE_SIZE));
  const mostPaths = pathStride === 1 ? reachable.length : PATH_SAMPLE_SIZE;
  const sampledPaths = [];
  for (let at = 0; at < reachable.length && sampledPaths.length < mostPaths; at += pathStride) {
    sampledPaths.push([reachable[at], PATH_LIMITS]);
  }
  for (const root of roots.slice(0, 2)) {
    sampledPaths.push([root, ROOT_LIMITS]);
  }
  const retainingPaths = [];
  for (const [sampled, limitSets] of sampledPaths) {
    for (const limits of limitSets) {
      retainingPaths.push({
        ordinal: sampled.ordinal,
        distance: sampled.distance,
        limits,
        result: await snapshot.getRetainingPaths(
          sampled.ordinal * nodeFieldCount,
          limits.maxDepth,
          limits.maxNodes,
          limits.maxSiblings,
        ),
      });
    }
  }

  const filter = new DevTools.HeapSnapshotModel.HeapSnapshotModel.NodeFilter();
  const aggregates = await snapshot.aggregatesWithFilter(filter);

  // The class-node provider, over a spread of the class keys taken by
  // size: alphabetical order buries the classes worth reading, and a
  // page's biggest class holds thousands of nodes where its smallest
  // holds one. Both orderings have to come out right.
  const classKeys = Object.keys(aggregates).sort(
    (a, b) => aggregates[b].count - aggregates[a].count || (a < b ? -1 : 1),
  );
  const classStride = Math.max(1, Math.floor(classKeys.length / CLASS_SAMPLE_SIZE));
  const sampledClasses = [];
  for (let at = 0; at < classKeys.length && sampledClasses.length < CLASS_SAMPLE_SIZE; at += classStride) {
    sampledClasses.push(classKeys[at]);
  }
  // A class key carries its constructor's script, line and column where
  // there is one, so that two constructors of the same name from
  // different scripts stay apart. There is usually exactly one such
  // class in a page and a spread by size will not land on it.
  const located = classKeys.find(key => !key.startsWith(','));
  if (located !== undefined && !sampledClasses.includes(located)) {
    sampledClasses.push(located);
  }
  const classNodes = [];
  for (const classKey of sampledClasses) {
    classNodes.push({
      classKey,
      ...(await head(snapshot.createNodesProviderForClass(classKey, filter))),
    });
  }

  const queries = [];
  for (const query of QUERIES) {
    queries.push({ query, ...(await head(snapshot.queryObjects(query))) });
  }

  // Each named filter, and one more naming a native context this
  // snapshot actually holds -- an id, so it has to be read off the
  // snapshot rather than written down.
  const nativeContextSizes = await snapshot.getNativeContextSizes();
  const filterNames = [...NAMED_FILTERS];
  const firstContext = nativeContextSizes.nativeContexts[0];
  if (firstContext) {
    filterNames.push(`nativeContext_${firstContext.nodeIndex}`);
  }
  const namedFilters = [];
  for (const filterName of filterNames) {
    const filter = new DevTools.HeapSnapshotModel.HeapSnapshotModel.NodeFilter();
    filter.filterName = filterName;
    const filtered = await snapshot.aggregatesWithFilter(filter);
    const keys = Object.keys(filtered).sort();
    namedFilters.push({
      filterName,
      // The whole class map: a filter that quietly kept everything and
      // one that kept the right thing agree on any single class.
      aggregates: filtered,
      // Plus the provider over one of them, which is the other half of
      // what the filter is for.
      classNodes: keys.length
        ? { classKey: keys[0], ...(await head(snapshot.createNodesProviderForClass(keys[0], filter))) }
        : null,
    });
  }

  return {
    statistics,
    staticData,
    nodes,
    queried,
    retainingPaths,
    classNodes,
    queries,
    namedFilters,
    nativeContextSizes,
    retainedByContextSummary: await snapshot.getRetainedByContextSummary(),
    duplicateStrings: await snapshot.getDuplicateStrings(),
    aggregates,
  };
}

/**
 * What changed between two snapshots of one heap.
 *
 * The base is re-classified under the CURRENT snapshot's interface
 * definitions first, because each snapshot names its plain objects
 * after the shapes it happens to hold.
 */
async function diff(base, current) {
  const definitions = await current.interfaceDefinitions();
  const aggregatesForDiff = await base.aggregatesForDiff(definitions);
  const diffs = await current.calculateSnapshotDiff(base.uid, aggregatesForDiff);
  // The six per-object lists are recorded head-first, the way a
  // provider's answer is: one class of a real page can gain thousands
  // of objects, and every id, index and size of them would be most of
  // this file. What is NOT truncated is the counts and the sizes those
  // lists add up to, so a list that went wrong past the head still has
  // to show up in a total.
  const out = {};
  for (const [classKey, value] of Object.entries(diffs)) {
    out[classKey] = { ...value };
    for (const field of [
      'addedIndexes',
      'addedIds',
      'addedSelfSizes',
      'deletedIndexes',
      'deletedIds',
      'deletedSelfSizes',
    ]) {
      out[classKey][field] = value[field].slice(0, ITEM_SAMPLE_SIZE);
    }
  }
  return out;
}

// ── Recording ───────────────────────────────────────────────────────────

async function record() {
  const loaded = new Map();
  const out = {};
  try {
    for (const [at, name] of ALL.entries()) {
      loaded.set(name, await load(name, at + 1));
    }
    for (const name of ALL) {
      const { snapshot, nodeFieldCount } = loaded.get(name);
      out[name] = { nodeFieldCount, ...(await analyse(snapshot, nodeFieldCount)) };
    }
    for (const [base, current] of PAIRS) {
      out[current].diffFrom = base;
      out[current].diff = await diff(loaded.get(base).snapshot, loaded.get(current).snapshot);
    }
  } finally {
    for (const { worker } of loaded.values()) {
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
