// Bun server: serves the built Vite dist + mock API.
// `vite build` writes to ./dist; this server hosts that and the
// /api/* routes used by the React app's react-query calls.
//
// run: `bun server.ts`  (env PORT defaults 3030)
//
// Both fd and pw configs spawn this via webServer with a /healthz
// readiness probe. Cleanup is automatic when the parent dies because
// Bun.serve owns the listener.

import { join } from 'node:path';

const PORT = Number(process.env.PORT ?? 3030);
const DIST = join(import.meta.dir, 'dist');
const FALLBACK = join(DIST, 'index.html');

// ── Seeded fixtures ────────────────────────────────────────────────
// Deterministic so identical bench runs produce identical data.

function seedPosts() {
  const tags = ['rust', 'typescript', 'react', 'cdp', 'perf', 'web', 'ai', 'api'];
  return Array.from({ length: 200 }, (_, i) => ({
    slug: `post-${String(i).padStart(3, '0')}`,
    title: `Post ${i}: ${[
      'Performance',
      'Architecture',
      'Async',
      'Hooks',
      'Routing',
      'Testing',
      'Caching',
      'Streaming',
    ][i % 8]} adventures`,
    excerpt: `Excerpt of post ${i}. ${'lorem ipsum '.repeat(3)}`.trim(),
    body: `Full body of post ${i}.\n\n${'lorem ipsum dolor sit amet. '.repeat(40)}`,
    tags: [tags[i % tags.length], tags[(i + 3) % tags.length]],
    date: `2026-${String((i % 12) + 1).padStart(2, '0')}-${String((i % 27) + 1).padStart(2, '0')}`,
  }));
}

function seedSales() {
  const regions: Array<'NA' | 'EU' | 'APAC'> = ['NA', 'EU', 'APAC'];
  const products = ['Widget', 'Gadget', 'Sprocket', 'Doohickey', 'Thingamajig'];
  const statuses: Array<'pending' | 'shipped' | 'delivered' | 'returned'> = [
    'pending',
    'shipped',
    'delivered',
    'returned',
  ];
  return Array.from({ length: 500 }, (_, i) => ({
    id: `S-${String(i).padStart(4, '0')}`,
    region: regions[i % regions.length],
    product: products[i % products.length],
    amount: 100 + ((i * 137) % 9_900),
    date: `2026-${String((i % 12) + 1).padStart(2, '0')}-${String((i % 27) + 1).padStart(2, '0')}`,
    status: statuses[i % statuses.length],
  }));
}

const POSTS = seedPosts();
const SALES = seedSales();

function json(body: unknown, init?: ResponseInit): Response {
  return new Response(JSON.stringify(body), {
    ...init,
    headers: {
      'content-type': 'application/json',
      'cache-control': 'no-store',
      ...(init?.headers as Record<string, string> | undefined),
    },
  });
}

// ── Routes ─────────────────────────────────────────────────────────

const server = Bun.serve({
  port: PORT,
  async fetch(req) {
    const url = new URL(req.url);
    const path = url.pathname;

    // Health probe — webServer.url config waits for 200 here.
    if (path === '/healthz') return new Response('ok');

    if (path === '/api/posts') {
      // Light artificial latency so async paths exercise loading state.
      await Bun.sleep(5);
      return json(POSTS);
    }
    if (path.startsWith('/api/posts/')) {
      const slug = path.slice('/api/posts/'.length);
      const post = POSTS.find((p) => p.slug === slug);
      if (!post) return json({ error: 'not found' }, { status: 404 });
      await Bun.sleep(5);
      return json(post);
    }
    if (path === '/api/sales') {
      await Bun.sleep(8);
      return json(SALES);
    }
    if (path === '/api/submit' && req.method === 'POST') {
      // Echo + 100ms simulated server processing.
      const body = await req.json();
      await Bun.sleep(20);
      return json(body);
    }

    // Static dist + SPA fallback.
    const filePath = path === '/' ? join(DIST, 'index.html') : join(DIST, path);
    const file = Bun.file(filePath);
    if (await file.exists()) {
      return new Response(file);
    }
    // Unknown path → SPA index for client-side routing.
    return new Response(Bun.file(FALLBACK));
  },
});

console.log(`bench app server listening on http://localhost:${server.port}`);
