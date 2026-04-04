// Stub types for @protocol/channels — only types used by injectedScript.ts
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

export type FrameExpectParams = {
  selector: string;
  expression: string;
  expressionArg?: any;
  expectedText?: ExpectedTextValue[];
  expectedNumber?: number;
  expectedValue?: any;
  useInnerText?: boolean;
  isNot: boolean;
  timeout: number;
};
