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
        if esc { esc = false; i += 1; continue; }
        if c == b'\\' { esc = true; i += 1; continue; }
        if c == b'"' { in_str = !in_str; i += 1; continue; }
        if in_str { i += 1; continue; }
        if c == b'{' || c == b'[' { depth += 1; }
        else if c == b'}' || c == b']' {
            if depth == 0 { return &data[vstart..i]; }
            depth -= 1;
        }
        else if c == b',' && depth == 0 { return &data[vstart..i]; }
        i += 1;
    }
    &data[vstart..data.len()]
}

/// Find the value for a top-level key in a JSON object. Matches at depth 1
/// only (inside the outermost {}), so nested keys with the same name don't
/// confuse it. Returns the raw value bytes (no quotes stripped for strings).
pub fn json_field<'a>(data: &'a [u8], key: &[u8]) -> &'a [u8] {
    let klen = key.len();
    let mut depth: i32 = 0;
    let mut in_str = false;
    let mut esc = false;
    let mut i: usize = 0;
    while i + klen + 3 < data.len() {
        let c = data[i];
        if esc { esc = false; i += 1; continue; }
        if c == b'\\' { esc = true; i += 1; continue; }
        if c == b'"' {
            if !in_str && depth == 1
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
        if in_str { i += 1; continue; }
        if c == b'{' || c == b'[' { depth += 1; }
        else if c == b'}' || c == b']' { depth -= 1; }
        i += 1;
    }
    &[]
}

/// Extract numeric "id" field from CDP JSON. Returns 0 if not found.
pub fn json_id(data: &[u8]) -> u64 {
    let slice = json_field(data, b"id");
    if slice.is_empty() { return 0; }
    let mut n: u64 = 0;
    for &c in slice {
        if !c.is_ascii_digit() { break; }
        n = n * 10 + u64::from(c - b'0');
    }
    n
}

/// Strip surrounding quotes from a JSON string value. Returns empty if not
/// a quoted string.
pub fn json_string(field: &[u8]) -> &[u8] {
    let f = field.iter().position(|&b| b != b' ' && b != b'\t')
        .map_or(field, |s| &field[s..]);
    let f = f.iter().rposition(|&b| b != b' ' && b != b'\t')
        .map_or(f, |e| &f[..=e]);
    if f.len() >= 2 && f[0] == b'"' && f[f.len() - 1] == b'"' {
        &f[1..f.len() - 1]
    } else {
        &[]
    }
}


/// Extract the "message" string from an error object.
pub fn error_message(error_field: &[u8]) -> &[u8] {
    json_string(json_field(error_field, b"message"))
}
