import {UtilityScript} from './utilityScript';

const world = window as typeof window & {__fd?: {newUtilityScript: () => UtilityScript}};
world.__fd ??= {newUtilityScript: () => new UtilityScript(window, false)};
