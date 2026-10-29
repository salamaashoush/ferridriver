/**
 * The globals of a plain script: a file `[test] scriptMatch` names, which
 * runs as one test whose body is the file's own top level. Reference this
 * file from a script, or list it in a tsconfig's `files`, to type them.
 */
import type { APIRequestContext, Browser, BrowserContext, Expect, Page, TestType } from './index';

declare global {
  /** The test's page. */
  const page: Page;
  /** The page's browser context. */
  const context: BrowserContext;
  const browser: Browser;
  /** An API request context of the test's own. */
  const request: APIRequestContext;
  const expect: Expect;
  /** The base `test`, for `test.step()` and `test.info()`. A script cannot declare tests. */
  const test: TestType;
}
