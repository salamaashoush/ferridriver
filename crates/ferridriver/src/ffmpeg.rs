//! Video encoding through an ffmpeg subprocess, as Playwright's
//! `FfmpegVideoRecorder` does it: each JPEG frame goes down stdin once,
//! wrapped in Matroska with its own timestamp ([`ebml`]), and ffmpeg holds
//! frames for as long as their timestamps say. No compile-time ffmpeg/libav
//! dependency: the binary is `FERRIDRIVER_FFMPEG`, the one `ferridriver
//! install ffmpeg` fetched, Playwright's, or `ffmpeg` on PATH.
//!
//! VP9 into `WebM` with Playwright's encoder settings, falling back to
//! libx264 into MP4 for an ffmpeg without a VP9 encoder.

mod ebml;

use std::io::Write;
use std::path::{Path, PathBuf};
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
    // Playwright's own ffmpeg carries libvpx-vp9 and no VP8.
    let Ok(ffmpeg) = find_ffmpeg() else { return "h264" };
    let has_vpx = Command::new(ffmpeg)
      .args(["-hide_banner", "-encoders"])
      .stdout(Stdio::piped())
      .stderr(Stdio::null())
      .output()
      .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("libvpx-vp9"));
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

/// The ffmpeg that records video, in the order `WebKit`'s launcher looks
/// for its browser: `FERRIDRIVER_FFMPEG`, ferridriver's cache, Playwright's
/// cache, then `ffmpeg` on PATH.
///
/// # Errors
///
/// Returns an error if none of them runs.
fn find_ffmpeg() -> Result<&'static Path> {
  static FFMPEG: OnceLock<Result<PathBuf>> = OnceLock::new();
  FFMPEG
    .get_or_init(|| {
      std::env::var_os("FERRIDRIVER_FFMPEG")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .or_else(|| crate::install::BrowserInstaller::new().find_installed_ffmpeg().map(PathBuf::from))
        .or_else(|| crate::install::find_playwright_browser(crate::install::CachedBrowser::Ffmpeg))
        .or_else(|| {
          let on_path = PathBuf::from("ffmpeg");
          Command::new(&on_path)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
            .then_some(on_path)
        })
        .ok_or_else(|| {
          FerriError::Unsupported(
            "ffmpeg not found. Install it to record video: ferridriver install ffmpeg (or put ffmpeg on PATH)".into(),
          )
        })
    })
    .as_ref()
    .map(PathBuf::as_path)
    .map_err(Clone::clone)
}

/// Playwright's VP9 settings (`videoRecorder.ts`): realtime encoding with
/// no lookahead, and a bitrate and thread count that grow with the pixel
/// rate over the 800x450 at 25 fps its 1 Mbit/s budget was tuned for, so a
/// large or fast recording stays sharp and keeps up.
fn vp9_args(width: u32, height: u32, fps: u32) -> Vec<String> {
  let pixel_rate = f64::from(width) * f64::from(height) * f64::from(fps);
  let scale = (pixel_rate / (800.0 * 450.0 * 25.0)).max(1.0);
  let bitrate = f64_to_i64(scale * 1000.0);
  let threads = f64_to_i64((scale / 4.0).ceil()).clamp(1, 8);
  let tile_columns = match threads {
    1 => 0,
    2 => 1,
    3 | 4 => 2,
    _ => 3,
  };
  [
    "-c:v",
    "libvpx-vp9",
    "-qmin",
    "0",
    "-qmax",
    "50",
    "-crf",
    "8",
    "-deadline",
    "realtime",
    "-cpu-used",
    "8",
    "-lag-in-frames",
    "0",
  ]
  .into_iter()
  .map(String::from)
  .chain([
    "-b:v".to_string(),
    format!("{bitrate}k"),
    "-threads".to_string(),
    threads.to_string(),
    "-tile-columns".to_string(),
    tile_columns.to_string(),
    "-row-mt".to_string(),
    "1".to_string(),
  ])
  .collect()
}

/// Spawn an ffmpeg process that reads JPEG frames from stdin and writes video.
/// Uses the same codec settings as Playwright.
fn spawn_ffmpeg(output_path: &Path, width: u32, height: u32, fps: u32) -> Result<Child> {
  let ffmpeg = find_ffmpeg()?;
  let w = width & !1;
  let h = height & !1;

  // Playwright's FfmpegVideoRecorder input:
  //   -f matroska -i pipe:0                  -- timestamped MJPEG frames on stdin
  //   -fpsprobesize 0 -probesize 32
  //   -analyzeduration 0                     -- start without buffering to analyse
  //   -r {fps}                               -- constant output rate; ffmpeg holds
  //                                             each frame until the next one's time
  //   -y -an                                 -- overwrite, no audio
  // `-avioflags direct` would break Matroska parsing, which needs the
  // input buffering it turns off.
  let mut args: Vec<String> = vec![
    "-loglevel".into(),
    "error".into(),
    "-f".into(),
    "matroska".into(),
    "-fpsprobesize".into(),
    "0".into(),
    "-probesize".into(),
    "32".into(),
    "-analyzeduration".into(),
    "0".into(),
    "-i".into(),
    "pipe:0".into(),
    "-y".into(),
    "-an".into(),
    "-r".into(),
    fps.to_string(),
  ];

  // Codec-specific settings (matching Playwright)
  if detect_encoder() == "vpx" {
    args.extend(vp9_args(w, h, fps));
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

  args.extend([
    "-metadata".into(),
    format!("creation_time={}", crate::tracing::now_iso8601()),
  ]);
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

/// Encode a recording the screencast never sent a frame to, which ffmpeg
/// would otherwise refuse to write: Playwright's `_stop` writes
/// `createWhiteImage`, a white JPEG at quality 80, at the stop time, so
/// the file still exists at the right size.
fn encode_blank(output_path: &Path, width: u32, height: u32, fps: u32, stop: f64) -> Result<()> {
  let white = image::RgbImage::from_pixel(width.max(1), height.max(1), image::Rgb([255, 255, 255]));
  let mut jpeg = Vec::new();
  image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80)
    .encode_image(&white)
    .map_err(|e| FerriError::backend(format!("white frame: {e}")))?;
  encode_frames(vec![(jpeg, stop)], stop, output_path, width, height, fps)
}

/// What the recorder feeds the encoder: frames stamped with seconds since
/// the recording started, then the time the recording stopped.
pub(crate) enum EncoderInput {
  Frame { jpeg: Vec<u8>, at: f64 },
  End { at: f64 },
}

/// The write side of Playwright's recorder: the Matroska header once, then
/// each frame once in its own timestamped cluster. ffmpeg's `-r` holds a
/// frame until the next one's time, so an idle page costs nothing here.
struct FrameWriter<W: Write> {
  out: W,
  last: Option<(Vec<u8>, f64)>,
}

impl<W: Write> FrameWriter<W> {
  fn new(mut out: W) -> std::io::Result<Self> {
    out.write_all(&ebml::header())?;
    Ok(Self { out, last: None })
  }

  /// A frame `at` seconds after the recording started.
  fn frame(&mut self, jpeg: Vec<u8>, at: f64) -> std::io::Result<()> {
    self.emit(&jpeg, at)?;
    self.last = Some((jpeg, at));
    Ok(())
  }

  fn emit(&mut self, jpeg: &[u8], at: f64) -> std::io::Result<()> {
    let ms = u64::try_from(f64_to_i64((at * 1000.0).round())).unwrap_or(0);
    self.out.write_all(&ebml::cluster_header(ms, jpeg.len()))?;
    self.out.write_all(jpeg)
  }

  /// The last frame once more, at the stop or a second after it arrived,
  /// whichever is later (Playwright's `_stop`): a frame is shown only
  /// until the next one's time, so the final one needs a successor to be
  /// shown at all, and the page's final state stays on screen.
  fn finish(mut self, stop: Option<f64>) -> std::io::Result<W> {
    if let Some((jpeg, at)) = self.last.take() {
      let held = stop.map_or(1.0, |stop| (stop - at).max(1.0));
      self.emit(&jpeg, at + held)?;
    }
    Ok(self.out)
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
  // Wait for the first frame before spawning the encoder. If none ever
  // arrives (a page that never produced a compositor frame, e.g. a data:
  // URL closed right after navigation), ffmpeg would exit with "Output
  // file does not contain any stream". Playwright's `_stop` writes a white
  // frame then, and so does `encode_blank`.
  let (first, first_at) = match rx.blocking_recv() {
    Some(EncoderInput::Frame { jpeg, at }) => (jpeg, at),
    Some(EncoderInput::End { at }) => return encode_blank(output_path, width, height, fps, at),
    None => return encode_blank(output_path, width, height, fps, 1.0),
  };

  let mut child = spawn_ffmpeg(output_path, width, height, fps)?;
  let stdin = child
    .stdin
    .take()
    .ok_or_else(|| FerriError::backend("failed to open ffmpeg stdin"))?;

  // A write fails only when ffmpeg has gone, and its exit status says why.
  if let Ok(mut writer) = FrameWriter::new(stdin) {
    let mut stop = None;
    let mut written = writer.frame(first, first_at);
    while written.is_ok() {
      match rx.blocking_recv() {
        Some(EncoderInput::Frame { jpeg, at }) => written = writer.frame(jpeg, at),
        Some(EncoderInput::End { at }) => {
          stop = Some(at);
          break;
        },
        None => break,
      }
    }
    if written.is_ok() {
      // Closing stdin is what tells ffmpeg the recording ended.
      drop(writer.finish(stop));
    }
  }
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
  let stdin = child
    .stdin
    .take()
    .ok_or_else(|| FerriError::backend("failed to open ffmpeg stdin"))?;
  if let Ok(mut writer) = FrameWriter::new(stdin) {
    let written = frames.into_iter().try_for_each(|(jpeg, at)| writer.frame(jpeg, at));
    if written.is_ok() {
      drop(writer.finish(Some(end)));
    }
  }
  finish_ffmpeg(child)
}

#[cfg(test)]
mod tests {
  use super::{FrameWriter, ebml};

  /// The `(timestamp ms, frame)` clusters a writer produced, read back.
  fn clusters(stream: &[u8]) -> Vec<(u64, Vec<u8>)> {
    let header = ebml::header();
    assert!(stream.starts_with(&header));
    let mut rest = &stream[header.len()..];
    let mut out = Vec::new();
    while !rest.is_empty() {
      // Cluster id (4), size vint (1-2), E7 + size + timestamp, A3 + size + 4-byte head, frame.
      assert_eq!(&rest[..4], &[0x1F, 0x43, 0xB6, 0x75]);
      let size_len = if rest[4] & 0x80 != 0 { 1 } else { 2 };
      let mut at = 4 + size_len;
      assert_eq!(rest[at], 0xE7);
      let ts_len = usize::from(rest[at + 1] & 0x7F);
      let ts = rest[at + 2..at + 2 + ts_len]
        .iter()
        .fold(0u64, |acc, byte| (acc << 8) | u64::from(*byte));
      at += 2 + ts_len;
      assert_eq!(rest[at], 0xA3);
      let (block_len, vint_len) = if rest[at + 1] & 0x80 != 0 {
        (usize::from(rest[at + 1] & 0x7F), 1)
      } else {
        ((usize::from(rest[at + 1] & 0x3F) << 8) | usize::from(rest[at + 2]), 2)
      };
      let frame_start = at + 1 + vint_len + 4;
      let frame_end = at + 1 + vint_len + block_len;
      out.push((ts, rest[frame_start..frame_end].to_vec()));
      rest = &rest[frame_end..];
    }
    out
  }

  #[test]
  fn each_frame_is_written_once_with_its_time() {
    let mut writer = FrameWriter::new(Vec::new()).unwrap();
    writer.frame(vec![1; 3], 0.1).unwrap();
    writer.frame(vec![2; 200], 0.5).unwrap();
    let stream = writer.finish(Some(5.72)).unwrap();
    // The last frame is repeated at the stop, so it is held until then.
    assert_eq!(
      clusters(&stream),
      vec![(100, vec![1; 3]), (500, vec![2; 200]), (5_720, vec![2; 200])]
    );
  }

  #[test]
  fn the_last_frame_is_held_for_at_least_a_second() {
    let mut writer = FrameWriter::new(Vec::new()).unwrap();
    writer.frame(vec![7], 2.0).unwrap();
    let stream = writer.finish(Some(2.25)).unwrap();
    assert_eq!(clusters(&stream), vec![(2_000, vec![7]), (3_000, vec![7])]);
    // A recording abandoned without a stop time is held the same second.
    let mut writer = FrameWriter::new(Vec::new()).unwrap();
    writer.frame(vec![7], 0.0).unwrap();
    assert_eq!(clusters(&writer.finish(None).unwrap()), vec![(0, vec![7]), (1_000, vec![7])]);
  }
}
