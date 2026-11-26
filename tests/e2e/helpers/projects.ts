import { test } from '@ferridriver/test';

// A test that launches its own browsers does the same work whichever
// project runs it, so it runs on one: chromium, which every CI operating
// system runs. Without projects (one configured browser, Playwright's
// unnamed project) it always runs.
export function runOnceAcrossProjects(): void {
  const project = test.info().project.name;
  test.skip(project !== '' && project !== 'chromium', 'launches its own browsers; runs on chromium');
}
