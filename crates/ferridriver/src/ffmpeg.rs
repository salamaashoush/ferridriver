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

pub fn encode_frames(
  frames: &[(Vec<u8>, f64)],
  output_path: &Path,
  width: u32,
  height: u32,
  fps: u32,
) -> Result<(), String> {
  ffmpeg::init().map_err(|e| format!("ffmpeg init: {e}"))?;

  let w = (width & !1) as u32;
  let h = (height & !1) as u32;

  // ── Output container ──
  let mut output_ctx =
    ffmpeg::format::output(output_path).map_err(|e| format!("open output: {e}"))?;

  let codec = find_encoder();
  let mut enc_ctx = ffmpeg::codec::context::Context::new_with_codec(codec)
    .encoder()
    .video()
    .map_err(|e| format!("encoder context: {e}"))?;

  enc_ctx.set_width(w);
  enc_ctx.set_height(h);
  enc_ctx.set_format(Pixel::YUV420P);
  enc_ctx.set_time_base(ffmpeg::Rational::new(1, fps as i32));
  enc_ctx.set_bit_rate(1_500_000);

  let mut opts = ffmpeg::Dictionary::new();
  match codec.name() {
    "libvpx" => {
      // Match Playwright's exact VP8 settings.
      opts.set("qmin", "0");
      opts.set("qmax", "50");
      opts.set("crf", "8");
      opts.set("deadline", "realtime");
      opts.set("speed", "8");
      opts.set("threads", "1");
    }
    "h264_videotoolbox" => {
      opts.set("allow_sw", "1");
      opts.set("realtime", "0");
    }
    _ => {
      // libx264 fallback
      opts.set("preset", "veryfast");
      opts.set("crf", "23");
      opts.set("tune", "fastdecode");
    }
  }

  let mut encoder = enc_ctx
    .open_with(opts)
    .map_err(|e| format!("open encoder: {e}"))?;

  let stream_index = {
    let mut stream = output_ctx
      .add_stream(codec)
      .map_err(|e| format!("add stream: {e}"))?;
    stream.set_parameters(&encoder);
    stream.set_time_base(ffmpeg::Rational::new(1, fps as i32));
    stream.index()
  };

  output_ctx
    .write_header()
    .map_err(|e| format!("write header: {e}"))?;

  // ── Reused MJPEG decoder -- created ONCE, not per frame ──
  let jpeg_codec =
    ffmpeg::decoder::find(ffmpeg::codec::Id::MJPEG).ok_or("MJPEG decoder not found")?;
  let mut jpeg_decoder = ffmpeg::codec::context::Context::new_with_codec(jpeg_codec)
    .decoder()
    .video()
    .map_err(|e| format!("jpeg decoder init: {e}"))?;

  // ── Lazy scaler -- recreated only if source pixel format changes ──
  let mut scaler: Option<scaling::Context> = None;
  let mut scaler_src_fmt = Pixel::None;

  // ── Reusable frame allocations ──
  let mut decoded_frame = ffmpeg::frame::Video::empty();
  let mut yuv_frame = ffmpeg::frame::Video::new(Pixel::YUV420P, w, h);

  let mut frame_idx: i64 = 0;
  let mut first_ts: Option<f64> = None;
  let mut last_pts: i64 = -1;

  let mut pkt = ffmpeg::Packet::empty();

  for (jpeg_bytes, ts) in frames {
    let first = *first_ts.get_or_insert(*ts);
    let target_frame = ((ts - first) * fps as f64).floor() as i64;

    // ── Decode JPEG using reused decoder ──
    let borrow_pkt = ffmpeg::Packet::borrow(jpeg_bytes);
    jpeg_decoder
      .send_packet(&borrow_pkt)
      .map_err(|e| format!("send jpeg: {e}"))?;
    jpeg_decoder
      .receive_frame(&mut decoded_frame)
      .map_err(|e| format!("receive jpeg frame: {e}"))?;

    // ── Lazy scaler: recreate only if format changed ──
    let src_fmt = decoded_frame.format();
    if scaler.is_none() || src_fmt != scaler_src_fmt {
      scaler = Some(
        scaling::Context::get(src_fmt, w, h, Pixel::YUV420P, w, h, scaling::Flags::BILINEAR)
          .map_err(|e| format!("scaler: {e}"))?,
      );
      scaler_src_fmt = src_fmt;
    }

    scaler
      .as_mut()
      .unwrap()
      .run(&decoded_frame, &mut yuv_frame)
      .map_err(|e| format!("scale: {e}"))?;

    // ── Gap fill: re-encode yuv_frame (still holds last data) with gap PTS values ──
    while frame_idx < target_frame && last_pts >= 0 {
      yuv_frame.set_pts(Some(frame_idx));
      send_frame_and_drain(&mut output_ctx, &mut encoder, &yuv_frame, stream_index, &mut pkt)?;
      frame_idx += 1;
    }

    yuv_frame.set_pts(Some(frame_idx));
    send_frame_and_drain(&mut output_ctx, &mut encoder, &yuv_frame, stream_index, &mut pkt)?;
    last_pts = frame_idx;
    frame_idx += 1;
  }

  // ── Trailing pad: 1 second of last frame ──
  if last_pts >= 0 {
    for _ in 0..fps {
      yuv_frame.set_pts(Some(frame_idx));
      send_frame_and_drain(&mut output_ctx, &mut encoder, &yuv_frame, stream_index, &mut pkt)?;
      frame_idx += 1;
    }
  }

  // ── Flush ──
  encoder.send_eof().map_err(|e| format!("send eof: {e}"))?;
  drain_all(&mut output_ctx, &mut encoder, stream_index, &mut pkt)?;

  output_ctx
    .write_trailer()
    .map_err(|e| format!("write trailer: {e}"))?;

  Ok(())
}

fn find_encoder() -> ffmpeg::Codec {
  // Match Playwright: VP8/WebM by default.
  // Fallback chain: libvpx -> h264_videotoolbox (macOS HW) -> libx264
  if let Some(c) = ffmpeg::encoder::find_by_name("libvpx") {
    return c;
  }
  if cfg!(target_os = "macos") {
    if let Some(c) = ffmpeg::encoder::find_by_name("h264_videotoolbox") {
      return c;
    }
  }
  ffmpeg::encoder::find_by_name("libx264")
    .or_else(|| ffmpeg::encoder::find(ffmpeg::codec::Id::H264))
    .expect("no video encoder available (need libvpx, h264_videotoolbox, or libx264)")
}

fn send_frame_and_drain(
  output_ctx: &mut ffmpeg::format::context::Output,
  encoder: &mut ffmpeg::encoder::video::Video,
  frame: &ffmpeg::frame::Video,
  stream_index: usize,
  pkt: &mut ffmpeg::Packet,
) -> Result<(), String> {
  encoder
    .send_frame(frame)
    .map_err(|e| format!("send frame: {e}"))?;
  drain_all(output_ctx, encoder, stream_index, pkt)
}

fn drain_all(
  output_ctx: &mut ffmpeg::format::context::Output,
  encoder: &mut ffmpeg::encoder::video::Video,
  stream_index: usize,
  pkt: &mut ffmpeg::Packet,
) -> Result<(), String> {
  let enc_tb = encoder.time_base();
  let stream_tb = output_ctx
    .stream(stream_index)
    .ok_or("stream not found")?
    .time_base();

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
pub fn encode_stream(
  mut rx: tokio::sync::mpsc::Receiver<(Vec<u8>, f64)>,
  output_path: &Path,
  width: u32,
  height: u32,
  fps: u32,
) -> Result<(), String> {
  ffmpeg::init().map_err(|e| format!("ffmpeg init: {e}"))?;

  let w = (width & !1) as u32;
  let h = (height & !1) as u32;

  let mut output_ctx =
    ffmpeg::format::output(output_path).map_err(|e| format!("open output: {e}"))?;

  let codec = find_encoder();
  let mut enc_ctx = ffmpeg::codec::context::Context::new_with_codec(codec)
    .encoder()
    .video()
    .map_err(|e| format!("encoder context: {e}"))?;

  enc_ctx.set_width(w);
  enc_ctx.set_height(h);
  enc_ctx.set_format(Pixel::YUV420P);
  enc_ctx.set_time_base(ffmpeg::Rational::new(1, fps as i32));
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
    }
    "h264_videotoolbox" => {
      opts.set("allow_sw", "1");
      opts.set("realtime", "0");
    }
    _ => {
      opts.set("preset", "veryfast");
      opts.set("crf", "23");
      opts.set("tune", "fastdecode");
    }
  }

  let mut encoder = enc_ctx
    .open_with(opts)
    .map_err(|e| format!("open encoder: {e}"))?;

  let stream_index = {
    let mut stream = output_ctx
      .add_stream(codec)
      .map_err(|e| format!("add stream: {e}"))?;
    stream.set_parameters(&encoder);
    stream.set_time_base(ffmpeg::Rational::new(1, fps as i32));
    stream.index()
  };

  output_ctx
    .write_header()
    .map_err(|e| format!("write header: {e}"))?;

  let jpeg_codec =
    ffmpeg::decoder::find(ffmpeg::codec::Id::MJPEG).ok_or("MJPEG decoder not found")?;
  let mut jpeg_decoder = ffmpeg::codec::context::Context::new_with_codec(jpeg_codec)
    .decoder()
    .video()
    .map_err(|e| format!("jpeg decoder init: {e}"))?;

  let mut scaler: Option<scaling::Context> = None;
  let mut scaler_src_fmt = Pixel::None;
  let mut decoded_frame = ffmpeg::frame::Video::empty();
  let mut yuv_frame = ffmpeg::frame::Video::new(Pixel::YUV420P, w, h);

  let mut frame_idx: i64 = 0;
  let mut first_ts: Option<f64> = None;
  let mut last_pts: i64 = -1;
  let mut pkt = ffmpeg::Packet::empty();

  // Process frames as they arrive from the channel.
  while let Some((jpeg_bytes, ts)) = rx.blocking_recv() {
    let first = *first_ts.get_or_insert(ts);
    let target_frame = ((ts - first) * fps as f64).floor() as i64;

    let borrow_pkt = ffmpeg::Packet::borrow(&jpeg_bytes);
    jpeg_decoder
      .send_packet(&borrow_pkt)
      .map_err(|e| format!("send jpeg: {e}"))?;
    jpeg_decoder
      .receive_frame(&mut decoded_frame)
      .map_err(|e| format!("receive jpeg frame: {e}"))?;

    let src_fmt = decoded_frame.format();
    if scaler.is_none() || src_fmt != scaler_src_fmt {
      scaler = Some(
        scaling::Context::get(src_fmt, w, h, Pixel::YUV420P, w, h, scaling::Flags::BILINEAR)
          .map_err(|e| format!("scaler: {e}"))?,
      );
      scaler_src_fmt = src_fmt;
    }

    scaler
      .as_mut()
      .unwrap()
      .run(&decoded_frame, &mut yuv_frame)
      .map_err(|e| format!("scale: {e}"))?;

    while frame_idx < target_frame && last_pts >= 0 {
      yuv_frame.set_pts(Some(frame_idx));
      send_frame_and_drain(&mut output_ctx, &mut encoder, &yuv_frame, stream_index, &mut pkt)?;
      frame_idx += 1;
    }

    yuv_frame.set_pts(Some(frame_idx));
    send_frame_and_drain(&mut output_ctx, &mut encoder, &yuv_frame, stream_index, &mut pkt)?;
    last_pts = frame_idx;
    frame_idx += 1;
  }

  // Trailing pad: 1 second of last frame.
  if last_pts >= 0 {
    for _ in 0..fps {
      yuv_frame.set_pts(Some(frame_idx));
      send_frame_and_drain(&mut output_ctx, &mut encoder, &yuv_frame, stream_index, &mut pkt)?;
      frame_idx += 1;
    }
  }

  encoder.send_eof().map_err(|e| format!("send eof: {e}"))?;
  drain_all(&mut output_ctx, &mut encoder, stream_index, &mut pkt)?;

  output_ctx
    .write_trailer()
    .map_err(|e| format!("write trailer: {e}"))?;

  Ok(())
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
