// Run Playwright's own screenshot comparators over the fixture pairs and
// record their verdicts, which `tests/comparator_differential.rs`
// holds ferridriver's port to.
//
//   bun scripts/comparator-diff/engine.mjs [fixtures-dir] > upstream.json
//
// The comparators are read from the Playwright checkout beside this
// repository (`PLAYWRIGHT_REPO` overrides it), unmodified: pixelmatch is
// `packages/utils/third_party/pixelmatch.js`, ssim-cie94 is
// `packages/utils/image_tools/compare.ts`, and images of different sizes
// are padded by `packages/isomorphic/imageUtils.ts::padImageToSize`, as
// `comparators.ts::compareImages` does.
import { createHash } from 'node:crypto';
import { readdirSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join, resolve } from 'node:path';
import { execFileSync } from 'node:child_process';
import { decodePng } from '../../tests/e2e/helpers/png.ts';

const root = resolve(import.meta.dirname, '../..');
const repo = resolve(process.env.PLAYWRIGHT_REPO ?? join(root, '../playwright'));
const fixtures = resolve(process.argv[2] ?? join(root, 'crates/ferridriver-test/tests/fixtures/comparator'));

const pixelmatch = createRequire(import.meta.url)(join(repo, 'packages/utils/third_party/pixelmatch.js'));
const { compare } = await import(join(repo, 'packages/utils/image_tools/compare.ts'));
const { padImageToSize } = await import(join(repo, 'packages/isomorphic/imageUtils.ts'));

const THRESHOLDS = ['0', '0.1', '0.2', '0.3', '0.5'];
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');

const image = file => {
  const png = decodePng(readFileSync(file));
  return { width: png.width, height: png.height, data: Buffer.from(png.data) };
};

const cases = {};
for (const name of readdirSync(fixtures, { withFileTypes: true }).filter(e => e.isDirectory()).map(e => e.name).sort()) {
  let expected = image(join(fixtures, name, 'expected.png'));
  let actual = image(join(fixtures, name, 'actual.png'));
  const size = { width: Math.max(expected.width, actual.width), height: Math.max(expected.height, actual.height) };
  expected = padImageToSize(expected, size);
  actual = padImageToSize(actual, size);
  const result = { width: size.width, height: size.height, pixelmatch: {}, ssim: {} };
  for (const threshold of THRESHOLDS) {
    const diff = Buffer.alloc(size.width * size.height * 4);
    const count = pixelmatch(expected.data, actual.data, diff, size.width, size.height, { threshold: Number(threshold) });
    result.pixelmatch[threshold] = { count, diff: sha256(diff) };
  }
  const diff = Buffer.alloc(size.width * size.height * 4);
  const count = compare(expected.data, actual.data, diff, size.width, size.height, { maxColorDeltaE94: 1.0 });
  result.ssim = { count, diff: sha256(diff) };
  cases[name] = result;
}

const commit = execFileSync('git', ['-C', repo, 'rev-parse', '--short', 'HEAD'], { encoding: 'utf8' }).trim();
console.log(JSON.stringify({ upstream: `microsoft/playwright@${commit}`, thresholds: THRESHOLDS, cases }, null, 2));
