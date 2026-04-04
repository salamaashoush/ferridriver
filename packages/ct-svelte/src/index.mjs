import { fileURLToPath } from "url";
import { dirname, join } from "path";

const __dirname = dirname(fileURLToPath(import.meta.url));

export const registerSourcePath = join(__dirname, "registerSource.mjs");
export const frameworkName = "svelte";
export const vitePlugin = () =>
  import("@sveltejs/vite-plugin-svelte").then((m) => m.svelte());
