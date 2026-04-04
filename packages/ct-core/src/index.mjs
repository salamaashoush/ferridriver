/**
 * @ferridriver/ct-core — main entry
 *
 * Re-exports the public API for component testing infrastructure.
 */

export { mount, unmount, update, wrapObject, createComponent } from "./mount.mjs";
export { createCtRunner } from "./runner.mjs";
export { ferridriverCtPlugin } from "./vitePlugin.mjs";
export { transformTestFile, scanTestFiles } from "./importTransform.mjs";
