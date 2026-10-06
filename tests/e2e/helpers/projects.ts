import { test } from '@ferridriver/test';

// A test that launches its own browsers does the same work whichever
// project runs it, so it runs on one: cdp-pipe, which both CI operating
// systems run. Without projects (one configured browser) it always runs.
export function runOnceAcrossProjects(): void {
  const project = test.info().project?.name;
  test.skip(project !== undefined && project !== 'cdp-pipe', 'launches its own browsers; runs on cdp-pipe');
}
