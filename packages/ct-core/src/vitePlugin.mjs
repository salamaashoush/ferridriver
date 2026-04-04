/**
 * ferridriver CT Vite plugin.
 *
 * This plugin:
 * 1. Reads the component registry (populated by the import transform)
 * 2. Transforms the ferridriver/index.ts entry file to inject:
 *    - The browser runtime (injected/index.js)
 *    - The framework registerSource (e.g. ct-react/registerSource.mjs)
 *    - Lazy import() calls for every component
 *    - Registry initialization
 * 3. During build, produces a self-contained bundle served by the preview server
 *
 * Usage in ferridriver.ct.config.ts:
 *   import { createPlugin } from '@ferridriver/ct-core/vitePlugin';
 *   export default { plugins: [createPlugin(componentMap, registerSourcePath)] };
 */

import { readFileSync } from "fs";
import { resolve, dirname, join } from "path";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));

/**
 * Create the Vite plugin.
 *
 * @param {Map<string, { id: string, importSource: string, remoteName?: string }>} componentRegistry
 *   Map of component ID → import info (populated by scanning test files)
 * @param {string} registerSourcePath
 *   Absolute path to the framework's registerSource.mjs
 * @param {string} [templateDir]
 *   Directory containing ferridriver/index.html + ferridriver/index.ts
 */
export function ferridriverCtPlugin(
  componentRegistry,
  registerSourcePath,
  templateDir
) {
  // Read the injected runtime source.
  const injectedSource = readFileSync(
    join(__dirname, "injected", "index.js"),
    "utf-8"
  );
  const registerSource = readFileSync(registerSourcePath, "utf-8");

  return {
    name: "ferridriver-ct",

    // Transform the entry file to inject the registry.
    transform(code, id) {
      // Match .ferridriver-ct/index.ts (our generated entry file).
      const normalized = resolve(id);
      const isEntry = normalized.includes(".ferridriver-ct") && normalized.includes("index.");

      if (!isEntry) return null;
      console.log(`[ferridriver-ct-plugin] Transforming: ${id} (${componentRegistry.size} components)`);

      const lines = [code, ""];

      // Inject runtime + registerSource.
      lines.push("// --- ferridriver CT runtime ---");
      lines.push(injectedSource);
      lines.push("");
      lines.push("// --- framework registerSource ---");
      lines.push(registerSource);
      lines.push("");

      // Generate lazy import() calls for each component.
      // Import paths must be absolute (Vite resolves from the template dir, not the test file).
      lines.push("// --- component registry ---");
      for (const [id, info] of componentRegistry.entries()) {
        const remoteName = info.remoteName || "default";
        // Resolve the import path relative to the test file that imported it.
        let importPath = info.importSource;
        if (importPath.startsWith(".") && info.filename) {
          importPath = resolve(dirname(info.filename), importPath);
        }
        lines.push(
          `const ${id} = () => import('${importPath}').then(mod => mod.${remoteName});`
        );
      }

      // Initialize the registry.
      const ids = [...componentRegistry.keys()];
      lines.push(
        `window.__ferriRegistry.initialize({ ${ids.join(", ")} });`
      );

      const output = lines.join("\n");
      console.log("[ferridriver-ct-plugin] Generated code (last 500 chars):", output.slice(-500));
      return { code: output, map: null };
    },
  };
}
