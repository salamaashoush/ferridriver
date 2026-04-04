/**
 * @ferridriver/ct-react
 *
 * Main entry point. Exports the register source path so the CT framework
 * can inject it into the browser page.
 */

import { fileURLToPath } from "url";
import { dirname, join } from "path";

const __dirname = dirname(fileURLToPath(import.meta.url));

export const registerSourcePath = join(__dirname, "registerSource.mjs");
export const frameworkName = "react";
export const vitePlugin = () =>
  import("@vitejs/plugin-react").then((m) => m.default());
