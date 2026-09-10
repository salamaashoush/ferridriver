import assert from 'node:assert/strict';
import { test } from '@ferridriver/test';
import { script } from './support.mjs';

test('bundled module uses native fs path buffer', async () => {
  const { value } = await script("import fs from 'node:fs';\nimport { readFileSync } from 'fs';\nimport { readFile } from 'node:fs/promises';\nimport path from 'node:path';\nimport { Buffer } from 'node:buffer';\nconst file: string = path.resolve('data.txt');\nconst viaDefault: string = fs.readFileSync(file, 'utf8');\nconst viaNamed: string = readFileSync(file, 'utf8');\nconst viaPromises: string = await readFile(file, 'utf8');\nconst joined: string = path.join('a', '..', 'b', 'c.txt');\nconst ext: string = path.extname(joined);\nconst b64: string = Buffer.from('hi').toString('base64');\nconst round: string = Buffer.from(b64, 'base64').toString('utf8');\nexport default { viaDefault, viaNamed, viaPromises, joined, ext, b64, round };\n", {"data.txt": "hello-node-compat"}, { module: true });
  assert.deepEqual(value, {
    "viaDefault": "hello-node-compat",
    "viaNamed": "hello-node-compat",
    "viaPromises": "hello-node-compat",
    "joined": "b/c.txt",
    "ext": ".txt",
    "b64": "aGk=",
    "round": "hi"
  });
});

test('dynamic import resolves native modules in plain scripts', async () => {
  const { value } = await script("\n      const path = (await import('path')).default;\n      const { Buffer } = await import('buffer');\n      const fd = await import('ferridriver');\n      const cucumber = await import('@cucumber/cucumber');\n      return {\n        dir: path.dirname('/a/b/c.txt'),\n        rel: path.relative('/a/b', '/a/d'),\n        hex: Buffer.from([0xde, 0xad]).toString('hex'),\n        isBuf: Buffer.isBuffer(Buffer.alloc(2)),\n        host: fd.host,\n        givenIsFn: typeof cucumber.Given === 'function',\n      };\n      ", {}, { module: false });
  assert.deepEqual(value, {
    "dir": "/a/b",
    "rel": "../d",
    "hex": "dead",
    "isBuf": true,
    "host": "script",
    "givenIsFn": true
  });
});

test('bundled module uses zlib string decoder perf hooks tty and stream web', async () => {
  const { value } = await script("import zlib from 'node:zlib';\nimport { StringDecoder } from 'node:string_decoder';\nimport { performance as perf } from 'node:perf_hooks';\nimport { isatty } from 'node:tty';\nimport { ReadableStream } from 'node:stream/web';\nimport { Buffer } from 'node:buffer';\nconst text = (b: any): string => b.toString('utf8');\nconst gzip = text(zlib.gunzipSync(zlib.gzipSync(Buffer.from('hello zlib'))));\nconst deflate = text(zlib.inflateSync(zlib.deflateSync(Buffer.from('deflate me'))));\nconst deflateRaw = text(zlib.inflateRawSync(zlib.deflateRawSync(Buffer.from('raw me'))));\nconst brotli = text(zlib.brotliDecompressSync(zlib.brotliCompressSync(Buffer.from('brotli me'))));\nconst zstd = text(zlib.zstdDecompressSync(zlib.zstdCompressSync(Buffer.from('zstd me'))));\n// A euro sign split across two writes: the decoder has to hold the\n// partial sequence rather than emit a replacement character.\nconst d = new StringDecoder('utf8');\nconst split = d.write(Buffer.from([0xe2, 0x82])) + d.end(Buffer.from([0xac]));\nexport default {\n  gzip, deflate, deflateRaw, brotli, zstd, split,\n  perfNow: typeof perf.now(),\n  isatty: typeof isatty(1),\n  streamWeb: typeof ReadableStream,\n};\n", {}, { module: true });
  assert.deepEqual(value, {
    "gzip": "hello zlib",
    "deflate": "deflate me",
    "deflateRaw": "raw me",
    "brotli": "brotli me",
    "zstd": "zstd me",
    "split": "€",
    "perfNow": "number",
    "isatty": "boolean",
    "streamWeb": "function"
  });
});

test('dynamic import resolves the vendored modules and navigator names this runtime', async () => {
  const { value } = await script("\n      const zlib = (await import('zlib')).default;\n      const { StringDecoder } = await import('string_decoder');\n      const { isatty } = await import('tty');\n      const { ReadableStream } = await import('stream/web');\n      return {\n        gzip: zlib.gunzipSync(zlib.gzipSync(Buffer.from('dyn'))).toString('utf8'),\n        decoder: new StringDecoder('utf8').end(Buffer.from('ok')),\n        isatty: typeof isatty(1),\n        streamWeb: typeof ReadableStream,\n        ua: navigator.userAgent,\n      };\n      ", {}, { module: false });
  assert.equal(value.gzip, 'dyn');
  assert.equal(value.decoder, 'ok');
  assert.equal(value.isatty, 'boolean');
  assert.equal(value.streamWeb, 'function');
  assert.ok(value.ua.startsWith('ferridriver/'));
});

test('performance covers user timing and the timeline', async () => {
  const { value } = await script("\n      const threw = (fn) => { try { fn(); return false; } catch { return true; } };\n\n      const m = performance.mark('a', { startTime: 10, detail: { k: 1 } });\n      performance.mark('b', { startTime: 40 });\n      // Backdated: recorded third, belongs first on the timeline.\n      performance.mark('early', { startTime: 5 });\n\n      const span = performance.measure('span', 'a', 'b');\n      const fromOptions = performance.measure('opts', { start: 100, duration: 25 });\n      const derivedStart = performance.measure('derived', { end: 80, duration: 30 });\n\n      return {\n        // now() is monotonic and moves forward, timeOrigin is wall clock.\n        nowMonotonic: performance.now() >= 0 && performance.now() <= performance.now(),\n        timeOriginIsWallClock: performance.timeOrigin > 1.7e12,\n        toJSON: Object.keys(performance.toJSON()).join(','),\n\n        markShape: [m.name, m.entryType, m.startTime, m.duration].join('|'),\n        markDetail: m.detail.k,\n        markIsEntry: m instanceof PerformanceEntry && m instanceof PerformanceMark,\n\n        measureShape: [span.name, span.entryType, span.startTime, span.duration].join('|'),\n        measureIsEntry: span instanceof PerformanceEntry && span instanceof PerformanceMeasure,\n        fromOptions: [fromOptions.startTime, fromOptions.duration].join('|'),\n        derivedStart: [derivedStart.startTime, derivedStart.duration].join('|'),\n\n        // getEntries sorts by startTime, not insertion order.\n        order: performance.getEntries().map((e) => e.name).join(','),\n        byType: performance.getEntriesByType('mark').map((e) => e.name).join(','),\n        byName: performance.getEntriesByName('a', 'mark').length,\n        byNameWrongType: performance.getEntriesByName('a', 'measure').length,\n        unknownType: performance.getEntriesByType('resource').length,\n\n        entryJson: JSON.stringify(span.toJSON()),\n\n        // Refusals the spec requires.\n        bothEndForms: threw(() => performance.measure('x', { start: 1 }, 'b')),\n        allThreeOptions: threw(() => performance.measure('x', { start: 1, end: 2, duration: 1 })),\n        emptyOptions: threw(() => performance.measure('x', {})),\n        missingMark: threw(() => performance.measure('x', 'nope')),\n        negativeStart: threw(() => performance.mark('x', { startTime: -1 })),\n        notConstructible: threw(() => new Performance()),\n\n        clearedMarks: (() => {\n          performance.clearMarks('a');\n          return performance.getEntriesByType('mark').map((e) => e.name).join(',');\n        })(),\n        clearedMeasures: (() => {\n          performance.clearMeasures();\n          return performance.getEntriesByType('measure').length;\n        })(),\n      };\n      ", {}, { module: false });
  assert.deepEqual(value, {
    "nowMonotonic": true,
    "timeOriginIsWallClock": true,
    "toJSON": "timeOrigin",
    "markShape": "a|mark|10|0",
    "markDetail": 1,
    "markIsEntry": true,
    "measureShape": "span|measure|10|30",
    "measureIsEntry": true,
    "fromOptions": "100|25",
    "derivedStart": "50|30",
    "order": "early,a,span,b,derived,opts",
    "byType": "early,a,b",
    "byName": 1,
    "byNameWrongType": 0,
    "unknownType": 0,
    "entryJson": "{\"name\":\"span\",\"entryType\":\"measure\",\"startTime\":10,\"duration\":30,\"detail\":null}",
    "bothEndForms": true,
    "allThreeOptions": true,
    "emptyOptions": true,
    "missingMark": true,
    "negativeStart": true,
    "notConstructible": true,
    "clearedMarks": "early,b",
    "clearedMeasures": 0
  });
});

test('performance now and process hrtime share one base', async () => {
  const { value } = await script("\n      const before = process.hrtime();\n      const now = performance.now();\n      const after = process.hrtime();\n      const toMs = hr => hr[0] * 1000 + hr[1] / 1e6;\n      return { before: toMs(before), now, after: toMs(after) };\n      ", {}, { module: false });
  for (const key of ['before', 'now', 'after']) assert.equal(typeof value[key], 'number');
  assert.ok(value.before <= value.now && value.now <= value.after, JSON.stringify(value));
});
