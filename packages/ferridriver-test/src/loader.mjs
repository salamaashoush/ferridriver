/**
 * Node.js ESM resolve hook.
 *
 * Maps .js imports to .ts files — the standard TypeScript convention where
 * source code uses `import './foo.js'` but the actual file is `./foo.ts`.
 *
 * TypeScript syntax handling is done by Node.js --experimental-transform-types
 * (or native strip-types on Node 22.6+). This hook only handles resolution.
 */

const JS_TO_TS = { '.js': '.ts', '.mjs': '.mts', '.cjs': '.cts', '.jsx': '.tsx' };
const TS_EXTENSIONS = ['.ts', '.tsx', '.mts', '.cts'];

export async function resolve(specifier, context, nextResolve) {
  try {
    return await nextResolve(specifier, context);
  } catch (err) {
    if (err.code !== 'ERR_MODULE_NOT_FOUND') throw err;
    // .js → .ts mapping
    for (const [jsExt, tsExt] of Object.entries(JS_TO_TS)) {
      if (specifier.endsWith(jsExt)) {
        try { return await nextResolve(specifier.slice(0, -jsExt.length) + tsExt, context); } catch {}
      }
    }
    // Extensionless → try .ts extensions
    for (const ext of TS_EXTENSIONS) {
      try { return await nextResolve(specifier + ext, context); } catch {}
    }
    throw err;
  }
}
