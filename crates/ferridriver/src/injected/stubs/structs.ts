// Stub types for @protocol/structs — only what the vendored sources
// import. Upstream generates the real file from the protocol definition
// (packages/protocol/src/structs.d.ts); ferridriver speaks the browser
// protocols directly, so it carries just these shapes.
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

export type NameValue = { name: string; value: string };

export type IndexedDBDatabase = {
  name: string,
  version: number,
  stores: {
    name: string,
    autoIncrement: boolean,
    keyPath?: string,
    keyPathArray?: string[],
    records: {
      key?: any,
      keyEncoded?: any,
      value?: any,
      valueEncoded?: any,
    }[],
    indexes: {
      name: string,
      keyPath?: string,
      keyPathArray?: string[],
      multiEntry: boolean,
      unique: boolean,
    }[],
  }[],
};

export type OPFSEntry = {
  path: string,
  type: 'file' | 'directory',
  base64?: string,
};

export type SetOriginStorage = {
  origin: string,
  localStorage: NameValue[],
  indexedDB?: IndexedDBDatabase[],
  opfs?: OPFSEntry[],
};

export type OriginStorage = {
  origin: string,
  localStorage: NameValue[],
  indexedDB?: IndexedDBDatabase[],
  opfs?: OPFSEntry[],
};
