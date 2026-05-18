// Typed helper imported by the step file (proves .ts resolution +
// transpile). `deadCodeUnused` is exported but never imported — rolldown
// must tree-shake it (and its marker string) out of the bundle.

export const add = (a: number, b: number): number => a + b;

export function deadCodeUnused(): string {
  return "TREE_SHAKE_ME_AWAY_MARKER_9F3A";
}
