// Stub types for @protocol/structs — only what injectedScript.ts imports.
// Upstream generates the real file from the protocol definition
// (packages/protocol/src/structs.d.ts); ferridriver speaks the browser
// protocols directly, so it carries just these three shapes.
export type Point = { x: number; y: number };
export type Rect = { x: number; y: number; width: number; height: number };

export type ExpectedTextValue = {
  string?: string;
  regexSource?: string;
  regexFlags?: string;
  matchSubstring?: boolean;
  ignoreCase?: boolean;
  normalizeWhiteSpace?: boolean;
};
