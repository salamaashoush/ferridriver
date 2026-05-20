# ferridriver-webkit-wire

Wire-level binary IPC protocol shared between the ferridriver WebKit host
binary and the parent process. Two hosts implement this protocol:

- **macOS**: `crates/ferridriver/src/backend/webkit/host.m` (Obj-C, drives
  `WKWebView`).
- **Linux**: `crates/ferridriver-webkit-host/src/main.rs` (Rust, drives
  `webkit6::WebView` on a GTK4 main loop).

Both hosts and the parent client (`crates/ferridriver/src/backend/webkit/
ipc.rs`) depend on this crate so the wire format stays byte-identical
across platforms.

## Frame format

```
Frame = u32 len_le, u32 req_id_le, u8 op, payload[len]
String = u32 len_le, UTF-8 bytes
```

All multi-byte integers are little-endian.

See [`Op`] for parent → host request codes and [`Rep`] for host → parent
reply / event codes.
