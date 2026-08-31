// Build the `.heapsnapshot` a browser will not produce.
//
//   node make-heapsnapshot.mjs
//
// A snapshot taken over CDP has no user roots -- the synthetic root's
// only child is `(GC roots)`, and everything under that is synthetic
// too. Upstream reads that as "internals were exposed" and skips
// `calculateShallowSizes` entirely, so three passes never run on either
// side: the shallow-size transfer, the page-object marking that feeds
// the essential-edge filter, and the first half of the distance walk.
// A comparison over a captured snapshot therefore agrees about branches
// neither implementation takes.
//
// So this one is built by hand, the way upstream's own
// `HeapSnapshot.test.ts` builds them, small enough to read and to
// reason about. It is still a differential: the real engine analyses
// this file too, and the recording is whatever IT concluded.
//
// Each shape below exists to make one branch discriminate. Removing the
// code that handles it has to change an answer, or the fixture is not
// evidence.

import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '../..');
const OUT = join(ROOT, 'crates/ferridriver-heap/tests/fixtures/handmade.heapsnapshot');

// The field layout and enum tables Chrome writes today. Kept verbatim
// so a reader that resolves offsets by name is exercised the same way.
const NODE_TYPES = [
  'hidden',
  'array',
  'string',
  'object',
  'code',
  'closure',
  'regexp',
  'number',
  'native',
  'synthetic',
  'concatenated string',
  'sliced string',
  'symbol',
  'bigint',
  'object shape',
];
const EDGE_TYPES = ['context', 'element', 'property', 'internal', 'hidden', 'shortcut', 'weak'];

const strings = [];
function str(value) {
  const at = strings.indexOf(value);
  return at === -1 ? strings.push(value) - 1 : at;
}

const nodes = [];
/** Declare a node; edges are attached afterwards, in node order. */
function node({ type, name, id, selfSize = 0, detachedness = 0 }) {
  nodes.push({ type: NODE_TYPES.indexOf(type), name, id, selfSize, detachedness, edges: [] });
  return nodes.length - 1;
}
function edge(from, { type, name, to }) {
  nodes[from].edges.push({ type: EDGE_TYPES.indexOf(type), name, to });
}

// ── The graph ───────────────────────────────────────────────────────────

// Node 0 must be the root: `buildDominatedNodes` requires it first or
// last, and every index here assumes first.
const root = node({ type: 'synthetic', name: '', id: 1 });
const gcRoots = node({ type: 'synthetic', name: '(GC roots)', id: 3 });
// A non-synthetic child of the root is a user root, which is the whole
// point: it turns on the shallow-size pass and the first distance walk.
const window = node({ type: 'object', name: 'Window', id: 5, selfSize: 64 });
// The one synthetic that still counts as a user root, and the `element`
// half of the page-object entry points.
const domTrees = node({ type: 'synthetic', name: '(Document DOM trees)', id: 7 });

// An array whose backing store nothing else points at: its size moves
// onto the array, and the JS-array measurement counts it.
const ownedArray = node({ type: 'object', name: 'Array', id: 9, selfSize: 32 });
const ownedElements = node({ type: 'array', name: '(object elements)', id: 11, selfSize: 512 });

// An array whose backing store has a second retainer: neither the
// transfer nor the measurement may claim it.
const sharedArray = node({ type: 'object', name: 'Array', id: 13, selfSize: 32 });
const sharedElements = node({ type: 'array', name: '(object elements)', id: 15, selfSize: 1024 });
const otherHolder = node({ type: 'object', name: 'Holder', id: 17, selfSize: 16 });

// A hidden node with a size, which statistics counts as system and
// stops looking at.
const hidden = node({ type: 'hidden', name: 'system / hidden', id: 19, selfSize: 128 });
// A hidden node named `Array`, which is the only shape that tells the
// early exit apart from its absence: without it the JS-array branch
// would claim this too, and the same bytes would be counted twice.
const hiddenArray = node({ type: 'hidden', name: 'Array', id: 39, selfSize: 64 });

// A detached native, whose state propagates to its child and renames
// both.
const detached = node({ type: 'native', name: 'HTMLDivElement', id: 21, selfSize: 48, detachedness: 2 });
const detachedChild = node({ type: 'native', name: 'HTMLParagraphElement', id: 23, selfSize: 24 });

// A WeakMap and its key, both pointing at one value. Only the key's
// edge is essential, so the value must come out dominated by the key.
const weakMap = node({ type: 'object', name: 'WeakMap', id: 25, selfSize: 40 });
const weakKey = node({ type: 'object', name: 'Key', id: 27, selfSize: 24 });
const weakValue = node({ type: 'object', name: 'Value', id: 29, selfSize: 256 });

// Reached only by a weak edge, so nothing retains it and the dominator
// pass has to pick it up in its second walk.
const weaklyHeld = node({ type: 'object', name: 'WeaklyHeld', id: 31, selfSize: 96 });

const code = node({ type: 'code', name: 'compiled', id: 33, selfSize: 200 });
const text = node({ type: 'string', name: 'a string', id: 35, selfSize: 80 });
const buffer = node({ type: 'native', name: 'system / JSArrayBufferData', id: 37, selfSize: 2048 });

const ephemeron = `1 / part of key (Key @${27}) -> value (Value @${29}) pair in WeakMap (table @${25})`;

edge(root, { type: 'element', name: 1, to: gcRoots });
edge(root, { type: 'shortcut', name: str('global'), to: window });
edge(root, { type: 'element', name: 2, to: domTrees });

edge(gcRoots, { type: 'element', name: 1, to: code });
edge(gcRoots, { type: 'element', name: 2, to: hidden });
edge(gcRoots, { type: 'element', name: 3, to: hiddenArray });

edge(window, { type: 'property', name: str('owned'), to: ownedArray });
edge(window, { type: 'property', name: str('shared'), to: sharedArray });
edge(window, { type: 'property', name: str('holder'), to: otherHolder });
edge(window, { type: 'property', name: str('map'), to: weakMap });
edge(window, { type: 'property', name: str('key'), to: weakKey });
edge(window, { type: 'property', name: str('text'), to: text });
edge(window, { type: 'property', name: str('buffer'), to: buffer });
// Weak edges retain nothing, so `weaklyHeld` is unreachable for the
// dominator walk and reachable for the distance walk to skip.
edge(window, { type: 'weak', name: str('weaklyHeld'), to: weaklyHeld });

edge(domTrees, { type: 'element', name: 1, to: detached });

edge(ownedArray, { type: 'internal', name: str('elements'), to: ownedElements });
edge(sharedArray, { type: 'internal', name: str('elements'), to: sharedElements });
edge(otherHolder, { type: 'property', name: str('alsoElements'), to: sharedElements });

edge(detached, { type: 'element', name: 1, to: detachedChild });

// The pair the dominator pass has to break: the table's edge is
// dropped so the value is dominated by the key.
edge(weakMap, { type: 'internal', name: str(ephemeron), to: weakValue });
edge(weakKey, { type: 'internal', name: str(ephemeron), to: weakValue });

// ── Serialise ───────────────────────────────────────────────────────────

const NODE_FIELDS = ['type', 'name', 'id', 'self_size', 'edge_count', 'detachedness'];
const EDGE_FIELDS = ['type', 'name_or_index', 'to_node'];

const flatNodes = [];
const flatEdges = [];
for (const entry of nodes) {
  flatNodes.push(entry.type, str(entry.name), entry.id, entry.selfSize, entry.edges.length, entry.detachedness);
}
for (const entry of nodes) {
  for (const { type, name, to } of entry.edges) {
    flatEdges.push(type, name, to * NODE_FIELDS.length);
  }
}

const profile = {
  snapshot: {
    meta: {
      node_fields: NODE_FIELDS,
      node_types: [NODE_TYPES, 'string', 'number', 'number', 'number', 'number'],
      edge_fields: EDGE_FIELDS,
      edge_types: [EDGE_TYPES, 'string_or_number', 'node'],
      trace_function_info_fields: [],
      trace_node_fields: [],
      sample_fields: [],
      location_fields: [],
    },
    node_count: nodes.length,
    edge_count: flatEdges.length / EDGE_FIELDS.length,
    trace_function_count: 0,
    extra_native_bytes: 4096,
  },
  nodes: flatNodes,
  edges: flatEdges,
  trace_function_infos: [],
  trace_tree: [],
  samples: [],
  locations: [],
  strings,
};

writeFileSync(OUT, `${JSON.stringify(profile)}\n`);
console.log(`wrote ${relative(ROOT, OUT)} (${nodes.length} nodes, ${profile.snapshot.edge_count} edges)`);
