//! Shared helpers for the `backends` integration test binary.
//!
//! Split out of the sprawling `tests/backends.rs` so new test groups
//! can live in dedicated files without duplicating the MCP-client and
//! payload-extraction plumbing. When you add a new group of tests,
//! create a new file here named by the behaviour it exercises (not by
//! session-local labels like phase / task / rule numbers) and add its
//! `pub mod` line below — `tests/backends.rs` will pick up the test
//! functions via the module path.

pub mod accessible_description;
pub mod action_options;
pub mod api_response;
pub mod aria_snapshot;
pub mod bdd;
pub mod binding_surface;
pub mod browser_context_options;
pub mod browser_type;
pub mod cdp_session;
pub mod client;
pub mod clock;
pub mod console_message;
pub mod context_events;
pub mod dialog;
pub mod download;
pub mod evaluate;
pub mod expect;
pub mod extension_tools;
pub mod file_chooser;
pub mod getby_regex;
pub mod handle_surface;
pub mod locator_handler;
pub mod multi_page;
pub mod nav;
pub mod navigation_response;
pub mod network;
pub mod observation;
pub mod page_api;
pub mod route_web_socket;
pub mod script_emul_storage;
pub mod script_handles_local;
pub mod script_input;
pub mod script_locators;
pub mod script_sessions;
pub mod session_bind;
pub mod storage_state;
pub mod tracing_har;
pub mod video;
pub mod web_error;
pub mod web_storage;

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
    "<!doctype html><body>outer<iframe src=\"/inner\"></iframe></body>"
  } else {
    "<!doctype html><body>backend-test</body>"
  };
  let resp = format!(
    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
    body.len(),
    body
  );
  let _ = stream.write_all(resp.as_bytes());
}
