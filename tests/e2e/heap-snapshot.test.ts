// `page.takeHeapSnapshot()` — a V8 heap snapshot, analysed.
//
// Chromium captures and answers; Firefox and WebKit reject with the
// typed Unsupported, because the format is V8's and neither engine
// writes anything a reader of it could open.
//
// What the analysis AGREES with is checked elsewhere and far harder:
// `just heap-diff` runs DevTools' own heap engine over two captured and
// two hand-built snapshots and compares node by node, and
// `cargo test -p ferridriver-heap --test differential` replays that
// offline. What is here is the other half — that the capture reaches
// the browser, that every query survives the binding layer, and that
// each one answers about the page in front of it rather than about
// nothing.

import { test, describe, expect } from '@ferridriver/test';

// A page that holds on to shapes each query is meant to find: a named
// class with a known population, a string repeated far past interning,
// and a detached DOM subtree. `grow()` then allocates more of the class
// and drops half the strings, which is what a diff has to see.
const RETAINING = `<html><body><main>heap</main><script>
  class Widget {
    constructor(n) { this.n = n; this.label = 'widget ' + n; }
  }
  // Assembled rather than repeated: V8 hands back the SAME string for
  // a full slice of one, and a literal is interned, so either would be
  // one object rather than three hundred copies of one.
  const duplicated = [];
  for (let i = 0; i < 300; i++) {
    duplicated.push('a repeated string long enough not to be interned away ' + (i % 1));
  }
  const detached = document.createElement('div');
  for (let i = 0; i < 120; i++) {
    const row = document.createElement('p');
    row.textContent = 'row ' + i;
    detached.append(row);
  }
  globalThis.__kept = {
    widgets: Array.from({ length: 200 }, (_, n) => new Widget(n)),
    duplicated,
    detached,
  };
  globalThis.__grow = () => {
    for (let n = 200; n < 700; n++) globalThis.__kept.widgets.push(new Widget(n));
    globalThis.__kept.duplicated.length = 100;
  };
</script></body></html>`;

describe('page.takeHeapSnapshot', () => {
  test('captures a heap and answers every query about it', async ({ page, browserName }) => {
    await page.setContent(RETAINING);

    if (browserName !== 'chromium') {
      let message = '';
      try {
        await page.takeHeapSnapshot();
      } catch (e) {
        message = String(e);
      }
      expect(message.includes('Chromium')).toBe(true);
      return;
    }

    const heap = await page.takeHeapSnapshot();

    // The snapshot itself: a real graph, not an empty one, and a file
    // the DevTools Memory panel would open.
    expect(heap.nodeCount()).toBeGreaterThan(1000);
    expect(heap.totalSize()).toBeGreaterThan(0);
    const statistics = heap.statistics();
    expect(statistics.total).toBe(statistics.native.total + statistics.v8heap.total);
    expect(statistics.v8heap.strings).toBeGreaterThan(0);
    expect(statistics.v8heap.code).toBeGreaterThan(0);

    const written = JSON.parse(new TextDecoder().decode(heap.bytes()));
    expect(Array.isArray(written.snapshot.meta.node_fields)).toBe(true);
    expect(written.snapshot.node_count).toBe(heap.nodeCount());

    // The class the page defined, found by name rather than by key: a
    // constructor with a script location is keyed by that location, so
    // the key is not something a test can spell.
    const widgets = heap.classes().find((c) => c.name === 'Widget');
    expect(widgets !== undefined).toBe(true);
    expect(widgets!.count).toBe(200);
    expect(widgets!.selfSize).toBeGreaterThan(0);
    expect(widgets!.classKey.startsWith(',')).toBe(false);

    const members = heap.classObjects(widgets!.classKey);
    expect(members.length).toBe(200);
    expect(members.every((m) => m.name === 'Widget')).toBe(true);

    // One of them, from every angle.
    const id = members[0].id;
    const object = heap.object(id);
    expect(object.id).toBe(id);
    expect(object.name).toBe('Widget');
    expect(object.type).toBe('object');
    expect(object.selfSize).toBeGreaterThan(0);
    expect(object.retainerCount).toBeGreaterThan(0);

    // A Widget carries `n` and `label`, so both have to be among what
    // it points at.
    const edgeNames = heap.edges(id).map((e) => e.name);
    expect(edgeNames.includes('label')).toBe(true);
    expect(object.edgeCount).toBeGreaterThan(0);

    // It lives in an array, so the array is what holds it.
    const retainers = heap.retainers(id);
    expect(retainers.length).toBeGreaterThan(0);
    expect(retainers.some((r) => r.node.type === 'array' || r.node.type === 'object')).toBe(true);

    // The chain up to the root ends at the root, which retains
    // everything, and each step retains at least the one before it.
    const chain = heap.dominators(id);
    expect(chain.length).toBeGreaterThan(1);
    expect(chain[0].nodeId).toBe(id);
    expect(chain[chain.length - 1].retainedSize).toBeGreaterThanOrEqual(chain[0].retainedSize);

    const paths = heap.retainingPaths(id);
    expect(paths.paths.length).toBeGreaterThan(0);
    // A path is a tree of retainers, each nearer a root than its child.
    const first = paths.paths[0];
    expect(first.nodeName.length).toBeGreaterThan(0);
    expect(first.distance).toBeGreaterThanOrEqual(2);

    // A budget the search cannot fit in has to be REPORTED rather than
    // silently truncating the answer. This object sits well past one
    // edge from a root, so with one edge to spend there is no path and
    // the depth bound is what says so.
    const shallow = heap.retainingPaths(id, { maxDepth: 1 });
    expect(shallow.paths.length).toBe(0);
    expect(shallow.limitsReached.depth).toBe(true);
    expect(shallow.limitsReached.nodes).toBe(false);

    // The 300 copies of one string, which is a group of 300 and not 300
    // groups of one.
    const repeated = heap.duplicateStrings().find((g) => g.value.includes('not to be interned away'));
    expect(repeated !== undefined).toBe(true);
    expect(repeated!.count).toBeGreaterThanOrEqual(300);
    expect(repeated!.nodes.length).toBe(repeated!.count);

    // The query filters, each deciding something on its own.
    //
    // `className` matches a node's own NAME, which is not the same
    // question `classObjects` answers: the constructor function is
    // called Widget too, and so is the string holding that name, so the
    // 200 instances come back only once the type narrows it.
    expect(heap.query({ className: '^Widget$', nodeType: 'object' }).length).toBe(200);
    expect(heap.query({ className: '^Widget$', nodeType: 'closure' }).length).toBe(1);
    expect(heap.query({ className: '^Widget$' }).length).toBeGreaterThan(200);
    // Case-insensitive, so the same objects come back spelled either way.
    expect(heap.query({ className: '^widget$', nodeType: 'object' }).length).toBe(200);
    expect(heap.query({ className: '^Widget$', nodeType: 'regexp' }).length).toBe(0);

    const detached = heap.query({ isDetached: true });
    expect(detached.length).toBeGreaterThan(100);
    expect(detached.every((n) => n.detachedDOMTreeNode || n.name.startsWith('Detached '))).toBe(true);

    // Heaviest first by default, oldest first by id.
    const bySize = heap.query({ className: '^Widget$', nodeType: 'object' });
    expect(bySize[0].retainedSize).toBeGreaterThanOrEqual(bySize[bySize.length - 1].retainedSize);
    const byId = heap.query({ className: '^Widget$', nodeType: 'object', sortBy: 'id' });
    expect(byId[0].id).toBeLessThan(byId[byId.length - 1].id);

    // And the size bands, which nothing above exercises.
    const heavy = heap.query({ minSelfSize: 1024 });
    expect(heavy.length).toBeGreaterThan(0);
    expect(heavy.every((n) => n.selfSize >= 1024)).toBe(true);
    expect(heap.query({ minSelfSize: 1024, maxSelfSize: 1023 }).length).toBe(0);
  });

  test('a diff names what the page allocated in between', async ({ page, browserName }) => {
    await page.setContent(RETAINING);
    if (browserName !== 'chromium') {
      let message = '';
      try {
        await page.takeHeapSnapshot();
      } catch (e) {
        message = String(e);
      }
      expect(message.includes('Chromium')).toBe(true);
      return;
    }

    const before = await page.takeHeapSnapshot();
    await page.evaluate('globalThis.__grow()');
    const after = await page.takeHeapSnapshot();

    const diff = after.diffSince(before);
    expect(diff.length).toBeGreaterThan(0);

    // 500 more Widgets, and none of the first 200 collected: the merge
    // has to match those by id rather than report the class replaced.
    const widgets = diff.find((d) => d.name === 'Widget');
    expect(widgets !== undefined).toBe(true);
    expect(widgets!.addedCount).toBe(500);
    expect(widgets!.removedCount).toBe(0);
    expect(widgets!.countDelta).toBe(500);
    expect(widgets!.addedIds.length).toBe(500);
    expect(widgets!.addedSelfSizes.every((s) => s > 0)).toBe(true);

    // The added ids are new: none of them was in the earlier snapshot.
    const known = new Set(before.classObjects(widgets!.classKey).map((n) => n.id));
    expect(widgets!.addedIds.some((id) => known.has(id))).toBe(false);
    // And they are objects the later snapshot can still be asked about.
    expect(after.object(widgets!.addedIds[0]).name).toBe('Widget');

    // Two hundred of the repeated strings were dropped, so a class went
    // the other way too.
    expect(diff.some((d) => d.removedCount > 0)).toBe(true);

    // Heaviest growth first.
    expect(diff[0].sizeDelta).toBeGreaterThanOrEqual(diff[diff.length - 1].sizeDelta);
  });
});
