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
//
// It is built twice. A snapshot diff needs two snapshots of one heap,
// and the pair has to differ in the ways the merge takes branches on:
// an object that survived, one that was collected, one that was
// allocated in between, a class only the older one has, a class only
// the newer one has, and a plain-object shape the two snapshots name
// differently from each other.

import { writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, relative } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, '../..');
const FIXTURES = join(ROOT, 'crates/ferridriver-heap/tests/fixtures');

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

/**
 * @param {boolean} grown Build the later snapshot of the pair.
 */
function build(grown) {
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

// A concatenated string, which V8 stores as a tree of pieces with no
// name of its own: the name has to be assembled by walking `first` and
// `second`, in that order.
const consString = node({ type: 'concatenated string', name: '', id: 41, selfSize: 40 });
const consLeft = node({ type: 'string', name: 'hello ', id: 43, selfSize: 24 });
const consRight = node({ type: 'concatenated string', name: '', id: 45, selfSize: 40 });
const consRightLeft = node({ type: 'string', name: 'brave ', id: 47, selfSize: 24 });
const consRightRight = node({ type: 'string', name: 'world', id: 49, selfSize: 24 });

// Two concatenated strings that assemble to the same text out of
// different pieces, and a third that V8 has already flattened -- its
// `first` half is the empty string. The flat one reads the same and
// must NOT be grouped with them: reporting a string as a duplicate of
// its own content tells nobody anything.
const consDupA = node({ type: 'concatenated string', name: '', id: 57, selfSize: 40 });
const consDupALeft = node({ type: 'string', name: 'dup ', id: 59, selfSize: 16 });
const consDupARight = node({ type: 'string', name: 'text', id: 61, selfSize: 16 });
const consDupB = node({ type: 'concatenated string', name: '', id: 63, selfSize: 40 });
const consDupBLeft = node({ type: 'string', name: 'du', id: 65, selfSize: 16 });
const consDupBRight = node({ type: 'string', name: 'p text', id: 67, selfSize: 16 });
const consFlat = node({ type: 'concatenated string', name: '', id: 69, selfSize: 40 });
const emptyPiece = node({ type: 'string', name: '', id: 71, selfSize: 16 });
const flatContent = node({ type: 'string', name: 'dup text', id: 73, selfSize: 32 });
// A string node of zero size is how V8 encodes a number, not a string,
// so it is skipped however its text reads.
const numberish = node({ type: 'string', name: 'dup text', id: 75, selfSize: 0 });

// A plain `Object`, which is named for every other plain object too, so
// it gets named after its properties instead. `__proto__` is skipped
// and a name carrying punctuation is quoted.
const plainObject = node({ type: 'object', name: 'Object', id: 51, selfSize: 56 });
// A plain object with more properties than the label can hold, so the
// budget truncates and the ellipsis appears.
const wideObject = node({ type: 'object', name: 'Object', id: 53, selfSize: 56 });
const propertyTarget = node({ type: 'object', name: 'Leaf', id: 55, selfSize: 8 });

const code = node({ type: 'code', name: 'compiled', id: 33, selfSize: 200 });
const text = node({ type: 'string', name: 'a string', id: 35, selfSize: 80 });
const buffer = node({ type: 'native', name: 'system / JSArrayBufferData', id: 37, selfSize: 2048 });

// A class whose members straddle the pair: 101 and 105 survive, 103 is
// collected and 107 is allocated after the first snapshot. Merging two
// id lists takes a different branch for each of those three, and a
// class where every member survives takes only the fourth.
//
// Declared out of id order deliberately. A diff merges two id lists by
// walking each once, which is a merge only if both ascend, and a class
// whose members happen to sit in the heap in id order would agree with
// or without the sort that puts them there.
const widgets = (grown ? [107, 101, 105] : [105, 101, 103]).map(id =>
  node({ type: 'object', name: 'Widget', id, selfSize: 48 }),
);
// One class in the older snapshot only, and one in the newer only:
// each is reached by a different half of the diff, and a pair sharing
// every class would exercise neither.
const onlyBefore = grown ? null : node({ type: 'object', name: 'Collected', id: 111, selfSize: 72 });
const onlyAfter = grown ? node({ type: 'object', name: 'Allocated', id: 113, selfSize: 88 }) : null;

// The shape names the two snapshots disagree about. An interface has
// to be shared by at least two objects, so `{p, q}` is a named shape in
// the older snapshot and `Object` in the newer, and `{r, s}` the other
// way round. Comparing each side under its OWN names would report both
// classes wholly replaced; the diff classifies the older snapshot under
// the newer one's names, and only then do these line up.
const pairwise = [];
for (const [property, ids] of [
  ['p', grown ? [121] : [121, 123]],
  ['r', grown ? [131, 133] : [131]],
]) {
  for (const id of ids) {
    pairwise.push({ property, at: node({ type: 'object', name: 'Object', id, selfSize: 40 }) });
  }
}

// ── The shapes the named node filters exist to find ────────────────────
//
// Each filter walks the graph AVOIDING something and keeps what the
// walk could not reach, so every one of them needs an object that is
// reachable ONLY through the thing it avoids. An object reachable two
// ways proves nothing: the walk finds it anyway.
//
// Added identically to both snapshots of the pair, so they contribute
// nothing to the diff.

// Two realms, because one cannot tell "owned" from "shared".
const realmA = node({ type: 'hidden', name: 'system / NativeContext', id: 141, selfSize: 128 });
const realmB = node({ type: 'hidden', name: 'system / NativeContext / two', id: 143, selfSize: 128 });
// Object -> Map -> meta-Map -> NativeContext. Nothing on the object
// says which realm it belongs to; only the third hop does, and a pass
// that read the object's own edges would find nothing at all.
const metaMap = node({ type: 'hidden', name: 'system / Map', id: 145, selfSize: 24 });
const shapeMap = node({ type: 'hidden', name: 'system / Map', id: 147, selfSize: 24 });
const ownedByRealm = node({ type: 'object', name: 'RealmOwned', id: 149, selfSize: 64 });
// Reached from both realms, so neither owns it.
const sharedByRealms = node({ type: 'object', name: 'SharedAcrossRealms', id: 151, selfSize: 56 });

// A closure scope and the one object behind it.
const scope = node({ type: 'hidden', name: 'system / Context', id: 153, selfSize: 32 });
const behindScope = node({ type: 'object', name: 'CapturedOnly', id: 155, selfSize: 88 });

// A listener whose callback IS the handler, and what only it holds.
const listener = node({ type: 'object', name: 'V8EventListener', id: 157, selfSize: 32 });
const handler = node({ type: 'closure', name: 'onClick', id: 159, selfSize: 40 });
const handlerCode = node({ type: 'code', name: 'onClick code', id: 161, selfSize: 48 });
const behindHandler = node({ type: 'object', name: 'HandlerOnly', id: 163, selfSize: 72 });
// And one whose callback is a framework wrapper, so the handler is a
// level down: without that fallback this listener marks nothing.
const wrappedListener = node({ type: 'object', name: 'V8EventListener', id: 165, selfSize: 32 });
const wrapper = node({ type: 'object', name: 'Wrapper', id: 167, selfSize: 24 });
const wrapped = node({ type: 'closure', name: 'wrapped', id: 169, selfSize: 40 });
const wrappedCode = node({ type: 'code', name: 'wrapped code', id: 171, selfSize: 48 });

// A global the DevTools console pinned. The edge NAME is what records
// that, and it is a string-named edge from a synthetic node.
const consolePinned = node({ type: 'object', name: 'ConsolePinned', id: 173, selfSize: 96 });
// And the same edge name from an ordinary object, which is NOT the
// console and must still be followed. Without the synthetic check this
// one reads as console-pinned too.
const notConsolePinned = node({ type: 'object', name: 'NotConsolePinned', id: 175, selfSize: 96 });

// A realm whose frame has gone. It still owns what it allocated, and
// that is the realm a leak hunt is looking for -- but only if the
// detached spelling counts as a native context at all.
const detachedRealm = node({ type: 'hidden', name: 'Detached system / NativeContext', id: 177, selfSize: 128 });
const ownedByDetachedRealm = node({ type: 'object', name: 'DetachedRealmOwned', id: 179, selfSize: 48 });

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
edge(window, { type: 'property', name: str('cons'), to: consString });
edge(window, { type: 'property', name: str('plain'), to: plainObject });
edge(window, { type: 'property', name: str('wide'), to: wideObject });
edge(window, { type: 'property', name: str('dupA'), to: consDupA });
edge(window, { type: 'property', name: str('dupB'), to: consDupB });
edge(window, { type: 'property', name: str('dupFlat'), to: consFlat });
edge(window, { type: 'property', name: str('numberish'), to: numberish });
// Weak edges retain nothing, so `weaklyHeld` is unreachable for the
// dominator walk and reachable for the distance walk to skip.
edge(window, { type: 'weak', name: str('weaklyHeld'), to: weaklyHeld });

edge(domTrees, { type: 'element', name: 1, to: detached });

// An unreachable node that points at a reachable one, so `Leaf` has a
// retainer no path can start from. Dropping it changes no answer on its
// own -- the recursion would find nothing and give up -- but it is one
// more sibling and one more node against the search's budgets, which is
// where the difference shows.
edge(weaklyHeld, { type: 'property', name: str('leaf'), to: propertyTarget });

edge(ownedArray, { type: 'internal', name: str('elements'), to: ownedElements });
edge(sharedArray, { type: 'internal', name: str('elements'), to: sharedElements });
edge(otherHolder, { type: 'property', name: str('alsoElements'), to: sharedElements });

edge(detached, { type: 'element', name: 1, to: detachedChild });

edge(consString, { type: 'internal', name: str('first'), to: consLeft });
edge(consString, { type: 'internal', name: str('second'), to: consRight });
edge(consRight, { type: 'internal', name: str('first'), to: consRightLeft });
edge(consRight, { type: 'internal', name: str('second'), to: consRightRight });

edge(consDupA, { type: 'internal', name: str('first'), to: consDupALeft });
edge(consDupA, { type: 'internal', name: str('second'), to: consDupARight });
edge(consDupB, { type: 'internal', name: str('first'), to: consDupBLeft });
edge(consDupB, { type: 'internal', name: str('second'), to: consDupBRight });
edge(consFlat, { type: 'internal', name: str('first'), to: emptyPiece });
edge(consFlat, { type: 'internal', name: str('second'), to: flatContent });

// Taken alternately from each end, so the order of these decides the
// label. `__proto__` is skipped wherever it falls, and the quoted one
// proves the escaping.
edge(plainObject, { type: 'property', name: str('alpha'), to: propertyTarget });
edge(plainObject, { type: 'property', name: str('__proto__'), to: propertyTarget });
edge(plainObject, { type: 'property', name: str('be, ta'), to: propertyTarget });
edge(plainObject, { type: 'internal', name: str('map'), to: propertyTarget });
edge(plainObject, { type: 'property', name: str('gamma'), to: propertyTarget });

for (let i = 0; i < 12; i++) {
  edge(wideObject, { type: 'property', name: str(`aPropertyNameLongEnoughToCount${i}`), to: propertyTarget });
}

for (const [at, ordinal] of widgets.entries()) {
  edge(window, { type: 'property', name: str(`widget${at}`), to: ordinal });
}
for (const [property, ordinal] of [
  ['collected', onlyBefore],
  ['allocated', onlyAfter],
]) {
  if (ordinal !== null) {
    edge(window, { type: 'property', name: str(property), to: ordinal });
  }
}
for (const [at, { property, at: ordinal }] of pairwise.entries()) {
  edge(window, { type: 'property', name: str(`shaped${at}`), to: ordinal });
  // Two properties apiece, which is what makes them a shape at all.
  edge(ordinal, { type: 'property', name: str(property), to: propertyTarget });
  edge(ordinal, { type: 'property', name: str(property === 'p' ? 'q' : 's'), to: propertyTarget });
}

edge(gcRoots, { type: 'element', name: 4, to: realmA });
edge(gcRoots, { type: 'element', name: 5, to: realmB });
edge(gcRoots, { type: 'shortcut', name: str('inspected / DevTools console'), to: consolePinned });
edge(gcRoots, { type: 'element', name: 6, to: detachedRealm });
edge(otherHolder, { type: 'property', name: str('decoy / DevTools console'), to: notConsolePinned });
edge(detachedRealm, { type: 'property', name: str('owned'), to: ownedByDetachedRealm });

edge(ownedByRealm, { type: 'internal', name: str('map'), to: shapeMap });
edge(shapeMap, { type: 'internal', name: str('map'), to: metaMap });
edge(metaMap, { type: 'internal', name: str('native_context'), to: realmA });
edge(window, { type: 'property', name: str('realmOwned'), to: ownedByRealm });
edge(realmA, { type: 'property', name: str('shared'), to: sharedByRealms });
edge(realmB, { type: 'property', name: str('shared'), to: sharedByRealms });

// Two retainers on the scope, so the shallow-size pass leaves its own
// size where it is: a context whose bytes moved to its owner is not
// counted as a context at all, and the count would be nought.
edge(window, { type: 'property', name: str('scope'), to: scope });
edge(otherHolder, { type: 'property', name: str('alsoScope'), to: scope });
edge(scope, { type: 'context', name: str('captured'), to: behindScope });

edge(window, { type: 'property', name: str('listener'), to: listener });
edge(listener, { type: 'element', name: 1, to: handler });
edge(handler, { type: 'internal', name: str('code'), to: handlerCode });
edge(handler, { type: 'property', name: str('kept'), to: behindHandler });

edge(window, { type: 'property', name: str('wrappedListener'), to: wrappedListener });
edge(wrappedListener, { type: 'element', name: 1, to: wrapper });
edge(wrapper, { type: 'property', name: str('inner'), to: wrapped });
edge(wrapped, { type: 'internal', name: str('code'), to: wrappedCode });

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

return {
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
}

for (const [name, grown] of [['handmade', false], ['handmade-grown', true]]) {
  const profile = build(grown);
  const out = join(FIXTURES, `${name}.heapsnapshot`);
  writeFileSync(out, `${JSON.stringify(profile)}\n`);
  console.log(
    `wrote ${relative(ROOT, out)} (${profile.snapshot.node_count} nodes, ${profile.snapshot.edge_count} edges)`,
  );
}
