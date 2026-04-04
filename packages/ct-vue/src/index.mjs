import { fileURLToPath } from "url";
import { dirname, join } from "path";

const __dirname = dirname(fileURLToPath(import.meta.url));

export const registerSourcePath = join(__dirname, "registerSource.mjs");
export const frameworkName = "vue";
export const vitePlugin = () =>
  import("@vitejs/plugin-vue").then((m) => m.default());
