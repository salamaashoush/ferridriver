//! Shared helpers for the `mcp_smoke` integration test binary.
//!
//! Browser-behaviour coverage belongs in `tests/e2e/*.test.ts` (run by
//! `ferridriver test`); only MCP-wire-specific groups live here. When
//! adding one, create a file named by the behaviour it exercises (not
//! by session-local labels like phase / task / rule numbers) and add
//! its `pub mod` line below — `tests/mcp_smoke.rs` picks the test
//! functions up via the module path.

pub mod bdd;
pub mod client;
pub mod error_convention;
pub mod evaluate;
pub mod extension_context;
pub mod extension_package;
pub mod extension_tools;
pub mod instances;
pub mod mcp_features;
pub mod multi_page;
pub mod nav;
pub mod observation;
pub mod response_contract;
pub mod script_sessions;
pub mod session_bind;
pub mod trace;
pub mod tracing_har;

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::thread;

/// Spawn a throwaway localhost HTTP server that serves a minimal HTML page
/// for every request. Returns the bound port. `http://localhost` is a
/// secure, non-opaque origin where `localStorage` is available (unlike
/// `data:` / `about:blank`), and gives the HTTP client a real peer address.
///
/// Paths starting with `/iframe` serve a page embedding a same-origin
/// `<iframe src="/inner">`; every other path serves the flat test page.
///
/// Each connection is served on its own thread. Browsers (WebKit in
/// particular) open speculative preconnections that carry no request for
/// up to ~60s; a single-threaded accept loop blocks reading that idle
/// socket while the real request starves — observed as a full 30s MCP
/// timeout on `goto`. `Connection: close` keeps clients from parking
/// keep-alive reuse on a socket this server has already dropped.
pub fn spawn_html_server() -> u16 {
  let listener = TcpListener::bind("127.0.0.1:0").expect("bind html server");
  let port = listener.local_addr().expect("addr").port();
  thread::spawn(move || {
    while let Ok((stream, _)) = listener.accept() {
      thread::spawn(move || serve_connection(stream));
    }
  });
  port
}

fn serve_connection(mut stream: std::net::TcpStream) {
  let mut reader = BufReader::new(match stream.try_clone() {
    Ok(s) => s,
    Err(_) => return,
  });
  let mut request_line = String::new();
  if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
    return;
  }
  loop {
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
      return;
    }
    if line == "\r\n" || line == "\n" {
      break;
    }
  }
  let path = request_line.split_whitespace().nth(1).unwrap_or("/");
  let body = if path.starts_with("/iframe") {
    "<!doctype html><body>outer<iframe src=\"/inner\"></iframe></body>".to_string()
  } else {
    // A <title> and a Set-Cookie let the HAR validator assert
    // log.pages[].title and response.cookies capture.
    "<!doctype html><title>HAR Fixture Title</title><body>backend-test</body>".to_string()
  };
  let resp = format!(
    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nSet-Cookie: harcookie=harvalue; Path=/\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
    body.len(),
    body
  );
  let _ = stream.write_all(resp.as_bytes());
}
