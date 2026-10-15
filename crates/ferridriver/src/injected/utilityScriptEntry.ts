import {UtilityScript} from './utilityScript';

// A worker has no window and evaluates through this same utility script.
// Where there is a window it stays the home: in a Firefox sandbox
// `globalThis` is the sandbox's own global, not the window callers read.
const world = (typeof window === 'undefined' ? globalThis : window) as typeof globalThis & {
  __fd?: {newUtilityScript: () => UtilityScript};
};
world.__fd ??= {newUtilityScript: () => new UtilityScript(world, false)};
