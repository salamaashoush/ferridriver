// Ported from crates/ferridriver-cli/tests/backends_support/video.rs —
// Video as a first-class handle via page.video() (types.d.ts:21621).
// CDP records via the native screencast, BiDi via the poll-based
// polyfill, WebKit via Screencast.startScreencast frames. Test titles
// mirror the original Rust fn names.

import { test, describe, expect } from '@ferridriver/test';

describe('video', () => {
  test('video_null_without_recording', async ({ page }) => {
    expect(page.video()).toBeNull();
  });

  test('video_recording_lifecycle', async ({ context, browserName }) => {
    // setRecordVideo -> context.newPage -> navigate -> close ->
    // video.path(). 1280x720 covers Firefox's polyfill output without
    // triggering ffmpeg's pad-smaller-than-input error (the polyfill
    // captures at Firefox's rendered viewport size).
    test.slow();
    const recordDir = test.info().outputPath('video-recordings');
    await context.setRecordVideo({ dir: recordDir, size: { width: 1280, height: 720 } });
    const recPage = await context.newPage();
    // Two navigations give the encoder a visible state transition; the
    // timer pad lets the screencast pump flush a trailing frame
    // deterministically rather than racing goto timing.
    await recPage.goto('data:text/html,<h1>rec-1</h1>');
    await recPage.goto('data:text/html,<h1>rec-2</h1>');
    await new Promise((r) => setTimeout(r, 250));
    const video = recPage.video();
    expect(video).not.toBeNull();
    await recPage.close();
    const filePath = await video!.path();
    expect(filePath.includes('video-recordings')).toBe(true);
    expect(fs.existsSync(filePath)).toBe(true);
    if (browserName === 'chromium') {
      // CDP recordings must be non-empty; a fast-close polyfill file on
      // Firefox can legitimately be tiny.
      const bytes = await fs.promises.readFile(filePath);
      expect(bytes.length).toBeGreaterThan(0);
    }
  });
});
