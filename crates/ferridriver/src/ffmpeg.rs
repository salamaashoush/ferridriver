//! Video encoding via ffmpeg subprocess (matches Playwright's approach).
//!
//! Spawns `ffmpeg` CLI and pipes JPEG frames to stdin. No compile-time
//! ffmpeg/libav dependency -- the binary just needs `ffmpeg` on PATH at
//! runtime when `--video` is used.
//!
//! Codec selection matches Playwright: VP8/WebM by default, fallback to
//! libx264/MP4 if VP8 is unavailable.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;

use crate::error::{FerriError, Result};

/// Safely convert f64 to i64 via string formatting (avoids clippy `cast_possible_truncation`).
fn f64_to_i64(v: f64) -> i64 {
  if !v.is_finite() {
    return 0;
  }
  format!("{v:.0}").parse::<i64>().unwrap_or(0)
}

/// Detect which encoder ffmpeg supports. Cached after first call.
fn detect_encoder() -> &'static str {
  static ENCODER: OnceLock<&'static str> = OnceLock::new();
  ENCODER.get_or_init(|| {
    // Check if ffmpeg supports VP8 (libvpx)
    let has_vpx = Command::new("ffmpeg")
      .args(["-hide_banner", "-encoders"])
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .output()
      .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("libvpx"));
    if has_vpx { "vpx" } else { "h264" }
  })
}

/// Return the correct file extension based on available encoder.
#[must_use]
pub fn video_extension() -> &'static str {
  if detect_encoder() == "vpx" { "webm" } else { "mp4" }
}

/// Return the correct MIME type based on available encoder.
#[must_use]
pub fn video_content_type() -> &'static str {
  if detect_encoder() == "vpx" {
    "video/webm"
  } else {
    "video/mp4"
  }
}

/// Find the ffmpeg binary path.
///
/// # Errors
///
/// Returns an error if ffmpeg is not found on PATH.
fn find_ffmpeg() -> Result<&'static str> {
  static FFMPEG: OnceLock<Result<&'static str>> = OnceLock::new();
  FFMPEG
    .get_or_init(|| {
      // Check if ffmpeg is available
      match Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
      {
        Ok(s) if s.success() => Ok("ffmpeg"),
        _ => Err(FerriError::Unsupported(
          "ffmpeg not found. Install ffmpeg to enable video recording:\n  \
           macOS:  brew install ffmpeg\n  \
           Linux:  apt install ffmpeg\n  \
           Windows: winget install ffmpeg"
            .into(),
        )),
      }
    })
    .clone()
}

/// Spawn an ffmpeg process that reads JPEG frames from stdin and writes video.
/// Uses the same codec settings as Playwright.
fn spawn_ffmpeg(output_path: &Path, width: u32, height: u32, fps: u32) -> Result<Child> {
  let ffmpeg = find_ffmpeg()?;
  let w = width & !1;
  let h = height & !1;

  // Build args matching Playwright's FfmpegVideoRecorder:
  //   -f image2pipe -c:v mjpeg -i pipe:0    -- read JPEG frames from stdin
  //   -avioflags direct                      -- reduce buffering
  //   -fpsprobesize 0 -probesize 32          -- reduce initial analysis delay
  //   -analyzeduration 0                     -- skip stream analysis
  //   -r {fps}                               -- output framerate
  //   -vf pad=W:H:0:0:gray,crop=W:H:0:0     -- resize to exact dimensions
  //   -y -an                                 -- overwrite, no audio
  //   -threads 1                             -- reduce CPU contention
  let mut args: Vec<String> = vec![
    "-loglevel".into(),
    "error".into(),
    "-f".into(),
    "image2pipe".into(),
    "-avioflags".into(),
    "direct".into(),
    "-fpsprobesize".into(),
    "0".into(),
    "-probesize".into(),
    "32".into(),
    "-analyzeduration".into(),
    "0".into(),
    "-c:v".into(),
    "mjpeg".into(),
    "-i".into(),
    "pipe:0".into(),
    "-y".into(),
    "-an".into(),
    "-r".into(),
    fps.to_string(),
  ];

  // Codec-specific settings (matching Playwright)
  if detect_encoder() == "vpx" {
    args.extend(
      [
        "-c:v",
        "vp8",
        "-qmin",
        "0",
        "-qmax",
        "50",
        "-crf",
        "8",
        "-deadline",
        "realtime",
        "-speed",
        "8",
        "-b:v",
        "1M",
        "-threads",
        "1",
      ]
      .map(String::from),
    );
  } else {
    args.extend(
      [
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "23",
        "-tune",
        "fastdecode",
        "-threads",
        "1",
      ]
      .map(String::from),
    );
  }

  // A screencast shared with another consumer can run larger than this
  // recording asked for, so frames that do not fit are scaled down rather
  // than cropped.
  args.extend([
    "-vf".into(),
    format!(
      "scale='min(iw,{w})':'min(ih,{h})':force_original_aspect_ratio=decrease,pad={w}:{h}:0:0:gray,crop={w}:{h}:0:0"
    ),
  ]);

  // Output file
  args.push(output_path.to_string_lossy().into_owned());

  crate::backend::process::spawn_exclusive(|| {
    Command::new(ffmpeg)
      .args(&args)
      .stdin(Stdio::piped())
      .stdout(Stdio::null())
      .stderr(Stdio::piped())
      .spawn()
  })
  .map_err(|e| FerriError::backend(format!("failed to spawn ffmpeg: {e}")))
}

/// Encode a white video of `duration` seconds at the requested
/// `width x height` with no piped input. Used as the fallback when the
/// screencast produced zero frames so the output file still exists, has
/// the right dimensions, and matches Playwright's `_stop` whitespace-frame
/// behaviour.
fn encode_blank(output_path: &Path, width: u32, height: u32, fps: u32, duration: f64) -> Result<()> {
  let ffmpeg = find_ffmpeg()?;
  let w = width & !1;
  let h = height & !1;
  let duration = duration.max(1.0 / f64::from(fps));
  let mut args: Vec<String> = vec![
    "-loglevel".into(),
    "error".into(),
    "-y".into(),
    "-f".into(),
    "lavfi".into(),
    "-i".into(),
    format!("color=c=white:s={w}x{h}:d={duration:.3}:r={fps}"),
    "-an".into(),
  ];
  if detect_encoder() == "vpx" {
    args.extend(
      [
        "-c:v",
        "vp8",
        "-qmin",
        "0",
        "-qmax",
        "50",
        "-crf",
        "8",
        "-deadline",
        "realtime",
        "-speed",
        "8",
        "-b:v",
        "1M",
        "-threads",
        "1",
      ]
      .map(String::from),
    );
  } else {
    args.extend(["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-threads", "1"].map(String::from));
  }
  args.push(output_path.to_string_lossy().into_owned());
  let output = Command::new(ffmpeg)
    .args(&args)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .output()
    .map_err(|e| FerriError::backend(format!("failed to spawn ffmpeg (blank): {e}")))?;
  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Err(FerriError::backend(format!(
      "ffmpeg blank-encode exited with {}: {stderr}",
      output.status
    )));
  }
  Ok(())
}

/// What the recorder feeds the encoder: frames stamped with seconds since
/// the recording started, then the time the recording stopped.
pub(crate) enum EncoderInput {
  Frame { jpeg: Vec<u8>, at: f64 },
  End { at: f64 },
}

/// Maps timestamped frames onto a constant-rate timeline that starts at
/// the recording's start and ends at its stop, so the encoded video's
/// time is wall time. Each output slot holds the newest frame received
/// by then; the first frame also fills the slots before it arrived and
/// the last one fills the slots up to the stop (an idle page sends no
/// frames).
struct FramePacer {
  fps: f64,
  pending: Option<(Vec<u8>, i64)>,
}

impl FramePacer {
  fn new(fps: u32) -> Self {
    Self {
      fps: f64::from(fps),
      pending: None,
    }
  }

  fn slot(&self, at: f64) -> i64 {
    f64_to_i64((at * self.fps).floor()).max(0)
  }

  fn push(
    &mut self,
    jpeg: Vec<u8>,
    at: f64,
    write: &mut impl FnMut(&[u8]) -> std::io::Result<()>,
  ) -> std::io::Result<()> {
    let slot = self.slot(at);
    self.pending = Some(match self.pending.take() {
      None => (jpeg, 0),
      Some((prev, prev_slot)) if slot > prev_slot => {
        for _ in prev_slot..slot {
          write(&prev)?;
        }
        (jpeg, slot)
      },
      Some((_, prev_slot)) => (jpeg, prev_slot),
    });
    Ok(())
  }

  fn finish(self, end: f64, write: &mut impl FnMut(&[u8]) -> std::io::Result<()>) -> std::io::Result<()> {
    if let Some((last, last_slot)) = self.pending {
      let total = f64_to_i64((end * self.fps).round()).max(last_slot + 1);
      for _ in last_slot..total {
        write(&last)?;
      }
    }
    Ok(())
  }
}

fn finish_ffmpeg(child: Child) -> Result<()> {
  let output = child
    .wait_with_output()
    .map_err(|e| FerriError::backend(format!("ffmpeg wait: {e}")))?;
  if !output.status.success() {
    let stderr = String::from_utf8_lossy(&output.stderr);
    return Err(FerriError::backend(format!(
      "ffmpeg exited with {}: {stderr}",
      output.status
    )));
  }
  Ok(())
}

/// Channel-driven encoding: pipe JPEG frames to ffmpeg subprocess as they arrive.
/// Runs on a blocking thread concurrently with the test.
///
/// # Errors
///
/// Returns an error if ffmpeg cannot be spawned or exits with an error.
pub(crate) fn encode_stream(
  mut rx: tokio::sync::mpsc::Receiver<EncoderInput>,
  output_path: &Path,
  width: u32,
  height: u32,
  fps: u32,
) -> Result<()> {
  // Wait for the first frame before spawning the image2pipe encoder.
  // If no frame ever arrives (short recording on a page that never
  // produced a compositor frame, e.g. data: URL closed immediately
  // after navigation), ffmpeg's image2pipe input would exit with
  // "Output file does not contain any stream". Playwright handles
  // this in `videoRecorder.ts::_stop` by writing a synthesised
  // white frame before closing stdin; we do the equivalent through
  // ffmpeg's `lavfi color=` source at the requested dimensions -- no
  // extra dependency, no fake bitmap.
  let (first, first_at) = match rx.blocking_recv() {
    Some(EncoderInput::Frame { jpeg, at }) => (jpeg, at),
    Some(EncoderInput::End { at }) => return encode_blank(output_path, width, height, fps, at),
    None => return encode_blank(output_path, width, height, fps, 1.0),
  };

  let mut child = spawn_ffmpeg(output_path, width, height, fps)?;
  let mut stdin = child
    .stdin
    .take()
    .ok_or_else(|| FerriError::backend("failed to open ffmpeg stdin"))?;
  let mut write = |frame: &[u8]| stdin.write_all(frame);

  let mut pacer = FramePacer::new(fps);
  let mut end = None;
  let mut written = pacer.push(first, first_at, &mut write);
  while written.is_ok() {
    match rx.blocking_recv() {
      Some(EncoderInput::Frame { jpeg, at }) => written = pacer.push(jpeg, at, &mut write),
      Some(EncoderInput::End { at }) => {
        end = Some(at);
        break;
      },
      None => break,
    }
  }
  if written.is_ok() {
    // Without a stop time the recording was abandoned; close on the last frame.
    let _ = pacer.finish(end.unwrap_or(0.0), &mut write);
  }

  // Close stdin to signal EOF, then wait for ffmpeg to finish
  drop(stdin);
  finish_ffmpeg(child)
}

/// Batch encode buffered `(jpeg, seconds since start)` frames into a video
/// that ends `end` seconds after the start.
///
/// # Errors
///
/// Returns an error if ffmpeg cannot be spawned or exits with an error.
pub(crate) fn encode_frames(
  frames: Vec<(Vec<u8>, f64)>,
  end: f64,
  output_path: &Path,
  width: u32,
  height: u32,
  fps: u32,
) -> Result<()> {
  let mut child = spawn_ffmpeg(output_path, width, height, fps)?;
  let mut stdin = child
    .stdin
    .take()
    .ok_or_else(|| FerriError::backend("failed to open ffmpeg stdin"))?;
  let mut write = |frame: &[u8]| stdin.write_all(frame);

  let mut pacer = FramePacer::new(fps);
  let written = frames
    .into_iter()
    .try_for_each(|(jpeg, at)| pacer.push(jpeg, at, &mut write));
  if written.is_ok() {
    let _ = pacer.finish(end, &mut write);
  }

  drop(stdin);
  finish_ffmpeg(child)
}

#[cfg(test)]
mod tests {
  use super::FramePacer;

  fn pace(frames: &[(u8, f64)], end: f64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut write = |frame: &[u8]| {
      out.push(frame[0]);
      Ok(())
    };
    let mut pacer = FramePacer::new(25);
    for &(id, at) in frames {
      pacer.push(vec![id], at, &mut write).unwrap();
    }
    pacer.finish(end, &mut write).unwrap();
    out
  }

  #[test]
  fn the_video_spans_the_recording_wall_time() {
    // One frame early on, then 5 s of an unchanged page.
    let out = pace(&[(1, 0.1), (2, 0.5)], 5.72);
    assert_eq!(out.len(), 143, "5.72 s at 25 fps");
    assert!(out[..12].iter().all(|&f| f == 1));
    assert!(out[12..].iter().all(|&f| f == 2));
  }

  #[test]
  fn the_first_frame_covers_the_time_before_it_arrived() {
    let out = pace(&[(1, 0.4)], 1.0);
    assert_eq!(out, vec![1; 25]);
  }

  #[test]
  fn frames_sharing_a_slot_keep_the_newest() {
    // 60 fps input must not stretch a 25 fps timeline.
    let frames: Vec<(u8, f64)> = (0..60u8).map(|i| (i, f64::from(i) / 60.0)).collect();
    let out = pace(&frames, 1.0);
    assert_eq!(out.len(), 25);
    assert_eq!(out[0], 2, "slot 0 shows the newest frame received in it");
    assert_eq!(out[24], 59);
  }

  #[test]
  fn a_stop_before_the_last_frame_still_shows_it_once() {
    let out = pace(&[(1, 0.0), (2, 1.0)], 0.9);
    assert_eq!(out.len(), 26);
    assert_eq!(out[25], 2);
  }
}
