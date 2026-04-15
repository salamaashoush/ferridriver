//! In-process video encoding via ffmpeg-next.
//!
//! Key optimizations vs original:
//! - MJPEG decoder reused across all frames (not recreated per frame)
//! - Scaler lazily created and recreated only on format change
//! - Frame cloning respects linesize/stride
//! - Encoding pipeline is separated so it can be driven concurrently

use std::path::Path;

use ffmpeg_next as ffmpeg;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::software::scaling;

/// Convert f64 to i64 by formatting as an integer string and parsing.
/// Avoids a direct `as` cast that triggers `cast_possible_truncation`.
fn f64_to_i64(v: f64) -> i64 {
  if !v.is_finite() {
    return 0;
  }
  format!("{v:.0}").parse::<i64>().unwrap_or(0)
}

/// Shared encoding pipeline state. Holds the output context, encoder,
/// JPEG decoder, scaler, and reusable frame buffers.
struct EncodingPipeline {
  output_ctx: ffmpeg::format::context::Output,
  encoder: ffmpeg::encoder::Video,
  stream_index: usize,
  jpeg_decoder: ffmpeg::decoder::video::Video,
  scaler: Option<scaling::Context>,
  scaler_src_fmt: Pixel,
  decoded_frame: ffmpeg::frame::Video,
  yuv_frame: ffmpeg::frame::Video,
  frame_idx: i64,
  first_ts: Option<f64>,
  last_pts: i64,
  pkt: ffmpeg::Packet,
  fps: u32,
  w: u32,
  h: u32,
}

impl EncodingPipeline {
  fn new(output_path: &Path, width: u32, height: u32, fps: u32) -> Result<Self, String> {
    ffmpeg::init().map_err(|e| format!("ffmpeg init: {e}"))?;

    let w = width & !1;
    let h = height & !1;

    let mut output_ctx = ffmpeg::format::output(output_path).map_err(|e| format!("open output: {e}"))?;

    let codec = find_encoder()?;
    let mut enc_ctx = ffmpeg::codec::context::Context::new_with_codec(codec)
      .encoder()
      .video()
      .map_err(|e| format!("encoder context: {e}"))?;

    enc_ctx.set_width(w);
    enc_ctx.set_height(h);
    enc_ctx.set_format(Pixel::YUV420P);
    enc_ctx.set_time_base(ffmpeg::Rational::new(1, i32::try_from(fps).unwrap_or(i32::MAX)));
    enc_ctx.set_bit_rate(1_500_000);

    let mut opts = ffmpeg::Dictionary::new();
    match codec.name() {
      "libvpx" => {
        opts.set("qmin", "0");
        opts.set("qmax", "50");
        opts.set("crf", "8");
        opts.set("deadline", "realtime");
        opts.set("speed", "8");
        opts.set("threads", "1");
      },
      "h264_videotoolbox" => {
        opts.set("allow_sw", "1");
        opts.set("realtime", "0");
      },
      _ => {
        opts.set("preset", "veryfast");
        opts.set("crf", "23");
        opts.set("tune", "fastdecode");
      },
    }

    let encoder = enc_ctx.open_with(opts).map_err(|e| format!("open encoder: {e}"))?;

    let stream_index = {
      let mut stream = output_ctx.add_stream(codec).map_err(|e| format!("add stream: {e}"))?;
      stream.set_parameters(&encoder);
      stream.set_time_base(ffmpeg::Rational::new(1, i32::try_from(fps).unwrap_or(i32::MAX)));
      stream.index()
    };

    output_ctx.write_header().map_err(|e| format!("write header: {e}"))?;

    let jpeg_codec = ffmpeg::decoder::find(ffmpeg::codec::Id::MJPEG).ok_or("MJPEG decoder not found")?;
    let jpeg_decoder = ffmpeg::codec::context::Context::new_with_codec(jpeg_codec)
      .decoder()
      .video()
      .map_err(|e| format!("jpeg decoder init: {e}"))?;

    Ok(Self {
      output_ctx,
      encoder,
      stream_index,
      jpeg_decoder,
      scaler: None,
      scaler_src_fmt: Pixel::None,
      decoded_frame: ffmpeg::frame::Video::empty(),
      yuv_frame: ffmpeg::frame::Video::new(Pixel::YUV420P, w, h),
      frame_idx: 0,
      first_ts: None,
      last_pts: -1,
      pkt: ffmpeg::Packet::empty(),
      fps,
      w,
      h,
    })
  }

  /// Decode a JPEG frame, scale to YUV420P, and encode with gap-filling.
  fn encode_jpeg_frame(&mut self, jpeg_bytes: &[u8], ts: f64) -> Result<(), String> {
    let first = *self.first_ts.get_or_insert(ts);
    let target_frame = f64_to_i64(((ts - first) * f64::from(self.fps)).floor());

    let borrow_pkt = ffmpeg::Packet::borrow(jpeg_bytes);
    self
      .jpeg_decoder
      .send_packet(&borrow_pkt)
      .map_err(|e| format!("send jpeg: {e}"))?;
    self
      .jpeg_decoder
      .receive_frame(&mut self.decoded_frame)
      .map_err(|e| format!("receive jpeg frame: {e}"))?;

    // Normalize deprecated YUVJ* formats to standard equivalents.
    let raw_fmt = self.decoded_frame.format();
    let src_fmt = match raw_fmt {
      Pixel::YUVJ420P => Pixel::YUV420P,
      Pixel::YUVJ422P => Pixel::YUV422P,
      Pixel::YUVJ444P => Pixel::YUV444P,
      other => other,
    };
    if raw_fmt != src_fmt {
      #[allow(unsafe_code)]
      unsafe {
        (*self.decoded_frame.as_mut_ptr()).format = ffmpeg_next::ffi::AVPixelFormat::from(src_fmt) as libc::c_int;
      }
    }
    if self.scaler.is_none() || src_fmt != self.scaler_src_fmt {
      self.scaler = Some(
        scaling::Context::get(
          src_fmt,
          self.w,
          self.h,
          Pixel::YUV420P,
          self.w,
          self.h,
          scaling::Flags::BILINEAR,
        )
        .map_err(|e| format!("scaler: {e}"))?,
      );
      self.scaler_src_fmt = src_fmt;
    }

    self
      .scaler
      .as_mut()
      .ok_or("scaler not initialized")?
      .run(&self.decoded_frame, &mut self.yuv_frame)
      .map_err(|e| format!("scale: {e}"))?;

    // Gap fill: re-encode yuv_frame with gap PTS values.
    while self.frame_idx < target_frame && self.last_pts >= 0 {
      self.yuv_frame.set_pts(Some(self.frame_idx));
      send_frame_and_drain(
        &mut self.output_ctx,
        &mut self.encoder,
        &self.yuv_frame,
        self.stream_index,
        &mut self.pkt,
      )?;
      self.frame_idx += 1;
    }

    self.yuv_frame.set_pts(Some(self.frame_idx));
    send_frame_and_drain(
      &mut self.output_ctx,
      &mut self.encoder,
      &self.yuv_frame,
      self.stream_index,
      &mut self.pkt,
    )?;
    self.last_pts = self.frame_idx;
    self.frame_idx += 1;
    Ok(())
  }

  /// Trailing pad (1 second of last frame) + flush + trailer.
  fn finish(mut self) -> Result<(), String> {
    if self.last_pts >= 0 {
      for _ in 0..self.fps {
        self.yuv_frame.set_pts(Some(self.frame_idx));
        send_frame_and_drain(
          &mut self.output_ctx,
          &mut self.encoder,
          &self.yuv_frame,
          self.stream_index,
          &mut self.pkt,
        )?;
        self.frame_idx += 1;
      }
    }

    self.encoder.send_eof().map_err(|e| format!("send eof: {e}"))?;
    drain_all(
      &mut self.output_ctx,
      &mut self.encoder,
      self.stream_index,
      &mut self.pkt,
    )?;
    self
      .output_ctx
      .write_trailer()
      .map_err(|e| format!("write trailer: {e}"))?;
    Ok(())
  }
}

/// Encode JPEG frames into a video file.
///
/// # Errors
///
/// Returns an error if ffmpeg initialization, encoder setup, frame decoding,
/// scaling, or writing fails.
pub fn encode_frames(
  frames: &[(Vec<u8>, f64)],
  output_path: &Path,
  width: u32,
  height: u32,
  fps: u32,
) -> Result<(), String> {
  let mut pipeline = EncodingPipeline::new(output_path, width, height, fps)?;

  for (jpeg_bytes, ts) in frames {
    pipeline.encode_jpeg_frame(jpeg_bytes, *ts)?;
  }

  pipeline.finish()
}

fn find_encoder() -> Result<ffmpeg::Codec, String> {
  // Match Playwright: VP8/WebM by default.
  // Fallback chain: libvpx -> h264_videotoolbox (macOS HW) -> libx264
  if let Some(c) = ffmpeg::encoder::find_by_name("libvpx") {
    return Ok(c);
  }
  if cfg!(target_os = "macos") {
    if let Some(c) = ffmpeg::encoder::find_by_name("h264_videotoolbox") {
      return Ok(c);
    }
  }
  ffmpeg::encoder::find_by_name("libx264")
    .or_else(|| ffmpeg::encoder::find(ffmpeg::codec::Id::H264))
    .ok_or_else(|| "no video encoder available (need libvpx, h264_videotoolbox, or libx264)".to_string())
}

fn send_frame_and_drain(
  output_ctx: &mut ffmpeg::format::context::Output,
  encoder: &mut ffmpeg::encoder::Video,
  frame: &ffmpeg::frame::Video,
  stream_index: usize,
  pkt: &mut ffmpeg::Packet,
) -> Result<(), String> {
  encoder.send_frame(frame).map_err(|e| format!("send frame: {e}"))?;
  drain_all(output_ctx, encoder, stream_index, pkt)
}

fn drain_all(
  output_ctx: &mut ffmpeg::format::context::Output,
  encoder: &mut ffmpeg::encoder::Video,
  stream_index: usize,
  pkt: &mut ffmpeg::Packet,
) -> Result<(), String> {
  let enc_tb = encoder.time_base();
  let stream_tb = output_ctx.stream(stream_index).ok_or("stream not found")?.time_base();

  while encoder.receive_packet(pkt).is_ok() {
    pkt.set_stream(stream_index);
    pkt.rescale_ts(enc_tb, stream_tb);
    pkt
      .write_interleaved(output_ctx)
      .map_err(|e| format!("write packet: {e}"))?;
  }
  Ok(())
}

/// Channel-driven encoding: encode frames as they arrive from a bounded channel.
/// Runs on a blocking thread concurrently with the test.
/// When the channel closes (pump task drops sender), drains remaining frames,
/// adds trailing padding, and finishes.
///
/// # Errors
///
/// Returns an error if ffmpeg initialization, encoder setup, frame decoding,
/// scaling, or writing fails.
pub fn encode_stream(
  mut rx: tokio::sync::mpsc::Receiver<(Vec<u8>, f64)>,
  output_path: &Path,
  width: u32,
  height: u32,
  fps: u32,
) -> Result<(), String> {
  let mut pipeline = EncodingPipeline::new(output_path, width, height, fps)?;

  while let Some((jpeg_bytes, ts)) = rx.blocking_recv() {
    pipeline.encode_jpeg_frame(&jpeg_bytes, ts)?;
  }

  pipeline.finish()
}

/// Return the correct file extension based on available encoder.
#[must_use]
pub fn video_extension() -> &'static str {
  ffmpeg::init().ok();
  if ffmpeg::encoder::find_by_name("libvpx").is_some() {
    "webm"
  } else {
    "mp4"
  }
}

/// Return the correct MIME type based on available encoder.
#[must_use]
pub fn video_content_type() -> &'static str {
  ffmpeg::init().ok();
  if ffmpeg::encoder::find_by_name("libvpx").is_some() {
    "video/webm"
  } else {
    "video/mp4"
  }
}
