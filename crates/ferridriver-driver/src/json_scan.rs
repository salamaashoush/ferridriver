//! Zero-allocation JSON field scanner — port of Bun's jsonField/jsonId/jsonString.
//!
//! Extracts field values from CDP JSON without full parse. Operates on byte
//! slices, returns sub-slices. No allocations, no serde.
//!
//! Only used for hot-path fields (id, result, error, method, sessionId) where
//! avoiding a full `serde_json` parse saves allocations per CDP message.

/// Scan a JSON value starting at `vstart`. Depth-counts braces/brackets,
/// tracks quoted strings. Returns the slice up to the end of the value
/// (depth-0 comma or enclosing close).
fn scan_value(data: &[u8], vstart: usize) -> &[u8] {
  let mut depth: i32 = 0;
  let mut in_str = false;
  let mut esc = false;
  let mut i = vstart;
  while i < data.len() {
    let c = data[i];
    if esc {
      esc = false;
      i += 1;
      continue;
    }
    if c == b'\\' {
      esc = true;
      i += 1;
      continue;
    }
    if c == b'"' {
      in_str = !in_str;
      i += 1;
      continue;
    }
    if in_str {
      i += 1;
      continue;
    }
    if c == b'{' || c == b'[' {
      depth += 1;
    } else if c == b'}' || c == b']' {
      if depth == 0 {
        return &data[vstart..i];
      }
      depth -= 1;
    } else if c == b',' && depth == 0 {
      return &data[vstart..i];
    }
    i += 1;
  }
  &data[vstart..data.len()]
}

/// Find the value for a top-level key in a JSON object. Matches at depth 1
/// only (inside the outermost {}), so nested keys with the same name don't
/// confuse it. Returns the raw value bytes (no quotes stripped for strings).
#[must_use]
pub fn json_field<'a>(data: &'a [u8], key: &[u8]) -> &'a [u8] {
  let klen = key.len();
  let mut depth: i32 = 0;
  let mut in_str = false;
  let mut esc = false;
  let mut i: usize = 0;
  while i + klen + 3 < data.len() {
    let c = data[i];
    if esc {
      esc = false;
      i += 1;
      continue;
    }
    if c == b'\\' {
      esc = true;
      i += 1;
      continue;
    }
    if c == b'"' {
      if !in_str
        && depth == 1
        && &data[i + 1..i + 1 + klen] == key
        && data[i + klen + 1] == b'"'
        && data[i + klen + 2] == b':'
      {
        return scan_value(data, i + klen + 3);
      }
      in_str = !in_str;
      i += 1;
      continue;
    }
    if in_str {
      i += 1;
      continue;
    }
    if c == b'{' || c == b'[' {
      depth += 1;
    } else if c == b'}' || c == b']' {
      depth -= 1;
    }
    i += 1;
  }
  &[]
}

/// Extract numeric "id" field from CDP JSON. Returns 0 if not found.
#[must_use]
pub fn json_id(data: &[u8]) -> u64 {
  let slice = json_field(data, b"id");
  if slice.is_empty() {
    return 0;
  }
  let mut n: u64 = 0;
  for &c in slice {
    if !c.is_ascii_digit() {
      break;
    }
    n = n * 10 + u64::from(c - b'0');
  }
  n
}

/// Strip surrounding quotes from a JSON string value. Returns empty if not
/// a quoted string.
#[must_use]
pub fn json_string(field: &[u8]) -> &[u8] {
  let f = field
    .iter()
    .position(|&b| b != b' ' && b != b'\t')
    .map_or(field, |s| &field[s..]);
  let f = f.iter().rposition(|&b| b != b' ' && b != b'\t').map_or(f, |e| &f[..=e]);
  if f.len() >= 2 && f[0] == b'"' && f[f.len() - 1] == b'"' {
    &f[1..f.len() - 1]
  } else {
    &[]
  }
}

/// Extract the "message" string from an error object.
#[must_use]
pub fn error_message(error_field: &[u8]) -> &[u8] {
  json_string(json_field(error_field, b"message"))
}

/// Top-level fields of a CDP JSON message extracted in a single pass.
#[derive(Default)]
pub struct CdpEnvelope<'a> {
  pub id: u64,
  pub method: &'a [u8],
  pub session_id: &'a [u8],
  pub result: &'a [u8],
  pub error: &'a [u8],
  pub params: &'a [u8],
}

/// Scan a CDP message once at depth 1 and extract its top-level envelope
/// fields (`id`, `method`, `sessionId`, `result`, `error`, `params`),
/// skipping over each field's value in one linear sweep.
#[must_use]
pub fn scan_cdp_envelope(data: &[u8]) -> CdpEnvelope<'_> {
  let mut env = CdpEnvelope::default();
  let mut depth: i32 = 0;
  let mut in_str = false;
  let mut esc = false;
  let mut pos: usize = 0;
  let len = data.len();
  while pos < len {
    let byte = data[pos];
    if esc {
      esc = false;
      pos += 1;
      continue;
    }
    if byte == b'\\' {
      esc = true;
      pos += 1;
      continue;
    }
    if byte == b'"' {
      if !in_str && depth == 1 {
        let key_start = pos + 1;
        let mut key_end = key_start;
        while key_end < len && data[key_end] != b'"' {
          if data[key_end] == b'\\' {
            key_end += 2;
          } else {
            key_end += 1;
          }
        }
        if key_end < len {
          let key = &data[key_start..key_end];
          let mut colon_pos = key_end + 1;
          while colon_pos < len && (data[colon_pos] == b' ' || data[colon_pos] == b'\t') {
            colon_pos += 1;
          }
          if colon_pos < len && data[colon_pos] == b':' {
            let vstart = colon_pos + 1;
            let val = scan_value(data, vstart);
            match key {
              b"id" => {
                let mut parsed_id: u64 = 0;
                for &digit in val {
                  if digit == b' ' || digit == b'\t' {
                    continue;
                  }
                  if !digit.is_ascii_digit() {
                    break;
                  }
                  parsed_id = parsed_id * 10 + u64::from(digit - b'0');
                }
                env.id = parsed_id;
              },
              b"method" => env.method = json_string(val),
              b"sessionId" => env.session_id = json_string(val),
              b"result" => env.result = val,
              b"error" => env.error = val,
              b"params" => env.params = val,
              _ => {},
            }
            pos = vstart + val.len();
            continue;
          }
          pos = key_end + 1;
          continue;
        }
      }
      in_str = !in_str;
      pos += 1;
      continue;
    }
    if in_str {
      pos += 1;
      continue;
    }
    if byte == b'{' || byte == b'[' {
      depth += 1;
    } else if byte == b'}' || byte == b']' {
      depth -= 1;
    }
    pos += 1;
  }
  env
}
