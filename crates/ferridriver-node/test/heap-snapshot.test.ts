// NAPI coverage for page.takeHeapSnapshot(), the V8 heap snapshot and
// the queries over it.
//
// What the analysis AGREES with is checked far more thoroughly
// elsewhere: `just heap-diff` runs DevTools' own heap engine over four
// snapshots and compares node by node. This is the binding surface —
// that a handle crosses the boundary as a class rather than a plain
// object, that the option bags reach Rust, and that every answer keeps
// the field names the generated .d.ts promises.

import { describe, it, expect, beforeAll, afterAll } from "bun:test";
import { type Browser, type Page } from "../index.js";
import { launchForBackend } from "./_helpers.js";

const BACKENDS: string[] = process.env.FERRIDRIVER_BACKEND ? [process.env.FERRIDRIVER_BACKEND] : ["cdp-pipe"];

// A page holding a named class with a known population, three hundred
// copies of one string, and a detached DOM subtree. Assembled rather
// than repeated, because V8 hands back the same string for a full slice
// and interns a literal.
const RETAINING = `<html><body><main>heap</main><script>
  class Widget {
    constructor(n) { this.n = n; this.label = 'widget ' + n; }
  }
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

for (const backend of BACKENDS) {
  describe(`heap snapshots [${backend}]`, () => {
    let browser: Browser;
    let page: Page;

    beforeAll(async () => {
      browser = await launchForBackend(backend);
      page = await browser.newPage();
      await page.setContent(RETAINING);
    });

    afterAll(async () => {
      await browser.close();
    });

    if (backend !== "cdp-pipe" && backend !== "cdp-raw") {
      it("rejects on an engine whose heap format is not V8's", async () => {
        expect(page.takeHeapSnapshot()).rejects.toThrow(/Chromium/);
      });
      return;
    }

    it("captures a graph and a file the Memory panel would open", async () => {
      const heap = await page.takeHeapSnapshot();
      expect(heap.nodeCount()).toBeGreaterThan(1000);
      expect(heap.totalSize()).toBeGreaterThan(0);

      const stats = heap.statistics();
      expect(stats.total).toBe(stats.native.total + stats.v8heap.total);
      expect(stats.v8heap.strings).toBeGreaterThan(0);
      expect(stats.native.total).toBeGreaterThan(0);

      const written = JSON.parse(new TextDecoder().decode(heap.bytes()));
      expect(Array.isArray(written.snapshot.meta.node_fields)).toBe(true);
      expect(written.snapshot.node_count).toBe(heap.nodeCount());
    });

    it("groups the page's own class and lists its members", async () => {
      const heap = await page.takeHeapSnapshot();
      const widgets = heap.classes().find((c) => c.name === "Widget");
      expect(widgets).toBeDefined();
      expect(widgets!.count).toBe(200);
      expect(widgets!.maxRetainedSize).toBeGreaterThan(0);
      // A constructor with a script location is keyed by it, so two of
      // the same name from different scripts stay apart.
      expect(widgets!.classKey.startsWith(",")).toBe(false);

      const members = heap.classObjects(widgets!.classKey);
      expect(members.length).toBe(200);
      expect(members.every((m) => m.name === "Widget" && m.type === "object")).toBe(true);
      expect(() => heap.classObjects(",NoSuchClass")).toThrow(/NoSuchClass/);
    });

    it("answers about one object from every angle", async () => {
      const heap = await page.takeHeapSnapshot();
      const widgets = heap.classes().find((c) => c.name === "Widget")!;
      const id = heap.classObjects(widgets.classKey)[0].id;

      const object = heap.object(id);
      expect(object.id).toBe(id);
      expect(object.name).toBe("Widget");
      expect(object.selfSize).toBeGreaterThan(0);
      expect(object.retainerCount).toBeGreaterThan(0);
      expect(() => heap.object(-1)).toThrow(/nodeId/);

      expect(heap.edges(id).map((e) => e.name)).toContain("label");
      expect(heap.retainers(id).length).toBeGreaterThan(0);

      const chain = heap.dominators(id);
      expect(chain[0].nodeId).toBe(id);
      expect(chain[chain.length - 1].retainedSize).toBeGreaterThanOrEqual(chain[0].retainedSize);

      const paths = heap.retainingPaths(id);
      expect(paths.paths.length).toBeGreaterThan(0);
      expect(paths.paths[0].distance).toBeGreaterThanOrEqual(2);

      // A budget the search cannot fit in is reported rather than
      // silently truncating the answer.
      const shallow = heap.retainingPaths(id, { maxDepth: 1 });
      expect(shallow.paths.length).toBe(0);
      expect(shallow.limitsReached.depth).toBe(true);
      expect(shallow.limitsReached.nodes).toBe(false);
    });

    it("finds the duplicated string and the objects a query describes", async () => {
      const heap = await page.takeHeapSnapshot();

      const repeated = heap.duplicateStrings().find((g) => g.value.includes("not to be interned away"));
      expect(repeated).toBeDefined();
      expect(repeated!.count).toBeGreaterThanOrEqual(300);
      expect(repeated!.nodes.length).toBe(repeated!.count);

      // `className` matches a node's own NAME, so the constructor
      // function answers to it too; the type is what narrows it to the
      // instances.
      expect(heap.query({ className: "^Widget$", nodeType: "object" }).length).toBe(200);
      expect(heap.query({ className: "^Widget$", nodeType: "closure" }).length).toBe(1);
      expect(heap.query({ className: "^widget$", nodeType: "object" }).length).toBe(200);

      const detached = heap.query({ isDetached: true });
      expect(detached.length).toBeGreaterThan(100);

      const byId = heap.query({ className: "^Widget$", nodeType: "object", sortBy: "id" });
      expect(byId[0].id).toBeLessThan(byId[byId.length - 1].id);

      const heavy = heap.query({ minSelfSize: 1024 });
      expect(heavy.length).toBeGreaterThan(0);
      expect(heavy.every((n) => n.selfSize >= 1024)).toBe(true);
      expect(heap.query({ minSelfSize: 1024, maxSelfSize: 1023 }).length).toBe(0);

      expect(() => heap.query({ className: "(" })).toThrow(/regular expression/);
    });

    it("narrows the heap to what one thing is holding", async () => {
      const heap = await page.takeHeapSnapshot();
      const all = heap.classes();

      // A fraction of the heap, not the heap: a filter that quietly
      // kept everything would pass any assertion about one class.
      const detached = heap.classes({ filterName: "objectsRetainedByDetachedDomNodes" });
      expect(detached.length).toBeGreaterThan(0);
      expect(detached.length).toBeLessThan(all.length);
      expect(detached.every((c) => c.name.startsWith("Detached "))).toBe(true);

      const paragraphs = detached.find((c) => c.name === "Detached <p>")!;
      expect(paragraphs).toBeDefined();
      const members = heap.classObjects(paragraphs.classKey, {
        filterName: "objectsRetainedByDetachedDomNodes",
      });
      expect(members.length).toBe(paragraphs.count);
      expect(members.every((m) => m.detachedDOMTreeNode)).toBe(true);

      const realms = heap.nativeContexts();
      expect(realms.nativeContexts.length).toBeGreaterThan(0);
      expect(realms.nativeContexts.every((c) => c.nodeName.includes("NativeContext"))).toBe(true);
      const owner = realms.nativeContexts.find((c) => c.attributedSize > 0)!;
      expect(owner).toBeDefined();
      const owned = heap.classes({ filterName: "attributedToNativeContext", objectId: owner.nodeId });
      expect(owned.length).toBeGreaterThan(0);
      expect(owned.length).toBeLessThan(all.length);

      const summary = heap.contextSummary();
      expect(summary.totalSize).toBe(summary.retainedByContextSize + summary.notRetainedByContextSize);
      expect(summary.contextCount).toBeGreaterThan(0);

      // An unknown name is refused rather than quietly meaning "all",
      // and the one filter that needs an id says so without one.
      expect(() => heap.classes({ filterName: "noSuchFilter" as never })).toThrow(/noSuchFilter/);
      expect(() => heap.classes({ filterName: "attributedToNativeContext" })).toThrow(/objectId/);
    });

    it("names what the page allocated between two snapshots", async () => {
      const before = await page.takeHeapSnapshot();
      await page.evaluate("globalThis.__grow()");
      const after = await page.takeHeapSnapshot();

      const diff = after.diffSince(before);
      const widgets = diff.find((d) => d.name === "Widget");
      expect(widgets).toBeDefined();
      // The first two hundred survived, so they are matched by id
      // rather than reported as replaced.
      expect(widgets!.addedCount).toBe(500);
      expect(widgets!.removedCount).toBe(0);
      expect(widgets!.addedIds.length).toBe(500);
      expect(after.object(widgets!.addedIds[0]).name).toBe("Widget");

      const known = new Set(before.classObjects(widgets!.classKey).map((n) => n.id));
      expect(widgets!.addedIds.some((id) => known.has(id))).toBe(false);

      // Two hundred strings were dropped, so a class went the other way.
      expect(diff.some((d) => d.removedCount > 0)).toBe(true);
      expect(diff[0].sizeDelta).toBeGreaterThanOrEqual(diff[diff.length - 1].sizeDelta);
    });
  });
}
