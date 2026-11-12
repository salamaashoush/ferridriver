//! A minimal EBML/Matroska writer, Playwright's `ebml.ts`: each MJPEG
//! frame goes to ffmpeg (`-f matroska -i pipe:0`) in a cluster of its own,
//! stamped with its time, so ffmpeg reads frame timing from the stream and
//! the recorder never repeats a frame to fake a constant rate. Only what a
//! single live MJPEG track needs is written.
//!
//! <https://www.matroska.org/technical/elements.html>,
//! <https://datatracker.ietf.org/doc/html/rfc8794> (EBML).

// Element IDs are written verbatim: their leading byte already encodes
// their length.
const EBML: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3];
const EBML_VERSION: &[u8] = &[0x42, 0x86];
const EBML_READ_VERSION: &[u8] = &[0x42, 0xF7];
const EBML_MAX_ID_LENGTH: &[u8] = &[0x42, 0xF2];
const EBML_MAX_SIZE_LENGTH: &[u8] = &[0x42, 0xF3];
const DOC_TYPE: &[u8] = &[0x42, 0x82];
const DOC_TYPE_VERSION: &[u8] = &[0x42, 0x87];
const DOC_TYPE_READ_VERSION: &[u8] = &[0x42, 0x85];
const SEGMENT: &[u8] = &[0x18, 0x53, 0x80, 0x67];
const INFO: &[u8] = &[0x15, 0x49, 0xA9, 0x66];
const TIMESTAMP_SCALE: &[u8] = &[0x2A, 0xD7, 0xB1];
const MUXING_APP: &[u8] = &[0x4D, 0x80];
const WRITING_APP: &[u8] = &[0x57, 0x41];
const TRACKS: &[u8] = &[0x16, 0x54, 0xAE, 0x6B];
const TRACK_ENTRY: &[u8] = &[0xAE];
const TRACK_NUMBER: &[u8] = &[0xD7];
const TRACK_UID: &[u8] = &[0x73, 0xC5];
const TRACK_TYPE: &[u8] = &[0x83];
const FLAG_LACING: &[u8] = &[0x9C];
const CODEC_ID: &[u8] = &[0x86];
const VIDEO: &[u8] = &[0xE0];
const PIXEL_WIDTH: &[u8] = &[0xB0];
const PIXEL_HEIGHT: &[u8] = &[0xBA];
const CLUSTER: &[u8] = &[0x1F, 0x43, 0xB6, 0x75];
const TIMESTAMP: &[u8] = &[0xE7];
const SIMPLE_BLOCK: &[u8] = &[0xA3];

/// "Unknown size", for a Segment written as a stream: an eight-byte vint
/// with every data bit set.
const UNKNOWN_SIZE: &[u8] = &[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

/// An EBML variable-length size: leading bits select the byte length, the
/// rest is the big-endian value. A length whose data bits would all be
/// set is reserved for "unknown", so such a value takes one more byte.
fn vint(value: u64) -> Vec<u8> {
  let mut length = 1u32;
  while length < 8 && u128::from(value) >= (1u128 << (7 * length)) - 1 {
    length += 1;
  }
  let mut bytes = value.to_be_bytes()[8 - length as usize..].to_vec();
  bytes[0] |= 1 << (8 - length);
  bytes
}

/// A non-negative integer in as few big-endian bytes as it takes.
fn uint(value: u64) -> Vec<u8> {
  let bytes = value.to_be_bytes();
  let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(7);
  bytes[first..].to_vec()
}

fn element(id: &[u8], payload: &[u8]) -> Vec<u8> {
  let size = vint(payload.len() as u64);
  let mut out = Vec::with_capacity(id.len() + size.len() + payload.len());
  out.extend_from_slice(id);
  out.extend_from_slice(&size);
  out.extend_from_slice(payload);
  out
}

/// The stream's opening: the EBML head, a Segment of unknown size, Info
/// with a millisecond timestamp scale, and one MJPEG video track.
pub(super) fn header() -> Vec<u8> {
  let ebml = element(
    EBML,
    &[
      element(EBML_VERSION, &uint(1)),
      element(EBML_READ_VERSION, &uint(1)),
      element(EBML_MAX_ID_LENGTH, &uint(4)),
      element(EBML_MAX_SIZE_LENGTH, &uint(8)),
      element(DOC_TYPE, b"matroska"),
      element(DOC_TYPE_VERSION, &uint(4)),
      element(DOC_TYPE_READ_VERSION, &uint(2)),
    ]
    .concat(),
  );
  let info = element(
    INFO,
    &[
      // Nanoseconds per tick: timestamps are in milliseconds.
      element(TIMESTAMP_SCALE, &uint(1_000_000)),
      element(MUXING_APP, b"ferridriver"),
      element(WRITING_APP, b"ferridriver"),
    ]
    .concat(),
  );
  let track = element(
    TRACK_ENTRY,
    &[
      element(TRACK_NUMBER, &uint(1)),
      element(TRACK_UID, &uint(1)),
      element(TRACK_TYPE, &uint(1)),
      element(FLAG_LACING, &uint(0)),
      element(CODEC_ID, b"V_MJPEG"),
      // The size is mandatory but not known yet. A larger placeholder can
      // make ffmpeg take a short progressive JPEG for an interlaced field,
      // so it is 1x1 and the decoder reads each frame's own.
      element(
        VIDEO,
        &[element(PIXEL_WIDTH, &uint(1)), element(PIXEL_HEIGHT, &uint(1))].concat(),
      ),
    ]
    .concat(),
  );
  [ebml, SEGMENT.to_vec(), UNKNOWN_SIZE.to_vec(), info, element(TRACKS, &track)].concat()
}

/// What precedes one frame: a Cluster at `timestamp_ms` holding a single
/// keyframe `SimpleBlock`. The caller writes the frame itself next, so a
/// large JPEG is never copied. A frame per Cluster keeps the block's
/// relative timecode at 0, inside its int16 range however far apart
/// frames arrive.
pub(super) fn cluster_header(timestamp_ms: u64, frame_length: usize) -> Vec<u8> {
  let frame_length = frame_length as u64;
  // Track number (one-byte vint), relative timecode (int16), flags.
  let block_head: [u8; 4] = [0x81, 0x00, 0x00, 0x80];
  let mut simple_block = SIMPLE_BLOCK.to_vec();
  simple_block.extend(vint(4 + frame_length));
  simple_block.extend_from_slice(&block_head);
  let timestamp = element(TIMESTAMP, &uint(timestamp_ms));
  let payload = timestamp.len() as u64 + simple_block.len() as u64 + frame_length;
  [CLUSTER.to_vec(), vint(payload), timestamp, simple_block].concat()
}

#[cfg(test)]
mod tests {
  use super::{cluster_header, header, uint, vint};

  #[test]
  fn sizes_take_the_shortest_vint_that_is_not_the_unknown_marker() {
    assert_eq!(vint(0), [0x80]);
    assert_eq!(vint(126), [0xFE]);
    // 127 in one byte would be all data bits set, which means "unknown".
    assert_eq!(vint(127), [0x40, 0x7F]);
    assert_eq!(vint(16_382), [0x7F, 0xFE]);
    assert_eq!(vint(16_383), [0x20, 0x3F, 0xFF]);
  }

  #[test]
  fn integers_drop_their_leading_zero_bytes() {
    assert_eq!(uint(0), [0x00]);
    assert_eq!(uint(1), [0x01]);
    assert_eq!(uint(1_000_000), [0x0F, 0x42, 0x40]);
  }

  #[test]
  fn a_cluster_counts_the_frame_that_follows_it() {
    let head = cluster_header(1_500, 1_000);
    // Cluster id, then its size: timestamp element (E7 82 05 DC) is 4
    // bytes, the SimpleBlock head A3 + size(1004 -> 2 bytes) + 4 is 7,
    // and the frame 1000.
    assert_eq!(&head[..4], &[0x1F, 0x43, 0xB6, 0x75]);
    assert_eq!(&head[4..6], &vint(4 + 7 + 1_000)[..]);
    assert_eq!(&head[6..10], &[0xE7, 0x82, 0x05, 0xDC]);
    assert_eq!(&head[10..], &[0xA3, 0x43, 0xEC, 0x81, 0x00, 0x00, 0x80]);
  }

  #[test]
  fn the_header_opens_a_matroska_stream_with_one_mjpeg_track() {
    let head = header();
    assert_eq!(&head[..4], &[0x1A, 0x45, 0xDF, 0xA3]);
    let find = |needle: &[u8]| head.windows(needle.len()).any(|window| window == needle);
    assert!(find(b"matroska"));
    assert!(find(b"V_MJPEG"));
    assert!(find(&[0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]));
  }
}
