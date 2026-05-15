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

  // Video filter for exact dimensions
  args.extend(["-vf".into(), format!("pad={w}:{h}:0:0:gray,crop={w}:{h}:0:0")]);

  // Output file
  args.push(output_path.to_string_lossy().into_owned());

  Command::new(ffmpeg)
    .args(&args)
    .stdin(Stdio::piped())
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| FerriError::backend(format!("failed to spawn ffmpeg: {e}")))
}

/// Encode a 1-second white video at the requested `width x height` with no
/// piped input. Used as the fallback when the screencast produced zero
/// frames so the output file still exists, has the right dimensions,
/// and matches Playwright's `_stop` whitespace-frame behaviour.
fn encode_blank(output_path: &Path, width: u32, height: u32, fps: u32) -> Result<()> {
  let ffmpeg = find_ffmpeg()?;
  let w = width & !1;
  let h = height & !1;
  let mut args: Vec<String> = vec![
    "-loglevel".into(),
    "error".into(),
    "-y".into(),
    "-f".into(),
    "lavfi".into(),
    "-i".into(),
    format!("color=c=white:s={w}x{h}:d=1:r={fps}"),
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

/// Channel-driven encoding: pipe JPEG frames to ffmpeg subprocess as they arrive.
/// Runs on a blocking thread concurrently with the test.
///
/// # Errors
///
/// Returns an error if ffmpeg cannot be spawned or exits with an error.
pub fn encode_stream(
  mut rx: tokio::sync::mpsc::Receiver<(Vec<u8>, f64)>,
  output_path: &Path,
  width: u32,
  height: u32,
  fps: u32,
) -> Result<()> {
  // Wait for the first frame before spawning the image2pipe encoder.
  // If the channel closes empty (short recording on a page that never
  // produced a compositor frame, e.g. data: URL closed immediately
  // after navigation), ffmpeg's image2pipe input would exit with
  // "Output file does not contain any stream". Playwright handles
  // this in `videoRecorder.ts::_stop` by writing a synthesised
  // white frame before closing stdin; we do the equivalent through
  // ffmpeg's `lavfi color=` source which produces a 1-second
  // constant-colour clip at the requested dimensions -- no extra
  // dependency, no fake bitmap.
  let Some(first_frame) = rx.blocking_recv() else {
    return encode_blank(output_path, width, height, fps);
  };

  let mut child = spawn_ffmpeg(output_path, width, height, fps)?;
  let mut stdin = child
    .stdin
    .take()
    .ok_or_else(|| FerriError::backend("failed to open ffmpeg stdin"))?;

  let mut first_ts: Option<f64> = None;
  let mut last_frame: Option<Vec<u8>> = None;
  let mut last_frame_number: i64 = -1;

  // Re-enter the same processing loop with the first frame already in hand.
  let mut next = Some(first_frame);
  while let Some((jpeg_bytes, ts)) = next.take().or_else(|| rx.blocking_recv()) {
    let first = *first_ts.get_or_insert(ts);
    let frame_number = f64_to_i64(((ts - first) * f64::from(fps)).floor());

    // Gap fill: repeat last frame to maintain framerate
    if let Some(ref prev) = last_frame {
      for _ in (last_frame_number + 1)..frame_number {
        if stdin.write_all(prev).is_err() {
          break;
        }
      }
    }

    if stdin.write_all(&jpeg_bytes).is_err() {
      break;
    }
    last_frame = Some(jpeg_bytes);
    last_frame_number = frame_number;
  }

  // Trailing pad: 1 second of last frame for convenience
  if let Some(ref frame) = last_frame {
    for _ in 0..fps {
      if stdin.write_all(frame).is_err() {
        break;
      }
    }
  }

  // Close stdin to signal EOF, then wait for ffmpeg to finish
  drop(stdin);
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

/// Batch encode: pipe all frames at once (for deferred/buffered recording).
///
/// # Errors
///
/// Returns an error if ffmpeg cannot be spawned or exits with an error.
pub fn encode_frames(frames: &[(Vec<u8>, f64)], output_path: &Path, width: u32, height: u32, fps: u32) -> Result<()> {
  let mut child = spawn_ffmpeg(output_path, width, height, fps)?;
  let mut stdin = child
    .stdin
    .take()
    .ok_or_else(|| FerriError::backend("failed to open ffmpeg stdin"))?;

  let mut first_ts: Option<f64> = None;
  let mut last_frame: Option<&[u8]> = None;
  let mut last_frame_number: i64 = -1;

  for (jpeg_bytes, ts) in frames {
    let first = *first_ts.get_or_insert(*ts);
    let frame_number = f64_to_i64(((ts - first) * f64::from(fps)).floor());

    // Gap fill
    if let Some(prev) = last_frame {
      for _ in (last_frame_number + 1)..frame_number {
        if stdin.write_all(prev).is_err() {
          break;
        }
      }
    }

    if stdin.write_all(jpeg_bytes).is_err() {
      break;
    }
    last_frame = Some(jpeg_bytes);
    last_frame_number = frame_number;
  }

  // Trailing pad
  if let Some(frame) = last_frame {
    for _ in 0..fps {
      if stdin.write_all(frame).is_err() {
        break;
      }
    }
  }

  drop(stdin);
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
