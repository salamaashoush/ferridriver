/**
 * ferridriver CT test runner orchestrator.
 *
 * Complete pipeline:
 * 1. Scan test files for component imports
 * 2. Build Vite bundle with component registry
 * 3. Start preview server
 * 4. Run tests with mount() fixture
 * 5. Clean up
 *
 * Usage:
 *   import { createCtRunner } from '@ferridriver/ct-core/runner';
 *
 *   const runner = await createCtRunner({
 *     testDir: './src',
 *     testMatch: '**\/*.ct.test.{ts,tsx}',
 *     framework: 'react',  // or 'vue', 'svelte'
 *     registerSourcePath: require.resolve('@ferridriver/ct-react/register'),
 *     frameworkPlugin: () => import('@vitejs/plugin-react').then(m => m.default()),
 *   });
 *   // runner.baseUrl is the preview server URL
 *   // runner.registry has the component map
 *   // runner.stop() shuts down
 */

import { resolve, join, dirname } from "path";
import { mkdirSync, writeFileSync, readFileSync, existsSync } from "fs";
import { scanTestFiles } from "./importTransform.mjs";
import { ferridriverCtPlugin } from "./vitePlugin.mjs";
import { fileURLToPath } from "url";

const __dirname = dirname(fileURLToPath(import.meta.url));

/**
 * Create and start a CT runner.
 *
 * @param {object} config
 * @param {string} config.projectDir - project root (where package.json lives)
 * @param {string[]} config.testFiles - absolute paths to test files to scan
 * @param {string} config.registerSourcePath - path to framework's registerSource.mjs
 * @param {Function} config.frameworkPlugin - async function returning Vite plugin
 * @param {number} [config.port=3100] - preview server port
 * @param {string} [config.cacheDir] - build output dir
 */
export async function createCtRunner(config) {
  const {
    projectDir,
    testFiles,
    registerSourcePath,
    frameworkPlugin,
    port = 3100,
    cacheDir = join(projectDir, "node_modules", ".ferridriver-ct"),
  } = config;

  // Step 1: Scan test files for component imports.
  const { registry, transformedFiles } = scanTestFiles(testFiles);
  console.log(
    `[ferridriver-ct] Found ${registry.size} component(s) in ${testFiles.length} test file(s)`
  );

  // Step 2: Create ferridriver/ template dir in the project (like Playwright's playwright/ dir).
  const templateDir = join(projectDir, ".ferridriver-ct");
  mkdirSync(templateDir, { recursive: true });

  // index.html in project root — entry point for the dev server.
  const indexHtmlPath = join(projectDir, "__ferri_ct_index.html");
  writeFileSync(
    indexHtmlPath,
`<!DOCTYPE html>
<html><head><meta charset="utf-8"></head>
<body><div id="root"></div>
<script type="module" src="./.ferridriver-ct/index.ts"></script>
</body></html>`
  );

  // index.ts — empty entry (the Vite plugin will inject registry + runtime).
  writeFileSync(join(templateDir, "index.ts"), "// ferridriver CT entry\n");

  // Step 3: Build Vite config.
  // Import Vite from the project's node_modules, not from ct-core's location.
  const { createRequire } = await import("module");
  const projectRequire = createRequire(join(projectDir, "package.json"));
  const vite = await import(projectRequire.resolve("vite"));
  // Only add our CT plugin. The framework plugin (react, vue, etc.) comes from
  // the project's vite.config.ts which we load via configFile.
  const ctPlugin = ferridriverCtPlugin(registry, registerSourcePath, templateDir);
  const plugins = [ctPlugin];

  const viteConfig = {
    // Root = project dir so Vite resolves node_modules (react, etc.) correctly.
    // Use the project's vite.config if it exists (has framework plugin configured).
    root: projectDir,
    configFile: existsSync(join(projectDir, "vite.config.ts"))
      ? join(projectDir, "vite.config.ts")
      : existsSync(join(projectDir, "vite.config.js"))
        ? join(projectDir, "vite.config.js")
        : false,
    plugins,
    server: {
      port,
      strictPort: false,
      host: "127.0.0.1",
    },
  };

  // Step 4: Start Vite dev server (not build+preview).
  // Dev mode handles CJS/ESM interop (React 19+) and provides HMR.
  console.log("[ferridriver-ct] Starting Vite dev server...");
  const devServer = await vite.createServer(viteConfig);
  await devServer.listen(port);
  const address = devServer.httpServer.address();
  const baseUrl = `http://127.0.0.1:${address.port}`;
  console.log(`[ferridriver-ct] Serving at ${baseUrl}`);

  // The entry HTML is at __ferri_ct_index.html, not index.html.
  const entryUrl = `${baseUrl}/__ferri_ct_index.html`;

  return {
    baseUrl: entryUrl,
    registry,
    transformedFiles,
    stop: async () => {
      await devServer.close();
      // Clean up generated files.
      const { rmSync } = await import("fs");
      try { rmSync(indexHtmlPath); } catch {}
      try { rmSync(templateDir, { recursive: true }); } catch {}
    },
  };
}
