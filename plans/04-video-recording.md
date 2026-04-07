# [DONE] Feature: Video Recording

## Context
Video recordings of test execution are invaluable for debugging failures, especially in CI where you cannot reproduce interactively. Playwright's video feature captures the entire test as a video file and attaches it to the report. This is a high-value feature for CI debugging and bug reports.

## Design

### Architecture
| Crate/Package | Role |
|---|---|
| `ferridriver` | CDP `Page.startScreencast` / `Page.stopScreencast`, frame capture API |
| `ferridriver-test` | Video lifecycle management (start/stop per test), config, artifact attachment |
| `ferridriver-bdd` | Auto-record per scenario |
| `ferridriver-cli` | `--video` flag |
| `packages/ferridriver-test` | `video` config option |

### Core Changes (ferridriver)
- New module `crates/ferridriver/src/video.rs`:
  - `VideoRecorder` struct: manages CDP `Page.startScreencast` subscription.
  - Receives JPEG/PNG frames via CDP events, writes to frame buffer.
  - `stop() -> PathBuf`: stops capture, encodes frames to WebM using `webm-writer` crate (pure Rust, no ffmpeg dependency).
  - Frame rate: configurable, default 25fps. Capture rate adapts to actual frame events.
  - `VideoSize { width, height }`: configurable, default matches viewport.

### Core Changes (ferridriver-test)
- Add to `TestConfig`:
  ```rust
  pub video: VideoMode,       // Off | On | RetainOnFailure
  pub video_size: Option<VideoSize>,
  ```
- `VideoMode` enum: `Off`, `On`, `RetainOnFailure`.
- In `Worker::run_test()`:
  - Before test: if video != Off, call `page.start_screencast()` -> `VideoRecorder`.
  - After test: call `recorder.stop()` -> video file path.
  - If `RetainOnFailure` and test passed: delete the video file.
  - If retained: attach as `TestInfo::attach("video", "video/webm", path)`.
- Video output path: `{test_output_dir}/{test-name}-{retry}.webm`.

### BDD Integration (ferridriver-bdd)
- BDD worker applies the same video logic per scenario.
- Video attached to scenario result, shown in HTML/JSON report.
- No BDD-specific config; uses `TestConfig::video`.

### NAPI + TypeScript (ferridriver-napi, packages/ferridriver-test)
- Config: `video: 'on' | 'off' | 'retain-on-failure'` in `ferridriver.config.ts`.
- NAPI maps string to `VideoMode` enum.

### CLI (ferridriver-cli)
- `--video <mode>` flag: `on`, `off`, `retain-on-failure`.
- Overrides config file setting.

### Component Testing (ferridriver-ct-*)
- Works identically — CT tests run in a real browser with CDP, so screencast is available.

## Implementation Steps
1. Implement CDP `Page.startScreencast` / `Page.stopScreencast` in `crates/ferridriver/src/page.rs`.
2. Create `crates/ferridriver/src/video.rs` — `VideoRecorder` with frame buffering and WebM encoding.
3. Add `webm-writer` (or `webm` crate) dependency to `ferridriver/Cargo.toml`.
4. Add `VideoMode`, `VideoSize` to `ferridriver-test/src/config.rs`.
5. Integrate video lifecycle into `Worker::run_test()` in `ferridriver-test/src/worker.rs`.
6. Attach video as `Attachment` to `TestInfo` on completion.
7. Update HTML reporter to embed `<video>` tags for video attachments.
8. Add `--video` flag to CLI.
9. Add NAPI config mapping.
10. Test with a simple navigation test, verify WebM file is produced and playable.

## Key Files
| File | Action |
|---|---|
| `crates/ferridriver/src/video.rs` | Create |
| `crates/ferridriver/src/page.rs` | Modify — add screencast API |
| `crates/ferridriver-test/src/config.rs` | Modify — add video config |
| `crates/ferridriver-test/src/worker.rs` | Modify — video lifecycle |
| `crates/ferridriver-test/src/reporter/html.rs` | Modify — embed video |
| `crates/ferridriver-cli/src/cli.rs` | Modify — `--video` flag |

## Verification
- Unit test: `VideoRecorder` encodes 10 test frames into a valid WebM file.
- Integration test: run a test with `video: 'on'`, verify `.webm` file exists in output dir.
- Integration test: `retain-on-failure` — passing test has no video, failing test has video.
- Manual: open WebM file in browser, verify it shows the test execution.
- HTML report: verify video is embedded and playable inline.
